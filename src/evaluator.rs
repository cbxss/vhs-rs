//! The evaluator: executes parsed tape commands against a live PTY session,
//! renders artifacts, and assembles the run report.
//!
//! This is the integration point of the whole crate: session (PTY + avt),
//! renderer (fontdue rasterizer), and encoders (png/gif/txt/cast) meet here.
//! Waits and asserts are event-driven — check the rendered buffer, then await
//! the next PTY chunk with a deadline; no polling.

use crate::artifacts::ArtifactRegistry;
use crate::command::Command;
use crate::encode::{cast, gif, png, txt};
use crate::error::ExitKind;
use crate::keys;
use crate::render::{BarStyle, MarginFill, RenderOptions, Renderer};
use crate::report::{ArtifactKind, CommandStatus, ReportBuilder};
use crate::resolve::{Resolved, Scope, resolve_commands};
use crate::session::Session;
use crate::snapshot::{SessionEvent, SessionEventKind};
use crate::term::Term;
use crate::theme::{self, Rgb, Theme};
use crate::token::TokenType;
use crate::util::parse_duration;
use regex::Regex;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// Half-period of the synthesized cursor blink (xterm-ish cadence).
const BLINK_HALF_PERIOD: Duration = Duration::from_millis(530);
/// Idle gaps longer than this get no synthesized blink frames (degenerate
/// tapes would otherwise balloon the GIF).
const BLINK_MAX_GAP: Duration = Duration::from_secs(30);

/// Everything `Set` can configure, with VHS defaults.
struct Settings {
    shell: String,
    typing_speed: Duration,
    wait_timeout: Duration,
    wait_pattern: Regex,
    playback_speed: f64,
    max_fps: f64,
    cursor_blink: bool,
    loop_offset: Option<f64>,
    render: RenderOptions,
    theme: Theme,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shell: "bash".into(),
            typing_speed: Duration::from_millis(50),
            wait_timeout: Duration::from_secs(15),
            wait_pattern: Regex::new(">$").expect("default pattern"),
            playback_speed: 1.0,
            max_fps: 50.0,
            cursor_blink: true, // VHS default
            loop_offset: None,
            render: RenderOptions::default(),
            theme: theme::default_theme(),
        }
    }
}

/// Outcome of a single failed step, mapped to the exit taxonomy. The report
/// `reason` derives from `exit` ([`ExitKind::reason`]) unless `reason`
/// overrides it with a more specific taxonomy entry (e.g. `child_exited`).
struct StepFailure {
    exit: ExitKind,
    reason: Option<&'static str>,
    message: String,
    detail: Option<serde_json::Value>,
}

/// What ended a [`wait_for`]: the pattern matched, the deadline passed, or
/// the child exited (and the pattern can never match).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitOutcome {
    Matched,
    TimedOut,
    ChildExited,
}

/// How the run future ended: normally, or preempted by a signal.
enum RunOutcome {
    Finished(ExitKind),
    Interrupted(&'static str),
}

/// Entry point called by the CLI after parse+validate succeeded. `timeout`
/// is the whole-run wall-clock budget (`--timeout`); on SIGINT/SIGTERM the
/// report is still finalized and printed before exiting.
pub fn run(
    tape_name: &str,
    commands: &[Command],
    json: bool,
    quiet: bool,
    timeout: Option<Duration>,
) -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("vhs-rs: failed to start runtime: {e}");
            return ExitKind::Runtime as i32;
        }
    };

    let mut report = ReportBuilder::new(tape_name);
    let outcome = rt.block_on(async {
        let deadline = timeout.map(|t| tokio::time::Instant::now() + t);
        let fut = run_inner(tape_name, commands, &mut report, quiet, deadline);
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut sigterm), Ok(mut sigint)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) else {
            return RunOutcome::Finished(fut.await);
        };
        tokio::pin!(fut);
        tokio::select! {
            exit = &mut fut => RunOutcome::Finished(exit),
            _ = sigterm.recv() => RunOutcome::Interrupted("SIGTERM"),
            _ = sigint.recv() => RunOutcome::Interrupted("SIGINT"),
        }
    });
    let exit = match outcome {
        RunOutcome::Finished(exit) => exit,
        RunOutcome::Interrupted(sig) => {
            report.set_failure(
                None,
                "interrupted",
                format!("run interrupted by {sig}; partial report follows"),
            );
            ExitKind::Runtime
        }
    };
    let report = report.finish(exit);

    if json {
        println!("{}", report.to_json());
    } else if !quiet {
        if let Some(f) = &report.failure {
            eprintln!("vhs-rs: {} — {}", f.reason, f.message);
        }
        for a in &report.artifacts {
            eprintln!("  wrote {}", a.path);
        }
    }

    exit as i32
}

