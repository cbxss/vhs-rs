//! The evaluator: executes parsed tape commands against a live PTY session,
//! renders artifacts, and assembles the run report.
//!
//! This is the integration point of the whole crate: session (PTY + avt),
//! renderer (fontdue rasterizer), and encoders (png/gif/txt/cast) meet here.
//! Waits and asserts are event-driven — check the rendered buffer, then await
//! the next PTY chunk with a deadline; no polling.

use crate::artifacts::ArtifactRegistry;
use crate::command::Command;
use crate::encode::{cast, png, txt};
use crate::error::ExitKind;
use crate::keys;
use crate::render::{BarStyle, MarginFill, RenderOptions, Renderer};
use crate::replay::{self, ReplaySpec};
use crate::report::{ArtifactKind, CommandStatus, ReportBuilder};
use crate::resolve::{Resolved, Scope, resolve_commands};
use crate::session::Session;
use crate::snapshot::{SessionEvent, SessionEventKind};
use crate::term::Term;
use crate::theme::{self, Rgb, Theme};
use crate::timeline::{CommandMarker, TIMELINE_VERSION, TimelineHeader, TimelineWriter};
use crate::token::TokenType;
use crate::util::parse_duration;
use regex::Regex;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::signal::unix::{SignalKind, signal};

/// Everything `Set` can configure, with VHS defaults.
pub(crate) struct Settings {
    pub(crate) shell: String,
    typing_speed: Duration,
    pub(crate) wait_timeout: Duration,
    pub(crate) wait_pattern: Regex,
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
pub(crate) struct StepFailure {
    pub(crate) exit: ExitKind,
    pub(crate) reason: Option<&'static str>,
    pub(crate) message: String,
    pub(crate) detail: Option<serde_json::Value>,
}

/// What ended a [`wait_for`]: the pattern matched, the deadline passed, or
/// the child exited (and the pattern can never match).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitOutcome {
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
/// is the whole-run wall-clock budget (`--timeout`); `record` is the
/// `--record` timeline path (in addition to any `Output x.jsonl` targets).
/// On SIGINT/SIGTERM the report is still finalized and printed before
/// exiting.
pub fn run(
    tape_name: &str,
    commands: &[Command],
    json: bool,
    quiet: bool,
    timeout: Option<Duration>,
    record: Option<&str>,
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
        let fut = run_inner(tape_name, commands, &mut report, quiet, deadline, record);
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
    record: Option<&str>,
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
            // Silent here: the command loop re-applies every preamble Set
            // (idempotently) and owns the one user-facing warning per line.
            TokenType::Set if !started => apply_setting(&mut settings, cmd, true),
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

    let mut engine = match Engine::spawn(
        tape_name, settings, spawn_env, &outputs, record, quiet, report,
    ) {
        Ok(engine) => engine,
        Err(msg) => {
            report.set_failure(None, ExitKind::Runtime.reason(), msg);
            return ExitKind::Runtime;
        }
    };

    // Implicit initial wait for the prompt — removes the classic race where
    // typing starts before the shell is up. A missing prompt is a warning
    // (custom shells may prompt differently), but a child that already
    // exited can never run anything: without this check a typo'd
    // `Set Shell` would sail through to a false success, because writes
    // into a dead PTY still land in the kernel buffer.
    let initial_wait = with_deadline(deadline, engine.initial_prompt_wait()).await;
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
                format!(
                    "shell {:?} exited before the tape started (bad Set Shell?)",
                    engine.argv
                ),
            );
            return ExitKind::Runtime;
        }
        Some(Ok(WaitOutcome::TimedOut)) | Some(Err(_)) if !quiet => {
            eprintln!(
                "vhs-rs: warning: prompt did not match /{}/ within {:?}; continuing",
                engine.settings.wait_pattern.as_str(),
                engine.settings.wait_timeout
            );
        }
        _ => {}
    }

    // ---- Command loop.
    let mut exit = ExitKind::Success;

    for (index, (cmd, res)) in commands.iter().zip(resolved.iter()).enumerate() {
        let step_start = Instant::now();
        let result = with_deadline(deadline, engine.exec(index, cmd, res))
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
        engine.record_marker(index, cmd, result.is_ok(), elapsed);
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

        engine.record_golden_frame().await;
    }

    // ---- Failure forensics: dump exactly what the terminal showed.
    if exit != ExitKind::Success {
        engine.write_forensics();
    }

    engine.finish(&outputs, report, &mut exit).await;
    exit
}

