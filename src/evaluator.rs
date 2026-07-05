//! The evaluator: executes parsed tape commands against a live PTY session,
//! renders artifacts, and assembles the run report.
//!
//! This is the integration point of the whole crate: session (PTY + avt),
//! renderer (fontdue rasterizer), and encoders (png/gif/txt/cast) meet here.
//! Waits and asserts are event-driven — check the rendered buffer, then await
//! the next PTY chunk with a deadline; no polling.

use crate::command::Command;
use crate::encode::{cast, gif, png, txt};
use crate::error::ExitKind;
use crate::keys;
use crate::render::{BarStyle, MarginFill, RenderOptions, Renderer};
use crate::report::{ArtifactKind, CommandStatus, ReportBuilder};
use crate::session::Session;
use crate::snapshot::{SessionEvent, SessionEventKind};
use crate::term::Term;
use crate::theme::{self, Rgb, Theme};
use crate::token::TokenType;
use crate::util::parse_duration;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Everything `Set` can configure, with VHS defaults.
struct Settings {
    shell: String,
    typing_speed: Duration,
    wait_timeout: Duration,
    wait_pattern: Regex,
    playback_speed: f64,
    max_fps: f64,
    render: RenderOptions,
    theme: Theme,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            shell: "bash".into(),
            typing_speed: Duration::from_millis(50),
            wait_timeout: Duration::from_secs(15),
            wait_pattern: Regex::new(">$").expect("default pattern"),
            playback_speed: 1.0,
            max_fps: 50.0,
            render: RenderOptions::default(),
            theme: theme::default_theme(),
        }
    }
}

/// Outcome of a single failed step, mapped to the exit taxonomy.
struct StepFailure {
    exit: ExitKind,
    reason: &'static str,
    message: String,
    detail: Option<serde_json::Value>,
}