async fn run_inner(
    tape_name: &str,
    commands: &[Command],
    report: &mut ReportBuilder,
    quiet: bool,
    deadline: Option<tokio::time::Instant>,
) -> ExitKind {
    // ---- Typed resolution side table: everything the parser already
    // validated (durations, counts, Wait/Assert scopes and regexes) lifted
    // out of the command strings once, aligned by index.
    let resolved = match resolve_commands(commands) {
        Ok(r) => r,
        Err(msg) => {
            report.set_failure(None, ExitKind::Runtime.reason(), msg);
            return ExitKind::Runtime;
        }
    };

    // ---- Settings pre-pass: everything before the first action command.
    let mut settings = Settings::default();
    let mut spawn_env: Vec<(String, String)> = Vec::new();
    let mut outputs: Vec<(String, String)> = Vec::new(); // (ext, path)
    let mut started = false;

    for (cmd, res) in commands.iter().zip(resolved.iter()) {
        match cmd.command_type {
            TokenType::Set if !started => apply_setting(&mut settings, cmd, quiet),
            TokenType::Env if !started => {
                spawn_env.push((cmd.options.clone(), cmd.args.clone()));
            }
            TokenType::Output => {
                if let Resolved::OutputTarget { ext, path } = res {
                    outputs.push((ext.clone(), path.clone()));
                }
            }
            TokenType::Require => {
                // Search the PATH the child will actually see: an earlier
                // `Env PATH` overrides the inherited one.
                let env_path = spawn_env
                    .iter()
                    .rev()
                    .find(|(k, _)| k == "PATH")
                    .map(|(_, v)| v.as_str());
                if which(&cmd.args, env_path).is_none() {
                    report.set_failure(
                        None,
                        ExitKind::Runtime.reason(),
                        format!("Require: {} not found in PATH", cmd.args),
                    );
                    return ExitKind::Runtime;
                }
            }
            TokenType::Set | TokenType::Env | TokenType::Hide | TokenType::Show => {}
            _ => started = true,
        }
    }

    let initial_theme = settings.theme.clone();

    // ---- Renderer + geometry (cols/rows derive from font metrics).
    let mut renderer = Renderer::new(settings.render.clone(), settings.theme.clone());
    let (cols, rows) = renderer.term_size();
    if cols < 10 || rows < 2 {
        report.set_failure(
            None,
            ExitKind::Runtime.reason(),
            format!(
                "terminal too small: {cols}x{rows} cells (check Width/Height/Padding/FontSize)"
            ),
        );
        return ExitKind::Runtime;
    }

    // ---- Spawn the session with a pinned, deterministic environment.
    let argv = shell_argv(&settings.shell);
    let mut env = vec![
        ("TERM".to_string(), "xterm-256color".to_string()),
        ("PS1".to_string(), "> ".to_string()),
        ("PROMPT_COMMAND".to_string(), String::new()),
        ("HISTFILE".to_string(), String::new()),
        ("LANG".to_string(), "C.UTF-8".to_string()),
        ("LC_ALL".to_string(), "C.UTF-8".to_string()),
        ("VHS_RS".to_string(), "1".to_string()),
    ];
    env.extend(spawn_env);

    let mut session = match Session::spawn(&argv, &env, cols, rows) {
        Ok(s) => s,
        Err(e) => {
            report.set_failure(
                None,
                ExitKind::Runtime.reason(),
                format!("failed to spawn {argv:?}: {e}"),
            );
            return ExitKind::Runtime;
        }
    };
    report.set_term(cols, rows, settings.shell.clone());

    // Golden writer for `Output .txt/.ascii/.test` (records after every command).
    let mut golden = txt::GoldenWriter::new();

    // One owner for artifact bookkeeping: planned golden targets, written
    // artifacts (drained into the report at the end), collision warnings,
    // and forensics naming.
    let mut registry = ArtifactRegistry::new(&outputs, tape_name, quiet);

    // Implicit initial wait for the prompt — removes the classic race where
    // typing starts before the shell is up. A missing prompt is a warning
    // (custom shells may prompt differently), but a child that already
    // exited can never run anything: without this check a typo'd
    // `Set Shell` would sail through to a false success, because writes
    // into a dead PTY still land in the kernel buffer.
    let initial_wait = with_deadline(
        deadline,
        wait_for(
            &mut session,
            Scope::Line,
            &settings.wait_pattern.clone(),
            settings.wait_timeout,
        ),
    )
    .await;
    match initial_wait {
        None => {
            report.set_failure(
                None,
                "run_timeout",
                "--timeout exceeded while waiting for the initial prompt".to_string(),
            );
            return ExitKind::Runtime;
        }
        Some(Ok(WaitOutcome::ChildExited)) => {
            report.set_failure(
                None,
                "child_exited",
                format!("shell {argv:?} exited before the tape started (bad Set Shell?)"),
            );
            return ExitKind::Runtime;
        }
        Some(Ok(WaitOutcome::TimedOut)) | Some(Err(_)) if !quiet => {
            eprintln!(
                "vhs-rs: warning: prompt did not match /{}/ within {:?}; continuing",
                settings.wait_pattern.as_str(),
                settings.wait_timeout
            );
        }
        _ => {}
    }

    // ---- Command loop.
    let mut clipboard = String::new();
    let mut theme_timeline: Vec<(Duration, Theme)> = Vec::new();
    let mut exit = ExitKind::Success;

    for (index, (cmd, res)) in commands.iter().zip(resolved.iter()).enumerate() {
        let step_start = Instant::now();
        let result = with_deadline(
            deadline,
            execute(
                cmd,
                res,
                &mut session,
                &mut settings,
                &mut renderer,
                &mut clipboard,
                &mut theme_timeline,
                &mut registry,
                index,
                quiet,
            ),
        )
        .await
        .unwrap_or_else(|| {
            Err(StepFailure {
                exit: ExitKind::Runtime,
                reason: Some("run_timeout"),
                message: "--timeout exceeded; aborting the run".to_string(),
                detail: None,
            })
        });

        let elapsed = step_start.elapsed();
        match result {
            Ok(detail) => {
                report.record(index, cmd, CommandStatus::Ok, elapsed, detail);
            }
            Err(fail) => {
                report.record(index, cmd, CommandStatus::Failed, elapsed, fail.detail);
                let reason = fail.reason.unwrap_or_else(|| fail.exit.reason());
                report.set_failure(Some(index), reason, fail.message);
                exit = fail.exit;
                // Everything after the failed command never ran.
                for (rest_index, rest_cmd) in commands.iter().enumerate().skip(index + 1) {
                    report.record(
                        rest_index,
                        rest_cmd,
                        CommandStatus::Skipped,
                        Duration::ZERO,
                        None,
                    );
                }
                break;
            }
        }

        if registry.has_golden_targets() {
            golden.record(&session.term().text());
        }
    }

    // ---- Failure forensics: dump exactly what the terminal showed.
    if exit != ExitKind::Success {
        let _ = session.drain();
        let (text_path, png_path) = registry.forensics_paths();
        if txt::write_capture(&text_path, &session.term().text()).is_ok() {
            registry.record(text_path.to_string_lossy(), ArtifactKind::FailureText, None);
        }
        let canvas = renderer.render(&session.term().snapshot());
        if png::write_png(&png_path, canvas).is_ok() {
            registry.record(png_path.to_string_lossy(), ArtifactKind::FailurePng, None);
        }
    }

    // ---- Teardown before encoding (frees the child; events are all captured).
    let _ = session.shutdown().await;

    // ---- End-of-run outputs. Encode errors are real failures (unlike VHS).
    for (ext, path) in &outputs {
        let result = match ext.as_str() {
            ".txt" | ".ascii" | ".test" => {
                golden.save(Path::new(path)).map(|_| ArtifactKind::Golden)
            }
            ".png" => {
                let canvas = renderer.render(&session.term().snapshot());
                png::write_png(Path::new(path), canvas).map(|_| ArtifactKind::Png)
            }
            ".gif" => encode_gif(
                path,
                &settings,
                &mut renderer,
                session.events(),
                (cols, rows),
                initial_theme.clone(),
                &theme_timeline,
            )
            .map(|_| ArtifactKind::Gif),
            ".cast" => cast::write_cast(
                Path::new(path),
                &cast::CastMeta {
                    cols,
                    rows,
                    command: Some(settings.shell.clone()),
                    title: None,
                    env: vec![("TERM".into(), "xterm-256color".into())],
                },
                session.events(),
            )
            .map(|_| ArtifactKind::Cast),
            other => {
                // validate() should have caught this; belt and braces.
                report.set_failure(
                    None,
                    ExitKind::Runtime.reason(),
                    format!("unsupported output {other}"),
                );
                exit = ExitKind::Runtime;
                continue;
            }
        };

        match result {
            Ok(kind) => registry.record(path.clone(), kind, None),
            Err(e) => {
                report.set_failure(
                    None,
                    ExitKind::Runtime.reason(),
                    format!("failed to write {path}: {e}"),
                );
                if exit == ExitKind::Success {
                    exit = ExitKind::Runtime;
                }
            }
        }
    }

    registry.drain_into(report);
    exit
}