/// The live execution state for one spawned session: everything a tape
/// command can touch, shared between the batch evaluator and the repl. The
/// batch pre-pass (settings/Env/Require before spawn) stays with the
/// callers; the Engine takes over once the terminal exists.
pub(crate) struct Engine {
    session: Session,
    settings: Settings,
    renderer: Renderer,
    /// Theme at spawn time (GIF replays start here).
    initial_theme: Theme,
    argv: Vec<String>,
    cols: usize,
    rows: usize,
    clipboard: String,
    theme_timeline: Vec<(Duration, Theme)>,
    registry: ArtifactRegistry,
    golden: txt::GoldenWriter,
    recorder: Recorder,
    quiet: bool,
}

impl Engine {
    /// Builds the renderer, spawns the shell on a pinned deterministic
    /// environment, and opens the artifact/recording machinery. Sets the
    /// report's term info as soon as the terminal exists. `Err` carries the
    /// user-facing failure message (the caller maps it to `runtime_error`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn(
        tape_name: &str,
        settings: Settings,
        spawn_env: Vec<(String, String)>,
        outputs: &[(String, String)],
        record: Option<&str>,
        quiet: bool,
        report: &mut ReportBuilder,
    ) -> Result<Self, String> {
        let initial_theme = settings.theme.clone();

        // ---- Renderer + geometry (cols/rows derive from font metrics).
        let renderer = Renderer::new(settings.render.clone(), settings.theme.clone());
        let (cols, rows) = renderer.term_size();
        if cols < 10 || rows < 2 {
            return Err(format!(
                "terminal too small: {cols}x{rows} cells (check Width/Height/Padding/FontSize)"
            ));
        }

        // ---- Spawn the session with a pinned, deterministic environment.
        let argv = shell_argv(&settings.shell);
        // C.UTF-8 is a glibc/musl locale; macOS doesn't ship it, and a locale
        // that fails to resolve falls back to plain C — readline then mangles
        // multibyte input, which surfaced as golden-tape timeouts on macOS CI.
        let locale = if cfg!(target_os = "macos") {
            "en_US.UTF-8"
        } else {
            "C.UTF-8"
        };
        let mut env = vec![
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("PS1".to_string(), "> ".to_string()),
            // Deliberately does NOT match the default WaitPattern (`>$`):
            // bash's stock PS2 is "> ", so an unclosed quote would otherwise
            // show a continuation prompt that `Wait` mistakes for "the
            // prompt is back".
            ("PS2".to_string(), "... ".to_string()),
            ("PROMPT_COMMAND".to_string(), String::new()),
            ("HISTFILE".to_string(), String::new()),
            ("LANG".to_string(), locale.to_string()),
            ("LC_ALL".to_string(), locale.to_string()),
            // macOS bash 3.2 otherwise opens every interactive session with
            // the "default shell is now zsh" banner, polluting the screen.
            (
                "BASH_SILENCE_DEPRECATION_WARNING".to_string(),
                "1".to_string(),
            ),
            ("VHS_RS".to_string(), "1".to_string()),
        ];
        env.extend(spawn_env);

        let session = Session::spawn(&argv, &env, cols, rows)
            .map_err(|e| format!("failed to spawn {argv:?}: {e}"))?;
        report.set_term(cols, rows, settings.shell.clone());

        // One owner for artifact bookkeeping: planned golden targets,
        // written artifacts (drained into the report at the end), collision
        // warnings, and forensics naming.
        let registry = ArtifactRegistry::new(outputs, tape_name, quiet);

        // Streaming timeline recorders: every `Output x.jsonl` target plus
        // `--record`. Created (header written) before anything runs, so even
        // a killed run leaves a renderable file. Note the header carries the
        // preamble settings; a mid-tape `Set PlaybackSpeed` affects this
        // run's own GIF but not a later `render` of the recording.
        let jsonl_targets: Vec<String> = outputs
            .iter()
            .filter(|(ext, _)| ext == ".jsonl")
            .map(|(_, path)| path.clone())
            .chain(record.map(String::from))
            .collect();
        let header = TimelineHeader {
            version: TIMELINE_VERSION,
            cols,
            rows,
            shell: settings.shell.clone(),
            tape: Some(tape_name.to_string()),
            theme: initial_theme.clone(),
            render: settings.render.clone(),
            cursor_blink: settings.cursor_blink,
            max_fps: settings.max_fps,
            playback_speed: settings.playback_speed,
            loop_offset: settings.loop_offset,
        };
        let recorder = Recorder::create(&jsonl_targets, &header)
            .map_err(|(path, e)| format!("failed to create timeline {path}: {e}"))?;

        Ok(Self {
            session,
            settings,
            renderer,
            initial_theme,
            argv,
            cols,
            rows,
            clipboard: String::new(),
            theme_timeline: Vec::new(),
            // Golden writer for `Output .txt/.ascii/.test` (records after
            // every command).
            golden: txt::GoldenWriter::new(),
            registry,
            recorder,
            quiet,
        })
    }

    /// The implicit wait for the shell prompt before the first keystroke.
    pub(crate) async fn initial_prompt_wait(&mut self) -> std::io::Result<WaitOutcome> {
        let pattern = self.settings.wait_pattern.clone();
        wait_for(
            &mut self.session,
            Scope::Line,
            &pattern,
            self.settings.wait_timeout,
        )
        .await
    }

    /// Executes one command against the live session.
    pub(crate) async fn exec(
        &mut self,
        index: usize,
        cmd: &Command,
        res: &Resolved,
    ) -> Result<Option<serde_json::Value>, StepFailure> {
        execute(
            cmd,
            res,
            &mut self.session,
            &mut self.settings,
            &mut self.renderer,
            &mut self.clipboard,
            &mut self.theme_timeline,
            &mut self.registry,
            index,
            self.quiet,
        )
        .await
    }

    /// Streams the just-finished command (events, theme changes, boundary
    /// marker) to the timeline recorders, when any are active.
    pub(crate) fn record_marker(
        &mut self,
        index: usize,
        cmd: &Command,
        ok: bool,
        elapsed: Duration,
    ) {
        if !self.recorder.active() {
            return;
        }
        let marker = CommandMarker {
            index,
            line: cmd.token.line,
            command: cmd.to_string(),
            status: if ok { "ok" } else { "failed" }.into(),
            elapsed_ms: elapsed.as_millis() as u64,
        };
        self.recorder
            .observe(&self.session, &self.theme_timeline, &marker);
    }

    /// Appends the post-command golden frame, when golden targets exist.
    pub(crate) async fn record_golden_frame(&mut self) {
        if !self.registry.has_golden_targets() {
            return;
        }
        // Byte-identical goldens must not depend on whether a keystroke's
        // echo landed inside the typing-speed settle window — on a loaded
        // machine (CI) it regularly misses. Wait for the PTY to go briefly
        // quiet before recording the frame.
        quiesce(
            &mut self.session,
            Duration::from_millis(25),
            Duration::from_millis(250),
        )
        .await;
        self.golden.record(&self.session.term().text());
    }

    /// Failure forensics: dump exactly what the terminal showed.
    pub(crate) fn write_forensics(&mut self) {
        let _ = self.session.drain();
        let (text_path, png_path) = self.registry.forensics_paths();
        if txt::write_capture(&text_path, &self.session.term().text()).is_ok() {
            self.registry
                .record(text_path.to_string_lossy(), ArtifactKind::FailureText, None);
        }
        let canvas = self.renderer.render(&self.session.term().snapshot());
        if png::write_png(&png_path, canvas).is_ok() {
            self.registry
                .record(png_path.to_string_lossy(), ArtifactKind::FailurePng, None);
        }
    }

    /// Teardown and end-of-run outputs: kills the child, flushes the
    /// recorders, encodes every `Output` target, and drains the artifact
    /// list into the report. Encode errors are real failures (unlike VHS):
    /// they set the report failure and flip `exit` to runtime.
    pub(crate) async fn finish(
        mut self,
        outputs: &[(String, String)],
        report: &mut ReportBuilder,
        exit: &mut ExitKind,
    ) {
        // ---- Teardown before encoding (frees the child; events are all
        // captured).
        let _ = self.session.shutdown().await;
        // The shutdown drain + Exit event are the timeline's final lines.
        self.recorder.sync_events(&self.session);

        for (ext, path) in outputs {
            let result = match ext.as_str() {
                // Already streamed while the run happened; the recorder
                // records the artifact (and any write failure) below.
                ".jsonl" => continue,
                ".txt" | ".ascii" | ".test" => self
                    .golden
                    .save(Path::new(path))
                    .map(|_| ArtifactKind::Golden),
                ".png" => {
                    let canvas = self.renderer.render(&self.session.term().snapshot());
                    png::write_png(Path::new(path), canvas).map(|_| ArtifactKind::Png)
                }
                ".gif" => {
                    let spec = ReplaySpec {
                        max_fps: self.settings.max_fps,
                        playback_speed: self.settings.playback_speed,
                        loop_offset: self.settings.loop_offset,
                        cursor_blink: self.settings.cursor_blink,
                        initial_theme: self.initial_theme.clone(),
                        theme_timeline: self.theme_timeline.clone(),
                    };
                    // Hide→Show wall time is cut, not frozen: without this
                    // the pre-Hide frame inherits the whole hidden span as
                    // its delay (VHS cuts hidden sections; so do we).
                    let events = crate::timeline::collapse_hidden(self.session.events());
                    let result = replay::encode_gif(
                        Path::new(path),
                        &spec,
                        &mut self.renderer,
                        &events,
                        (self.cols, self.rows),
                    );
                    // The replay leaves the renderer on the timeline's last
                    // theme; later renders (Output .png, forensics) must use
                    // the run's final theme.
                    self.renderer.set_theme(self.settings.theme.clone());
                    result.map(|_| ArtifactKind::Gif)
                }
                ".cast" => cast::write_cast(
                    Path::new(path),
                    &cast::CastMeta {
                        cols: self.cols,
                        rows: self.rows,
                        command: Some(self.settings.shell.clone()),
                        title: None,
                        env: vec![("TERM".into(), "xterm-256color".into())],
                    },
                    self.session.events(),
                )
                .map(|_| ArtifactKind::Cast),
                other => {
                    // validate() should have caught this; belt and braces.
                    report.set_failure(
                        None,
                        ExitKind::Runtime.reason(),
                        format!("unsupported output {other}"),
                    );
                    *exit = ExitKind::Runtime;
                    continue;
                }
            };

            match result {
                Ok(kind) => self.registry.record(path.clone(), kind, None),
                Err(e) => {
                    report.set_failure(
                        None,
                        ExitKind::Runtime.reason(),
                        format!("failed to write {path}: {e}"),
                    );
                    if *exit == ExitKind::Success {
                        *exit = ExitKind::Runtime;
                    }
                }
            }
        }

        self.recorder.finish(&mut self.registry, report, exit);
        self.registry.drain_into(report);
    }

    pub(crate) fn argv(&self) -> &[String] {
        &self.argv
    }

    pub(crate) fn term_info(&self) -> (usize, usize, String) {
        (self.cols, self.rows, self.settings.shell.clone())
    }

    pub(crate) fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(crate) fn exited(&self) -> bool {
        self.session.exited()
    }

    pub(crate) fn drain(&mut self) -> std::io::Result<bool> {
        let changed = self.session.drain()?;
        if changed {
            self.recorder.sync_events(&self.session);
        }
        Ok(changed)
    }

    pub(crate) async fn wait_change(&mut self, deadline: Duration) -> std::io::Result<bool> {
        let changed = self.session.wait_change(deadline).await?;
        if changed {
            let _ = self.session.drain()?;
            self.recorder.sync_events(&self.session);
        }
        Ok(changed)
    }

    pub(crate) fn add_timeline_output(
        &mut self,
        tape_name: &str,
        path: &str,
    ) -> Result<(), String> {
        let header = TimelineHeader {
            version: TIMELINE_VERSION,
            cols: self.cols,
            rows: self.rows,
            shell: self.settings.shell.clone(),
            tape: Some(tape_name.to_string()),
            theme: self.initial_theme.clone(),
            render: self.settings.render.clone(),
            cursor_blink: self.settings.cursor_blink,
            max_fps: self.settings.max_fps,
            playback_speed: self.settings.playback_speed,
            loop_offset: self.settings.loop_offset,
        };
        self.recorder
            .add(
                path.to_string(),
                &header,
                &self.session,
                &self.theme_timeline,
            )
            .map_err(|e| format!("failed to create timeline {path}: {e}"))
    }
}