/// Entry point called by the CLI after parse+validate succeeded.
pub fn run(tape_name: &str, commands: &[Command], json: bool, quiet: bool) -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("vterm: failed to start runtime: {e}");
            return ExitKind::Runtime as i32;
        }
    };

    let mut report = ReportBuilder::new(tape_name);
    let exit = rt.block_on(run_inner(tape_name, commands, &mut report, quiet));
    let report = report.finish(exit);

    if json {
        println!("{}", report.to_json());
    } else if !quiet {
        if let Some(f) = &report.failure {
            eprintln!("vterm: {} — {}", f.reason, f.message);
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
) -> ExitKind {
    // ---- Settings pre-pass: everything before the first action command.
    let mut settings = Settings::default();
    let mut spawn_env: Vec<(String, String)> = Vec::new();
    let mut outputs: Vec<(String, String)> = Vec::new(); // (ext, path)
    let mut started = false;

    for cmd in commands {
        match cmd.command_type {
            TokenType::Set if !started => apply_setting(&mut settings, cmd, quiet),
            TokenType::Env if !started => {
                spawn_env.push((cmd.options.clone(), cmd.args.clone()));
            }
            TokenType::Output => outputs.push((cmd.options.clone(), cmd.args.clone())),
            TokenType::Require => {
                if which(&cmd.args).is_none() {
                    report.set_failure(
                        None,
                        "runtime_error",
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
            "runtime_error",
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
        ("VTERM".to_string(), "1".to_string()),
    ];
    env.extend(spawn_env);

    let mut session = match Session::spawn(&argv, &env, cols, rows) {
        Ok(s) => s,
        Err(e) => {
            report.set_failure(
                None,
                "runtime_error",
                format!("failed to spawn {argv:?}: {e}"),
            );
            return ExitKind::Runtime;
        }
    };
    report.set_term(cols, rows, settings.shell.clone());

    // Golden writer for `Output .txt/.ascii/.test` (records after every command).
    let mut golden = txt::GoldenWriter::new();
    let goldens_registered = outputs
        .iter()
        .any(|(ext, _)| matches!(ext.as_str(), ".txt" | ".ascii" | ".test"));

    // Forensics stem: first output path (sans extension), else the tape name.
    let forensics_stem = outputs
        .first()
        .map(|(ext, p)| p.trim_end_matches(ext.as_str()).to_string())
        .unwrap_or_else(|| tape_name.trim_end_matches(".tape").to_string());

    // Implicit initial wait for the prompt — removes the classic race where
    // typing starts before the shell is up. Non-fatal: warn and continue.
    if !wait_for(
        &mut session,
        Scope::Line,
        &settings.wait_pattern.clone(),
        settings.wait_timeout,
    )
    .await
    .unwrap_or(false)
        && !quiet
    {
        eprintln!(
            "vterm: warning: prompt did not match /{}/ within {:?}; continuing",
            settings.wait_pattern.as_str(),
            settings.wait_timeout
        );
    }

    // ---- Command loop.
    let mut clipboard = String::new();
    let mut theme_timeline: Vec<(Duration, Theme)> = Vec::new();
    let mut exit = ExitKind::Success;

    for (index, cmd) in commands.iter().enumerate() {
        let step_start = Instant::now();
        let result = execute(
            cmd,
            &mut session,
            &mut settings,
            &mut renderer,
            &mut clipboard,
            &mut theme_timeline,
            report,
            index,
            quiet,
        )
        .await;

        let elapsed = step_start.elapsed();
        match result {
            Ok(detail) => {
                report.record(index, cmd, CommandStatus::Ok, elapsed, detail);
            }
            Err(fail) => {
                report.record(index, cmd, CommandStatus::Failed, elapsed, fail.detail);
                report.set_failure(Some(index), fail.reason, fail.message);
                exit = fail.exit;
                break;
            }
        }

        if goldens_registered {
            golden.record(&session.term().text());
        }
    }

    // ---- Failure forensics: dump exactly what the terminal showed.
    if exit != ExitKind::Success {
        let _ = session.drain();
        let text_path = format!("{forensics_stem}.failure.txt");
        if txt::write_capture(Path::new(&text_path), &session.term().text()).is_ok() {
            report.add_artifact(&text_path, ArtifactKind::FailureText, None);
        }
        let png_path = format!("{forensics_stem}.failure.png");
        let canvas = renderer.render(&session.term().snapshot());
        if png::write_png(Path::new(&png_path), canvas).is_ok() {
            report.add_artifact(&png_path, ArtifactKind::FailurePng, None);
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
                report.set_failure(None, "runtime_error", format!("unsupported output {other}"));
                exit = ExitKind::Runtime;
                continue;
            }
        };

        match result {
            Ok(kind) => report.add_artifact(path.clone(), kind, None),
            Err(e) => {
                report.set_failure(
                    None,
                    "runtime_error",
                    format!("failed to write {path}: {e}"),
                );
                if exit == ExitKind::Success {
                    exit = ExitKind::Runtime;
                }
            }
        }
    }

    exit
}

/// Executes one command. `Ok(detail)` on success, `Err(StepFailure)` aborts.
#[allow(clippy::too_many_arguments)]
async fn execute(
    cmd: &Command,
    session: &mut Session,
    settings: &mut Settings,
    renderer: &mut Renderer,
    clipboard: &mut String,
    theme_timeline: &mut Vec<(Duration, Theme)>,
    report: &mut ReportBuilder,
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
                    "vterm: warning: Env after commands started has no effect (line {})",
                    cmd.token.line
                );
            }
            Ok(None)
        }

        Type => {
            let speed = speed_of(cmd, settings.typing_speed);
            let mut buf = [0u8; 4];
            for ch in cmd.args.chars() {
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
            let speed = speed_of(cmd, settings.typing_speed);
            let count: usize = cmd.args.parse().unwrap_or(1);
            let bytes = keys::keypress_bytes(cmd.command_type, session.application_cursor())
                .ok_or_else(|| runtime_fail(format!("no key mapping for {}", cmd.command_type)))?
                .to_vec();
            for _ in 0..count {
                session.write(&bytes).await.map_err(io_fail)?;
                settle(session, speed).await;
            }
            Ok(None)
        }

        ScrollUp | ScrollDown => {
            // Deferred: needs synthesized mouse-wheel events. Explicit, not silent.
            Err(runtime_fail(format!(
                "{} is not supported yet",
                cmd.command_type
            )))
        }

        Ctrl => {
            session
                .write(&keys::ctrl_bytes(&cmd.args))
                .await
                .map_err(io_fail)?;
            settle(session, settings.typing_speed).await;
            Ok(None)
        }
        Alt => {
            session
                .write(&keys::alt_bytes(&cmd.args))
                .await
                .map_err(io_fail)?;
            settle(session, settings.typing_speed).await;
            Ok(None)
        }
        Shift => {
            session
                .write(&keys::shift_bytes(&cmd.args))
                .await
                .map_err(io_fail)?;
            settle(session, settings.typing_speed).await;
            Ok(None)
        }

        Sleep => {
            let d = parse_duration(&cmd.args)
                .ok_or_else(|| runtime_fail(format!("bad duration {}", cmd.args)))?;
            settle(session, d).await;
            Ok(None)
        }

        Wait => {
            let (scope, regex) = wait_args(cmd, settings)?;
            let timeout = timeout_of(cmd, settings.wait_timeout)?;
            let started = Instant::now();
            if wait_for(session, scope, &regex, timeout)
                .await
                .map_err(io_fail)?
            {
                Ok(Some(serde_json::json!({
                    "scope": scope.name(), "regex": regex.as_str(),
                    "matched": true, "elapsed_ms": started.elapsed().as_millis() as u64,
                })))
            } else {
                let seen = scope.text(session.term());
                Err(StepFailure {
                    exit: ExitKind::WaitTimeout,
                    reason: "wait_timeout",
                    message: format!(
                        "timeout waiting for /{}/ to match {}; last value was: {}",
                        regex.as_str(),
                        scope.name(),
                        seen.lines().last().unwrap_or("")
                    ),
                    detail: Some(serde_json::json!({
                        "scope": scope.name(), "regex": regex.as_str(),
                        "matched": false, "screen_text": seen,
                    })),
                })
            }
        }

        Assert => {
            let (scope, regex) = wait_args(cmd, settings)?;
            let matched = if cmd.options.is_empty() {
                let _ = session.drain();
                regex.is_match(&scope.text(session.term()))
            } else {
                let timeout = timeout_of(cmd, settings.wait_timeout)?;
                wait_for(session, scope, &regex, timeout)
                    .await
                    .map_err(io_fail)?
            };
            if matched {
                Ok(Some(serde_json::json!({
                    "scope": scope.name(), "regex": regex.as_str(), "matched": true,
                })))
            } else {
                let seen = scope.text(session.term());
                Err(StepFailure {
                    exit: ExitKind::AssertFailed,
                    reason: "assert_failed",
                    message: format!("Assert /{}/ did not match {}", regex.as_str(), scope.name()),
                    detail: Some(serde_json::json!({
                        "scope": scope.name(), "regex": regex.as_str(),
                        "matched": false, "screen_text": seen,
                    })),
                })
            }
        }

        Screenshot => {
            let _ = session.drain();
            let snap = session.term().snapshot();
            let canvas = renderer.render(&snap);
            png::write_png(Path::new(&cmd.args), canvas)
                .map_err(|e| runtime_fail(format!("screenshot {}: {e}", cmd.args)))?;
            report.add_artifact(&cmd.args, ArtifactKind::Png, Some(index));
            // Text sibling: the same screen as the agent's cheap input.
            let txt_path = PathBuf::from(&cmd.args).with_extension("txt");
            txt::write_capture(&txt_path, &session.term().text())
                .map_err(|e| runtime_fail(format!("screenshot text sibling: {e}")))?;
            report.add_artifact(txt_path.to_string_lossy(), ArtifactKind::Text, Some(index));
            Ok(Some(serde_json::json!({"path": cmd.args})))
        }

        Capture => {
            let _ = session.drain();
            txt::write_capture(Path::new(&cmd.args), &session.term().text())
                .map_err(|e| runtime_fail(format!("capture {}: {e}", cmd.args)))?;
            report.add_artifact(&cmd.args, ArtifactKind::Text, Some(index));
            Ok(Some(serde_json::json!({"path": cmd.args})))
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
            *clipboard = cmd.args.clone();
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

#[derive(Clone, Copy, PartialEq)]
enum Scope {
    Line,
    Screen,
}

impl Scope {
    fn name(&self) -> &'static str {
        match self {
            Scope::Line => "Line",
            Scope::Screen => "Screen",
        }
    }
    fn text(&self, term: &Term) -> String {
        match self {
            Scope::Line => term.current_line(),
            Scope::Screen => term.text(),
        }
    }
}

fn wait_args(cmd: &Command, settings: &Settings) -> Result<(Scope, Regex), StepFailure> {
    let (scope_str, pattern) = match cmd.args.split_once(' ') {
        Some((s, re)) => (s, Some(re)),
        None => (cmd.args.as_str(), None),
    };
    let scope = match scope_str {
        "Screen" => Scope::Screen,
        _ => Scope::Line,
    };
    let regex = match pattern {
        Some(re) => {
            Regex::new(re).map_err(|e| runtime_fail(format!("invalid regex /{re}/: {e}")))?
        }
        None => settings.wait_pattern.clone(),
    };
    Ok((scope, regex))
}

/// Event-driven wait: check, then await the next output chunk, re-check.
/// Returns Ok(false) on timeout or if the child exits without matching.
async fn wait_for(
    session: &mut Session,
    scope: Scope,
    regex: &Regex,
    timeout: Duration,
) -> std::io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        session.drain()?;
        if regex.is_match(&scope.text(session.term())) {
            return Ok(true);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || session.exited() {
            return Ok(false);
        }
        session.wait_change(remaining).await?;
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

    // Start from the tape's initial theme; apply mid-tape changes at their time.
    renderer.set_theme(initial_theme);
    let mut theme_idx = 0;
    let mut visible = true;

    for ev in events {
        while theme_idx < theme_timeline.len() && theme_timeline[theme_idx].0 <= ev.time {
            renderer.set_theme(theme_timeline[theme_idx].1.clone());
            theme_idx += 1;
        }
        match &ev.kind {
            SessionEventKind::Output(s) => {
                term.feed(s);
                if visible {
                    let canvas = renderer.render(&term.snapshot());
                    enc.push_frame(ev.time, &canvas.buf)?;
                }
            }
            SessionEventKind::Resize(c, r) => term.resize(*c, *r),
            SessionEventKind::Visibility(v) => visible = *v,
            SessionEventKind::Exit => break,
        }
    }

    // Restore the final theme for any later renders (Output .png, forensics).
    renderer.set_theme(settings.theme.clone());
    enc.finish()?;
    Ok(())
}

// ---- Settings -------------------------------------------------------------------

fn apply_setting(settings: &mut Settings, cmd: &Command, quiet: bool) {
    let v = cmd.args.as_str();
    let warn = |msg: String| {
        if !quiet {
            eprintln!("vterm: warning: {msg}");
        }
    };
    match cmd.options.as_str() {
        "Shell" => settings.shell = v.into(),
        "FontSize" => set_f32(&mut settings.render.font_size, v),
        "FontFamily" => warn(format!(
            "Set FontFamily {v}: vterm uses the embedded JetBrains Mono; ignored"
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
        "TypingSpeed" => {
            if let Some(d) = parse_duration(v) {
                settings.typing_speed = d;
            }
        }
        "PlaybackSpeed" => {
            if let Ok(f) = v.parse::<f64>() {
                if f > 0.0 {
                    settings.playback_speed = f;
                }
            }
        }
        "Framerate" => {
            if let Ok(f) = v.parse::<f64>() {
                if f > 0.0 {
                    settings.max_fps = f.min(50.0);
                }
            }
        }
        "WaitTimeout" => {
            if let Some(d) = parse_duration(v) {
                settings.wait_timeout = d;
            }
        }
        "WaitPattern" => {
            if let Ok(re) = Regex::new(v) {
                settings.wait_pattern = re;
            }
        }
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
        "CursorBlink" => { /* parsed; blink animation lands in M5 */ }
        "LoopOffset" => warn("Set LoopOffset is not supported yet; ignored".into()),
        other => warn(format!("unknown setting {other}")),
    }
}

fn set_usize(target: &mut usize, v: &str) {
    if let Ok(n) = v.parse::<f64>() {
        if n >= 0.0 {
            *target = n as usize;
        }
    }
}

fn set_f32(target: &mut f32, v: &str) {
    if let Ok(n) = v.parse::<f32>() {
        if n > 0.0 {
            *target = n;
        }
    }
}

fn speed_of(cmd: &Command, default: Duration) -> Duration {
    if cmd.options.is_empty() {
        default
    } else {
        parse_duration(&cmd.options).unwrap_or(default)
    }
}

fn timeout_of(cmd: &Command, default: Duration) -> Result<Duration, StepFailure> {
    if cmd.options.is_empty() {
        Ok(default)
    } else {
        parse_duration(&cmd.options)
            .ok_or_else(|| runtime_fail(format!("bad timeout {}", cmd.options)))
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

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|p| p.is_file())
}

fn io_fail(e: std::io::Error) -> StepFailure {
    runtime_fail(format!("PTY I/O error: {e}"))
}

fn runtime_fail(message: String) -> StepFailure {
    StepFailure {
        exit: ExitKind::Runtime,
        reason: "runtime_error",
        message,
        detail: None,
    }
}