/// Executes one command from its typed resolution (`res` carries everything
/// that was parsed out of the command strings; `cmd` remains for line
/// numbers and verbatim data). `Ok(detail)` on success, `Err(StepFailure)`
/// aborts.
#[allow(clippy::too_many_arguments)]
async fn execute(
    cmd: &Command,
    res: &Resolved,
    session: &mut Session,
    settings: &mut Settings,
    renderer: &mut Renderer,
    clipboard: &mut String,
    theme_timeline: &mut Vec<(Duration, Theme)>,
    registry: &mut ArtifactRegistry,
    index: usize,
    quiet: bool,
) -> Result<Option<serde_json::Value>, StepFailure> {
    use TokenType::*;

    match cmd.command_type {
        // Handled in the pre-pass.
        Output | Require => Ok(None),
        Env => {
            if !quiet {
                eprintln!(
                    "vhs-rs: warning: Env after commands started has no effect (line {})",
                    cmd.token.line
                );
            }
            Ok(None)
        }

        Type => {
            let Resolved::TypeText { speed, text } = res else {
                return Err(resolved_mismatch(cmd));
            };
            let speed = speed.unwrap_or(settings.typing_speed);
            let mut buf = [0u8; 4];
            for ch in text.chars() {
                session
                    .write(ch.encode_utf8(&mut buf).as_bytes())
                    .await
                    .map_err(io_fail)?;
                settle(session, speed).await;
            }
            Ok(None)
        }

        Enter | Space | Backspace | Delete | Insert | Escape | Tab | Down | Left | Right | Up
        | PageUp | PageDown | Home | End => {
            let Resolved::Keypress { speed, count } = res else {
                return Err(resolved_mismatch(cmd));
            };
            let speed = speed.unwrap_or(settings.typing_speed);
            let bytes = keys::keypress_bytes(cmd.command_type, session.application_cursor())
                .ok_or_else(|| runtime_fail(format!("no key mapping for {}", cmd.command_type)))?
                .to_vec();
            for _ in 0..*count {
                session.write(&bytes).await.map_err(io_fail)?;
                settle(session, speed).await;
            }
            Ok(None)
        }

        ScrollUp | ScrollDown => {
            let Resolved::Keypress { speed, count } = res else {
                return Err(resolved_mismatch(cmd));
            };
            let speed = speed.unwrap_or(settings.typing_speed);
            let _ = session.drain();
            if mouse_reporting_enabled(session.events()) {
                let cursor = session.term().cursor();
                let bytes = keys::wheel_bytes(cmd.command_type == ScrollUp, cursor.col, cursor.row);
                for _ in 0..*count {
                    session.write(&bytes).await.map_err(io_fail)?;
                    settle(session, speed).await;
                }
            } else if !quiet {
                eprintln!(
                    "vhs-rs: warning: {}: child has not enabled mouse reporting; ignored",
                    cmd.command_type
                );
            }
            Ok(None)
        }

        Ctrl | Alt | Shift => {
            let bytes = match cmd.command_type {
                Ctrl => keys::ctrl_bytes(&cmd.args),
                Alt => keys::alt_bytes(&cmd.args),
                _ => keys::shift_bytes(&cmd.args),
            };
            session.write(&bytes).await.map_err(io_fail)?;
            settle(session, settings.typing_speed).await;
            Ok(None)
        }

        Sleep => {
            let Resolved::Sleep { duration } = res else {
                return Err(resolved_mismatch(cmd));
            };
            settle(session, *duration).await;
            Ok(None)
        }

        Wait => {
            let Resolved::WaitLike {
                scope,
                regex,
                timeout,
            } = res
            else {
                return Err(resolved_mismatch(cmd));
            };
            let scope = *scope;
            let regex = regex.as_ref().unwrap_or(&settings.wait_pattern);
            let timeout = timeout.unwrap_or(settings.wait_timeout);
            let started = Instant::now();
            match wait_for(session, scope, regex, timeout)
                .await
                .map_err(io_fail)?
            {
                WaitOutcome::Matched => {
                    Ok(Some(match_detail(scope, regex, Some(started.elapsed()))))
                }
                WaitOutcome::TimedOut => {
                    let message = format!(
                        "timeout waiting for /{}/ to match {}; last value was: {}",
                        regex.as_str(),
                        scope.name(),
                        scope.text(session.term()).lines().last().unwrap_or("")
                    );
                    Err(match_failure(
                        ExitKind::WaitTimeout,
                        None,
                        scope,
                        regex,
                        session.term(),
                        message,
                    ))
                }
                WaitOutcome::ChildExited => {
                    let message = format!(
                        "child exited while waiting for /{}/ to match {}",
                        regex.as_str(),
                        scope.name(),
                    );
                    Err(match_failure(
                        ExitKind::Runtime,
                        Some("child_exited"),
                        scope,
                        regex,
                        session.term(),
                        message,
                    ))
                }
            }
        }

        Assert => {
            let Resolved::WaitLike {
                scope,
                regex,
                timeout,
            } = res
            else {
                return Err(resolved_mismatch(cmd));
            };
            let scope = *scope;
            let regex = regex.as_ref().unwrap_or(&settings.wait_pattern);
            let outcome = match timeout {
                // No timeout: a single immediate check.
                None => {
                    let _ = session.drain();
                    if regex.is_match(&scope.text(session.term())) {
                        WaitOutcome::Matched
                    } else {
                        WaitOutcome::TimedOut
                    }
                }
                Some(timeout) => wait_for(session, scope, regex, *timeout)
                    .await
                    .map_err(io_fail)?,
            };
            if outcome == WaitOutcome::Matched {
                Ok(Some(match_detail(scope, regex, None)))
            } else {
                // A dead child still fails the *assertion* (exit 1): the
                // pattern genuinely isn't on the final screen. The message
                // carries the child-exited hint.
                let message = format!(
                    "Assert /{}/ did not match {}{}",
                    regex.as_str(),
                    scope.name(),
                    if outcome == WaitOutcome::ChildExited {
                        " (child exited before the deadline)"
                    } else {
                        ""
                    }
                );
                Err(match_failure(
                    ExitKind::AssertFailed,
                    None,
                    scope,
                    regex,
                    session.term(),
                    message,
                ))
            }
        }

        Screenshot => {
            let Resolved::PathCommand { path } = res else {
                return Err(resolved_mismatch(cmd));
            };
            let _ = session.drain();
            let snap = session.term().snapshot();
            let canvas = renderer.render(&snap);
            png::write_png(Path::new(path), canvas)
                .map_err(|e| runtime_fail(format!("screenshot {path}: {e}")))?;
            registry.record(path.clone(), ArtifactKind::Png, Some(index));
            // Text sibling: the same screen as the agent's cheap input. Skip
            // it if that path is a registered `Output` golden target — the
            // end-of-run golden write must not be clobbered.
            let txt_path = PathBuf::from(path).with_extension("txt");
            let txt_str = txt_path.to_string_lossy();
            if registry.is_golden_target(&txt_str) {
                if !quiet {
                    eprintln!(
                        "vhs-rs: warning: screenshot text sibling {txt_str} collides with an Output golden target; skipped"
                    );
                }
            } else {
                txt::write_capture(&txt_path, &session.term().text())
                    .map_err(|e| runtime_fail(format!("screenshot text sibling: {e}")))?;
                registry.record(txt_str, ArtifactKind::Text, Some(index));
            }
            Ok(Some(serde_json::json!({"path": path})))
        }

        Capture => {
            let Resolved::PathCommand { path } = res else {
                return Err(resolved_mismatch(cmd));
            };
            let _ = session.drain();
            txt::write_capture(Path::new(path), &session.term().text())
                .map_err(|e| runtime_fail(format!("capture {path}: {e}")))?;
            registry.record(path.clone(), ArtifactKind::Text, Some(index));
            Ok(Some(serde_json::json!({"path": path})))
        }

        Hide => {
            session.note_visibility(false);
            Ok(None)
        }
        Show => {
            session.note_visibility(true);
            Ok(None)
        }

        Copy => {
            let Resolved::CopyText { text } = res else {
                return Err(resolved_mismatch(cmd));
            };
            *clipboard = text.clone();
            Ok(None)
        }
        Paste => {
            session.write(clipboard.as_bytes()).await.map_err(io_fail)?;
            settle(session, settings.typing_speed).await;
            Ok(None)
        }

        Set => {
            // Only runtime-mutable settings reach here (validate() enforces it,
            // and the pre-pass already applied the preamble ones — reapplying
            // is idempotent for those).
            apply_setting(settings, cmd, quiet);
            if cmd.options == "Theme" {
                renderer.set_theme(settings.theme.clone());
                theme_timeline.push((session.elapsed(), settings.theme.clone()));
            }
            Ok(None)
        }

        Source | Illegal => Ok(None),
        other => Err(runtime_fail(format!("unhandled command {other}"))),
    }
}