/// The set of live timeline writers for one run (`Output x.jsonl` targets +
/// `--record`), with per-writer failure isolation: a writer that errors
/// mid-run stops receiving events and surfaces as a failed artifact at the
/// end, without aborting the run or the other writers.
struct Recorder {
    writers: Vec<(String, TimelineWriter)>,
    failed: Vec<(String, std::io::Error)>,
    themes_written: usize,
}

impl Recorder {
    /// Opens a writer (and writes the header) for every path. Creation
    /// failure is fatal — a run that can't produce a requested artifact
    /// should not start.
    fn create(paths: &[String], header: &TimelineHeader) -> Result<Self, (String, std::io::Error)> {
        let mut writers = Vec::with_capacity(paths.len());
        for path in paths {
            match TimelineWriter::create(Path::new(path), header) {
                Ok(w) => writers.push((path.clone(), w)),
                Err(e) => return Err((path.clone(), e)),
            }
        }
        Ok(Self {
            writers,
            failed: Vec::new(),
            themes_written: 0,
        })
    }

    fn active(&self) -> bool {
        !self.writers.is_empty()
    }

    fn add(
        &mut self,
        path: String,
        header: &TimelineHeader,
        session: &Session,
        theme_timeline: &[(Duration, Theme)],
    ) -> std::io::Result<()> {
        let mut writer = TimelineWriter::create(Path::new(&path), header)?;
        writer.sync(session.events())?;
        for (time, theme) in theme_timeline {
            writer.write_theme(*time, theme)?;
        }
        self.writers.push((path, writer));
        Ok(())
    }