// ---- Wait/Assert machinery --------------------------------------------------

/// Report detail for a successful Wait/Assert match (`elapsed` only for Wait).
fn match_detail(scope: Scope, regex: &Regex, elapsed: Option<Duration>) -> serde_json::Value {
    let mut detail = serde_json::json!({
        "scope": scope.name(), "regex": regex.as_str(), "matched": true,
    });
    if let Some(elapsed) = elapsed {
        detail["elapsed_ms"] = (elapsed.as_millis() as u64).into();
    }
    detail
}

/// `StepFailure` for a Wait timeout / Assert mismatch. `screen_text` always
/// carries the full screen at check time (the agent's primary evidence);
/// Line-scoped checks additionally get `line_text`, the line the regex was
/// actually matched against.
fn match_failure(
    exit: ExitKind,
    reason: Option<&'static str>,
    scope: Scope,
    regex: &Regex,
    term: &Term,
    message: String,
) -> StepFailure {
    let mut detail = serde_json::json!({
        "scope": scope.name(), "regex": regex.as_str(),
        "matched": false, "screen_text": term.text(),
    });
    if scope == Scope::Line {
        detail["line_text"] = term.current_line().into();
    }
    StepFailure {
        exit,
        reason,
        message,
        detail: Some(detail),
    }
}

/// Event-driven wait: check, then await the next output chunk, re-check.
/// Distinguishes a passed deadline from a child that exited without ever
/// matching — the two need different remediation (longer timeout vs. a
/// dead session), so they must not collapse into one "timeout".
async fn wait_for(
    session: &mut Session,
    scope: Scope,
    regex: &Regex,
    timeout: Duration,
) -> std::io::Result<WaitOutcome> {
    let deadline = Instant::now() + timeout;
    loop {
        session.drain()?;
        if regex.is_match(&scope.text(session.term())) {
            return Ok(WaitOutcome::Matched);
        }
        if session.exited() {
            return Ok(WaitOutcome::ChildExited);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(WaitOutcome::TimedOut);
        }
        session.wait_change(remaining).await?;
    }
}

/// Runs `fut` under the optional whole-run deadline (`--timeout`); `None`
/// means the deadline elapsed before `fut` completed.
async fn with_deadline<T>(
    deadline: Option<tokio::time::Instant>,
    fut: impl Future<Output = T>,
) -> Option<T> {
    match deadline {
        None => Some(fut.await),
        Some(d) => tokio::time::timeout_at(d, fut).await.ok(),
    }
}

/// Sleeps for `d` wall time while continuing to drain PTY output, so the
/// event log keeps accurate timestamps during pauses.
async fn settle(session: &mut Session, d: Duration) {
    let deadline = Instant::now() + d;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        if session.exited() {
            tokio::time::sleep(remaining).await;
            return;
        }
        let _ = session.wait_change(remaining).await;
        let _ = session.drain();
    }
}

// ---- GIF replay ---------------------------------------------------------------