    /// Streams everything new since the last call: session events, theme
    /// changes, and the just-finished command's marker.
    fn observe(
        &mut self,
        session: &Session,
        theme_timeline: &[(Duration, Theme)],
        marker: &CommandMarker,
    ) {
        self.each(|w| w.sync(session.events()));
        while self.themes_written < theme_timeline.len() {
            let (time, theme) = &theme_timeline[self.themes_written];
            let (time, theme) = (*time, theme.clone());
            self.each(|w| w.write_theme(time, &theme));
            self.themes_written += 1;
        }
        let time = session.elapsed();
        self.each(|w| w.write_command(time, marker));
    }

    /// Streams any not-yet-written session events (the teardown tail).
    fn sync_events(&mut self, session: &Session) {
        self.each(|w| w.sync(session.events()));
    }

    /// Records surviving writers as artifacts and failed ones as run
    /// failures (runtime exit, first failure message wins in the report).
    fn finish(
        self,
        registry: &mut ArtifactRegistry,
        report: &mut ReportBuilder,
        exit: &mut ExitKind,
    ) {
        for (path, _) in self.writers {
            registry.record(path, ArtifactKind::Timeline, None);
        }
        for (path, e) in self.failed {
            report.set_failure(
                None,
                ExitKind::Runtime.reason(),
                format!("failed to write {path}: {e}"),
            );
            if *exit == ExitKind::Success {
                *exit = ExitKind::Runtime;
            }
        }
    }