/// Replays the event log through a fresh emulator, rendering each visible
/// state change into the styled frame and streaming it to the encoder.
#[allow(clippy::too_many_arguments)]
fn encode_gif(
    path: &str,
    settings: &Settings,
    renderer: &mut Renderer,
    events: &[SessionEvent],
    size: (usize, usize),
    initial_theme: Theme,
    theme_timeline: &[(Duration, Theme)],
) -> std::io::Result<()> {
    let (cols, rows) = size;
    let opts = renderer.options().clone();
    let mut term = Term::new(cols, rows);
    let mut enc = gif::GifEncoder::create(
        Path::new(path),
        gif::GifOptions {
            max_fps: settings.max_fps,
            playback_speed: settings.playback_speed,
            ..gif::GifOptions::new(opts.width as u16, opts.height as u16)
        },
    )?;
    if let Some(p) = settings.loop_offset {
        enc.set_loop_offset(p);
    }

    // Start from the tape's initial theme; apply mid-tape changes at their time.
    renderer.set_theme(initial_theme);
    let mut theme_idx = 0;
    let mut visible = true;
    let blink = settings.cursor_blink;
    // Double-buffered snapshots: `last_snap` is the grid of the last pushed
    // frame (valid while `last_time` is `Some`; blink frames synthesize from
    // it), `scratch` receives the next snapshot, and the two swap after each
    // push — so per-event snapshotting allocates nothing at steady state.
    let empty_snap = || crate::snapshot::GridSnapshot {
        cols: 0,
        rows: 0,
        cells: Vec::new(),
        cursor: crate::snapshot::Cursor {
            col: 0,
            row: 0,
            visible: false,
        },
    };
    let mut last_snap = empty_snap();
    let mut scratch = empty_snap();
    let mut last_time: Option<Duration> = None;

    // Renders idle-gap blink toggles between the last frame and `until`.
    let synth = |renderer: &mut Renderer,
                 enc: &mut gif::GifEncoder,
                 last: Option<Duration>,
                 snap: &crate::snapshot::GridSnapshot,
                 until: Duration|
     -> std::io::Result<()> {
        let Some(since) = last else {
            return Ok(());
        };
        if !snap.cursor.visible || until.saturating_sub(since) > BLINK_MAX_GAP {
            return Ok(());
        }
        for (t, on) in blink_frames(since, until, BLINK_HALF_PERIOD) {
            let canvas = renderer.render_frame(snap, on);
            enc.push_frame(t, &canvas.buf)?;
        }
        Ok(())
    };

    let mut end_time = Duration::ZERO;
    for ev in events {
        while theme_idx < theme_timeline.len() && theme_timeline[theme_idx].0 <= ev.time {
            renderer.set_theme(theme_timeline[theme_idx].1.clone());
            theme_idx += 1;
        }
        end_time = end_time.max(ev.time);
        match &ev.kind {
            SessionEventKind::Output(s) => {
                if visible && blink {
                    synth(renderer, &mut enc, last_time, &last_snap, ev.time)?;
                }
                term.feed(s);
                if visible {
                    term.snapshot_into(&mut scratch);
                    let cursor_on = !blink || blink_phase_on(ev.time, BLINK_HALF_PERIOD);
                    let canvas = renderer.render_frame(&scratch, cursor_on);
                    enc.push_frame(ev.time, &canvas.buf)?;
                    std::mem::swap(&mut last_snap, &mut scratch);
                    last_time = Some(ev.time);
                }
            }
            SessionEventKind::Resize(c, r) => {
                term.resize(*c, *r);
                last_time = None; // stale grid; don't synthesize from it
            }
            SessionEventKind::Visibility(v) => visible = *v,
            SessionEventKind::Exit => {
                // Blink through the trailing idle gap before the child exits.
                if visible && blink {
                    synth(renderer, &mut enc, last_time, &last_snap, ev.time)?;
                }
                break;
            }
        }
    }

    // The held final frame must end cursor-visible: re-push the last grid
    // with the cursor on (coalesces if the pending frame already shows it).
    if blink && last_time.is_some() {
        let canvas = renderer.render_frame(&last_snap, true);
        enc.push_frame(end_time, &canvas.buf)?;
    }

    // Restore the final theme for any later renders (Output .png, forensics).
    renderer.set_theme(settings.theme.clone());
    enc.finish()?;
    Ok(())
}

/// Blink phase at absolute session time `t`: `true` = cursor shown.
fn blink_phase_on(t: Duration, half_period: Duration) -> bool {
    (t.as_millis() / half_period.as_millis()).is_multiple_of(2)
}

/// Blink toggle boundaries strictly inside `(start, end)`, each paired with
/// the phase that begins there (`true` = cursor shown). Pure so the boundary
/// math is unit-testable; phases align to absolute time, not the gap start,
/// so cadence stays continuous across frames.
fn blink_frames(start: Duration, end: Duration, half_period: Duration) -> Vec<(Duration, bool)> {
    let half_ms = half_period.as_millis() as u64;
    let mut frames = Vec::new();
    if half_ms == 0 || end <= start {
        return frames;
    }
    let start_ms = start.as_millis() as u64;
    let end_ms = end.as_millis() as u64;
    let mut k = start_ms / half_ms + 1; // first boundary after `start`
    while k * half_ms < end_ms {
        frames.push((Duration::from_millis(k * half_ms), k.is_multiple_of(2)));
        k += 1;
    }
    frames
}

/// Whether the child has mouse reporting enabled: scans Output events for
/// DEC private modes 1000/1002/1003 (`CSI ? … h|l`, `;`-separated parameter
/// lists included); the last set/reset wins. Only called on scroll commands,
/// so a linear scan is fine.
fn mouse_reporting_enabled(events: &[SessionEvent]) -> bool {
    static MODE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\x1b\[\?([0-9;]+)([hl])").expect("static regex"));
    let re = &*MODE_RE;
    let mut enabled = false;
    for ev in events {
        if let SessionEventKind::Output(s) = &ev.kind {
            for cap in re.captures_iter(s) {
                if cap[1]
                    .split(';')
                    .any(|p| matches!(p, "1000" | "1002" | "1003"))
                {
                    enabled = &cap[2] == "h";
                }
            }
        }
    }
    enabled
}

// ---- Settings -------------------------------------------------------------------

fn apply_setting(settings: &mut Settings, cmd: &Command, quiet: bool) {
    let v = cmd.args.as_str();
    let warn = |msg: String| {
        if !quiet {
            eprintln!("vhs-rs: warning: {msg}");
        }
    };
    match cmd.options.as_str() {
        "Shell" => settings.shell = v.into(),
        "FontSize" => set_f32(&mut settings.render.font_size, v),
        "FontFamily" => warn(format!(
            "Set FontFamily {v}: vhs_rs uses the embedded JetBrains Mono; ignored"
        )),
        "Width" => set_usize(&mut settings.render.width, v),
        "Height" => set_usize(&mut settings.render.height, v),
        "Padding" => set_usize(&mut settings.render.padding, v),
        "Margin" => set_usize(&mut settings.render.margin, v),
        "MarginFill" => {
            if let Some(c) = Rgb::from_hex(v) {
                settings.render.margin_fill = MarginFill::Color(c);
            } else {
                warn(format!(
                    "MarginFill {v}: not a color; using theme background"
                ));
            }
        }
        "WindowBar" => match v.parse::<BarStyle>() {
            Ok(style) => settings.render.window_bar = Some(style),
            Err(_) if v.is_empty() => settings.render.window_bar = None,
            Err(e) => warn(e),
        },
        "WindowBarSize" => set_usize(&mut settings.render.window_bar_size, v),
        "BorderRadius" => set_usize(&mut settings.render.border_radius, v),
        "LetterSpacing" => set_f32(&mut settings.render.letter_spacing, v),
        "LineHeight" => set_f32(&mut settings.render.line_height, v),
        "TypingSpeed" => match parse_duration(v) {
            Some(d) => settings.typing_speed = d,
            None => warn(format!("TypingSpeed {v}: not a duration; ignored")),
        },
        "PlaybackSpeed" => {
            if let Ok(f) = v.parse::<f64>()
                && f > 0.0
            {
                settings.playback_speed = f;
            } else {
                warn(format!("PlaybackSpeed {v}: not a positive number; ignored"));
            }
        }
        "Framerate" => {
            if let Ok(f) = v.parse::<f64>()
                && f > 0.0
            {
                settings.max_fps = f.min(50.0);
            } else {
                warn(format!("Framerate {v}: not a positive number; ignored"));
            }
        }
        "WaitTimeout" => match parse_duration(v) {
            Some(d) => settings.wait_timeout = d,
            None => warn(format!("WaitTimeout {v}: not a duration; ignored")),
        },
        "WaitPattern" => match Regex::new(v) {
            Ok(re) => settings.wait_pattern = re,
            Err(_) => warn(format!("WaitPattern {v}: not a valid regex; ignored")),
        },
        "Theme" => {
            let parsed = if v.trim_start().starts_with('{') {
                theme::from_json(v).ok()
            } else {
                theme::load_builtin(v)
            };
            match parsed {
                Some(t) => settings.theme = t,
                None => warn(format!("unknown theme {v:?}; keeping current")),
            }
        }
        "CursorBlink" => match v {
            "true" => settings.cursor_blink = true,
            "false" => settings.cursor_blink = false,
            other => warn(format!("CursorBlink {other}: expected true or false")),
        },
        "LoopOffset" => match v.trim_end_matches('%').parse::<f64>() {
            Ok(p) if (0.0..=100.0).contains(&p) => settings.loop_offset = Some(p),
            _ => warn(format!(
                "LoopOffset {v}: expected a percentage 0-100; ignored"
            )),
        },
        other => warn(format!("unknown setting {other}")),
    }
}

fn set_usize(target: &mut usize, v: &str) {
    if let Ok(n) = v.parse::<f64>()
        && n >= 0.0
    {
        *target = n as usize;
    }
}

fn set_f32(target: &mut f32, v: &str) {
    if let Ok(n) = v.parse::<f32>()
        && n > 0.0
    {
        *target = n;
    }
}

fn shell_argv(shell: &str) -> Vec<String> {
    match shell {
        "bash" => vec![
            "bash".into(),
            "--noprofile".into(),
            "--norc".into(),
            "-i".into(),
        ],
        "sh" => vec!["sh".into(), "-i".into()],
        "zsh" => vec!["zsh".into(), "-f".into(), "-i".into()],
        "fish" => vec!["fish".into(), "--no-config".into(), "-i".into()],
        custom => custom.split_whitespace().map(String::from).collect(),
    }
}

/// Finds `bin` on `env_path` (the child's `Env PATH` override) or the
/// inherited PATH. Only executable regular files count — a data file that
/// happens to share the binary's name must not satisfy `Require`.
fn which(bin: &str, env_path: Option<&str>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = env_path
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"))?;
    std::env::split_paths(&path).map(|dir| dir.join(bin)).find(|p| {
        p.is_file()
            && p.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    })
}

fn io_fail(e: std::io::Error) -> StepFailure {
    runtime_fail(format!("PTY I/O error: {e}"))
}

fn runtime_fail(message: String) -> StepFailure {
    StepFailure {
        exit: ExitKind::Runtime,
        reason: None,
        message,
        detail: None,
    }
}

/// The resolution side table disagreed with the command list — impossible
/// unless `resolve_commands` and `execute` drift apart.
fn resolved_mismatch(cmd: &Command) -> StepFailure {
    runtime_fail(format!(
        "internal error: resolution mismatch for {} (line {})",
        cmd.command_type, cmd.token.line
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn blink_phase_follows_half_periods() {
        let half = ms(530);
        // [0, 530) on, [530, 1060) off, [1060, 1590) on, ...
        assert!(blink_phase_on(ms(0), half));
        assert!(blink_phase_on(ms(529), half));
        assert!(!blink_phase_on(ms(530), half));
        assert!(!blink_phase_on(ms(1059), half));
        assert!(blink_phase_on(ms(1060), half));
        assert!(!blink_phase_on(ms(530 * 3), half));
    }

    #[test]
    fn blink_frames_boundary_math() {
        let half = ms(530);

        // Gap shorter than the half-period: no synthesized frames.
        assert!(blink_frames(ms(0), ms(529), half).is_empty());
        assert!(blink_frames(ms(600), ms(1000), half).is_empty());

        // Empty and inverted ranges.
        assert!(blink_frames(ms(100), ms(100), half).is_empty());
        assert!(blink_frames(ms(200), ms(100), half).is_empty());

        // A 2s gap from t=0 crosses boundaries at 530/1060/1590 with
        // alternating phases (off, on, off).
        let frames = blink_frames(ms(0), ms(2000), half);
        assert_eq!(
            frames,
            vec![(ms(530), false), (ms(1060), true), (ms(1590), false)]
        );

        // Boundaries are exclusive at both ends.
        let frames = blink_frames(ms(530), ms(1060), half);
        assert!(frames.is_empty(), "got {frames:?}");

        // Phase aligns to absolute time, not to the gap start: a gap starting
        // mid-phase still toggles at global boundaries.
        let frames = blink_frames(ms(700), ms(1700), half);
        assert_eq!(frames, vec![(ms(1060), true), (ms(1590), false)]);
    }

    fn out(s: &str) -> SessionEvent {
        SessionEvent {
            time: Duration::ZERO,
            kind: SessionEventKind::Output(s.into()),
        }
    }

    /// Replaying the same event log twice must produce byte-identical GIFs,
    /// and the file's structure (dimensions, frame count, delays) must match
    /// what the blink/coalescing rules predict independently.
    #[test]
    fn encode_gif_replay_is_deterministic() {
        let at = |ms: u64, kind: SessionEventKind| SessionEvent {
            time: Duration::from_millis(ms),
            kind,
        };
        // Blink half-period is 530ms: the 100→700ms gap synthesizes an
        // off-frame at 530, the 700→1200 gap an on-frame at 1060; the final
        // cursor-on re-push at 1200 is identical to the pending frame at
        // 1060 and coalesces.
        let events = vec![
            at(0, SessionEventKind::Output("hello".into())),
            at(100, SessionEventKind::Output("x".into())),
            at(700, SessionEventKind::Output("y".into())),
            at(1200, SessionEventKind::Exit),
        ];

        let settings = Settings {
            render: RenderOptions {
                width: 200,
                height: 100,
                padding: 10,
                font_size: 16.0,
                ..RenderOptions::default()
            },
            ..Settings::default()
        };
        let mut renderer = Renderer::new(settings.render.clone(), settings.theme.clone());

        let path = |run: usize| {
            std::env::temp_dir().join(format!(
                "vhs_rs-eval-gif-determinism-{}-{run}.gif",
                std::process::id()
            ))
        };
        for run in 0..2 {
            encode_gif(
                path(run).to_str().unwrap(),
                &settings,
                &mut renderer,
                &events,
                (16, 5),
                settings.theme.clone(),
                &[],
            )
            .unwrap();
        }
        let bytes0 = std::fs::read(path(0)).unwrap();
        let bytes1 = std::fs::read(path(1)).unwrap();
        assert_eq!(bytes0, bytes1, "two replays produced different files");

        // Structure: frames at 0/100/530/700/1060 written (the 1200 re-push
        // coalesces), delays = successor gaps in centiseconds + 1s hold.
        // `::gif` is the external decoder crate (the local `gif` name is the
        // encoder module imported above).
        let mut options = ::gif::DecodeOptions::new();
        options.set_color_output(::gif::ColorOutput::RGBA);
        let mut decoder = options
            .read_info(std::fs::File::open(path(0)).unwrap())
            .unwrap();
        assert_eq!((decoder.width(), decoder.height()), (200, 100));
        let mut delays = Vec::new();
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            delays.push(frame.delay);
        }
        assert_eq!(delays, vec![10, 43, 17, 36, 100]);

        for run in 0..2 {
            std::fs::remove_file(path(run)).ok();
        }
    }

    #[test]
    fn mouse_mode_scanner() {
        // (events, expected)
        let cases: &[(Vec<SessionEvent>, bool)] = &[
            (vec![], false),
            (vec![out("plain output, no modes")], false),
            (vec![out("\x1b[?1000h")], true),
            (vec![out("\x1b[?1002h")], true),
            (vec![out("\x1b[?1003h")], true),
            // The last occurrence wins, across events.
            (vec![out("\x1b[?1000h"), out("\x1b[?1000l")], false),
            (vec![out("\x1b[?1000l"), out("\x1b[?1002h")], true),
            (vec![out("pre\x1b[?1000h mid \x1b[?1000l post")], false),
            // Combined parameter lists (e.g. vim: mouse + SGR ext together).
            (vec![out("\x1b[?1002;1006h")], true),
            (vec![out("\x1b[?1006;1000l")], false),
            // Look-alike private modes must not trigger.
            (
                vec![out("\x1b[?1004h\x1b[?1006h\x1b[?1049h\x1b[?25h")],
                false,
            ),
            (vec![out("\x1b[?12h\x1b[?1h")], false),
            // Non-Output events are ignored.
            (
                vec![
                    out("\x1b[?1000h"),
                    SessionEvent {
                        time: Duration::ZERO,
                        kind: SessionEventKind::Exit,
                    },
                ],
                true,
            ),
        ];

        for (events, expected) in cases {
            assert_eq!(
                mouse_reporting_enabled(events),
                *expected,
                "events: {events:?}"
            );
        }
    }
}