    /// Applies `f` to every live writer, retiring writers that fail.
    fn each(&mut self, mut f: impl FnMut(&mut TimelineWriter) -> std::io::Result<()>) {
        let mut i = 0;
        while i < self.writers.len() {
            match f(&mut self.writers[i].1) {
                Ok(()) => i += 1,
                Err(e) => {
                    let (path, _) = self.writers.remove(i);
                    self.failed.push((path, e));
                }
            }
        }
    }
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

/// Drains until the PTY has been silent for `idle` (or `cap` total, so a
/// child that streams continuously can't stall the run). Used before golden
/// frames: a frame's content must not race the echo of the keystrokes that
/// produced it.
async fn quiesce(session: &mut Session, idle: Duration, cap: Duration) {
    let deadline = Instant::now() + cap;
    loop {
        let _ = session.drain();
        if session.exited() {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        match session.wait_change(idle.min(remaining)).await {
            Ok(true) => {} // output arrived; restart the idle window
            _ => return,   // silent for the whole window (or unreadable)
        }
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

pub(crate) fn apply_setting(settings: &mut Settings, cmd: &Command, quiet: bool) {
    let v = cmd.args.as_str();
    let warn = |msg: String| {
        if !quiet {
            eprintln!("vhs-rs: warning: {msg}");
        }
    };
    match cmd.options.as_str() {
        "Shell" => settings.shell = v.into(),
        "FontSize" => set_f32(&mut settings.render.font_size, "FontSize", v, &warn),
        "FontFamily" => warn(format!(
            "Set FontFamily {v}: vhs_rs uses the embedded JetBrains Mono; ignored"
        )),
        "Width" => set_usize(&mut settings.render.width, "Width", v, &warn),
        "Height" => set_usize(&mut settings.render.height, "Height", v, &warn),
        "Padding" => set_usize(&mut settings.render.padding, "Padding", v, &warn),
        "Margin" => set_usize(&mut settings.render.margin, "Margin", v, &warn),
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
        "WindowBarSize" => set_usize(
            &mut settings.render.window_bar_size,
            "WindowBarSize",
            v,
            &warn,
        ),
        "BorderRadius" => set_usize(&mut settings.render.border_radius, "BorderRadius", v, &warn),
        "LetterSpacing" => set_f32(
            &mut settings.render.letter_spacing,
            "LetterSpacing",
            v,
            &warn,
        ),
        "LineHeight" => set_f32(&mut settings.render.line_height, "LineHeight", v, &warn),
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

fn set_usize(target: &mut usize, name: &str, v: &str, warn: &dyn Fn(String)) {
    if let Ok(n) = v.parse::<f64>()
        && n >= 0.0
    {
        *target = n as usize;
    } else {
        warn(format!("{name} {v}: not a non-negative number; ignored"));
    }
}

fn set_f32(target: &mut f32, name: &str, v: &str, warn: &dyn Fn(String)) {
    if let Ok(n) = v.parse::<f32>()
        && n > 0.0
    {
        *target = n;
    } else {
        warn(format!("{name} {v}: not a positive number; ignored"));
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
pub(crate) fn which(bin: &str, env_path: Option<&str>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = env_path
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"))?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|p| {
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

    fn out(s: &str) -> SessionEvent {
        SessionEvent {
            time: Duration::ZERO,
            kind: SessionEventKind::Output(s.into()),
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
