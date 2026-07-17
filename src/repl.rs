//! `vhs-rs repl`: line-oriented live driving protocol for agents.
//!
//! Stdin is the tape language. Stdout is newline-delimited JSON only and is
//! flushed after every line so a piped agent never waits behind block
//! buffering. Stderr remains available for human warnings.

use std::io::{BufRead as _, Write};
use std::time::Duration;

use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::command::Command;
use crate::error::{ExitKind, ParseError};
use crate::evaluator::{self, Engine, Settings, StepFailure, WaitOutcome};
use crate::parser::is_runtime_setting;
use crate::report::{CommandStatus, ReportBuilder};
use crate::resolve::{Resolved, resolve_commands};
use crate::token::TokenType;

const TAPE_NAME: &str = "repl";
const PTY_WAIT: Duration = Duration::from_secs(3600);

/// CLI request for `vhs-rs repl`.
#[derive(Debug)]
pub struct ReplRequest {
    /// Abort the session on the first failed command, restoring batch-style
    /// control flow and exit taxonomy.
    pub strict: bool,
    /// Suppress warnings and progress output.
    pub quiet: bool,
    /// Whole-session wall-clock budget.
    pub timeout: Option<Duration>,
    /// Timeline path streamed as the session runs.
    pub record: Option<String>,
}

/// Runs the repl and returns the process exit code.
pub fn repl(req: &ReplRequest) -> i32 {
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

    rt.block_on(repl_inner(req))
}

struct ReplState<'a> {
    settings: Settings,
    spawn_env: Vec<(String, String)>,
    outputs: Vec<(String, String)>,
    engine: Option<Engine>,
    report: ReportBuilder,
    next_index: usize,
    strict: bool,
    quiet: bool,
    record: Option<&'a str>,
    exit: ExitKind,
    final_forensics_written: bool,
}

impl<'a> ReplState<'a> {
    fn new(req: &'a ReplRequest) -> Self {
        Self {
            settings: Settings::default(),
            spawn_env: Vec::new(),
            outputs: Vec::new(),
            engine: None,
            report: ReportBuilder::new(TAPE_NAME),
            next_index: 0,
            strict: req.strict,
            quiet: req.quiet,
            record: req.record.as_deref(),
            exit: ExitKind::Success,
            final_forensics_written: false,
        }
    }
}

enum Next {
    Line(Option<Result<String, String>>),
    Pty(std::io::Result<bool>),
    Timeout,
    Signal(&'static str),
}

async fn repl_inner(req: &ReplRequest) -> i32 {
    let mut out = std::io::stdout().lock();
    if !write_json_line(
        &mut out,
        &serde_json::json!({"kind": "ready", "version": 1}),
    ) {
        return ExitKind::Runtime as i32;
    }

    let mut state = ReplState::new(req);
    let mut lines = stdin_lines();
    let deadline = req.timeout.map(|d| tokio::time::Instant::now() + d);
    let mut input_line = 0usize;
    let mut sigterm = signal(SignalKind::terminate()).ok();
    let mut sigint = signal(SignalKind::interrupt()).ok();

    loop {
        let engine_active = state.engine.as_ref().is_some_and(|e| !e.exited());
        let next = tokio::select! {
            line = lines.recv() => Next::Line(line),
            changed = wait_pty(state.engine.as_mut()), if engine_active => Next::Pty(changed),
            _ = wait_deadline(deadline), if deadline.is_some() => Next::Timeout,
            _ = recv_signal(&mut sigterm) => Next::Signal("SIGTERM"),
            _ = recv_signal(&mut sigint) => Next::Signal("SIGINT"),
        };

        match next {
            Next::Line(None) => break,
            Next::Line(Some(Err(msg))) => {
                state.exit = ExitKind::Runtime;
                state
                    .report
                    .set_failure(None, ExitKind::Runtime.reason(), msg);
                break;
            }
            Next::Line(Some(Ok(line))) => {
                input_line += 1;
                if !handle_input_line(&mut state, &mut out, input_line, &line, deadline).await {
                    return ExitKind::Runtime as i32;
                }
                if state.exit != ExitKind::Success {
                    break;
                }
            }
            Next::Pty(Ok(_)) => {}
            Next::Pty(Err(e)) => {
                state.exit = ExitKind::Runtime;
                state.report.set_failure(
                    None,
                    ExitKind::Runtime.reason(),
                    format!("PTY I/O error: {e}"),
                );
                break;
            }
            Next::Timeout => {
                state.exit = ExitKind::Runtime;
                state.report.set_failure(
                    None,
                    "run_timeout",
                    "--timeout exceeded; aborting the repl session",
                );
                break;
            }
            Next::Signal(sig) => {
                state.exit = ExitKind::Runtime;
                state.report.set_failure(
                    None,
                    "interrupted",
                    format!("repl interrupted by {sig}; partial report follows"),
                );
                break;
            }
        }
    }

    finish(state, &mut out).await
}

async fn wait_pty(engine: Option<&mut Engine>) -> std::io::Result<bool> {
    match engine {
        Some(engine) => engine.wait_change(PTY_WAIT).await,
        None => std::future::pending().await,
    }
}

async fn wait_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn recv_signal(signal: &mut Option<tokio::signal::unix::Signal>) {
    match signal {
        Some(signal) => {
            let _ = signal.recv().await;
        }
        None => std::future::pending().await,
    }
}

fn stdin_lines() -> mpsc::Receiver<Result<String, String>> {
    let (tx, rx) = mpsc::channel(8);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let item = line.map_err(|e| format!("failed to read stdin: {e}"));
            if tx.blocking_send(item).is_err() {
                return;
            }
        }
    });
    rx
}

async fn handle_input_line(
    state: &mut ReplState<'_>,
    out: &mut impl Write,
    input_line: usize,
    line: &str,
    deadline: Option<tokio::time::Instant>,
) -> bool {
    let (commands, errors) = crate::parse_tape(line);
    if !errors.is_empty() {
        return write_json_line(out, &parse_error_response(input_line, &errors));
    }
    if commands.is_empty() {
        return write_json_line(
            out,
            &serde_json::json!({"kind": "empty", "input_line": input_line}),
        );
    }

    let resolved = match resolve_commands(&commands) {
        Ok(resolved) => resolved,
        Err(msg) => {
            state.exit = ExitKind::Runtime;
            state
                .report
                .set_failure(None, ExitKind::Runtime.reason(), msg.clone());
            return write_json_line(out, &runtime_error_response(input_line, msg));
        }
    };

    for (offset, (cmd, res)) in commands.iter().zip(resolved.iter()).enumerate() {
        let control = handle_command(state, out, input_line, cmd, res, deadline).await;
        if !control.wrote_response {
            return false;
        }
        if control.stop_session {
            for rest in commands.iter().skip(offset + 1) {
                let index = state.next_index;
                state.next_index += 1;
                state
                    .report
                    .record(index, rest, CommandStatus::Skipped, Duration::ZERO, None);
                if !write_json_line(
                    out,
                    &command_response(
                        index,
                        input_line,
                        rest,
                        CommandStatus::Skipped,
                        Duration::ZERO,
                        None,
                        None,
                    ),
                ) {
                    return false;
                }
            }
            break;
        }
    }

    true
}

struct CommandControl {
    wrote_response: bool,
    stop_session: bool,
}

async fn handle_command(
    state: &mut ReplState<'_>,
    out: &mut impl Write,
    input_line: usize,
    cmd: &Command,
    res: &Resolved,
    deadline: Option<tokio::time::Instant>,
) -> CommandControl {
    let index = state.next_index;
    state.next_index += 1;
    let step_start = std::time::Instant::now();

    if let Some(result) = handle_without_engine(state, cmd, res, index) {
        let elapsed = step_start.elapsed();
        return write_result(state, out, index, input_line, cmd, elapsed, result).await;
    }

    if let Err(msg) = ensure_engine(state, out).await {
        let fail = StepFailure {
            exit: ExitKind::Runtime,
            reason: None,
            message: msg,
            detail: None,
        };
        return write_result(
            state,
            out,
            index,
            input_line,
            cmd,
            step_start.elapsed(),
            Err(fail),
        )
        .await;
    }

    if let Some(result) = handle_after_engine(state, cmd, res, index) {
        let elapsed = step_start.elapsed();
        return write_result(state, out, index, input_line, cmd, elapsed, result).await;
    }

    if should_fail_when_child_exited(cmd)
        && let Some(engine) = state.engine.as_mut()
    {
        match engine.drain() {
            Ok(_) if engine.exited() => {
                let fail = StepFailure {
                    exit: ExitKind::Runtime,
                    reason: Some("child_exited"),
                    message: "child exited before this command could run".into(),
                    detail: None,
                };
                return write_result(
                    state,
                    out,
                    index,
                    input_line,
                    cmd,
                    step_start.elapsed(),
                    Err(fail),
                )
                .await;
            }
            Err(e) => {
                let fail = StepFailure {
                    exit: ExitKind::Runtime,
                    reason: None,
                    message: format!("PTY I/O error: {e}"),
                    detail: None,
                };
                return write_result(
                    state,
                    out,
                    index,
                    input_line,
                    cmd,
                    step_start.elapsed(),
                    Err(fail),
                )
                .await;
            }
            _ => {}
        }
    }

    let result = run_command_with_deadline(
        state.engine.as_mut().expect("spawned"),
        index,
        cmd,
        res,
        deadline,
    )
    .await;
    let elapsed = step_start.elapsed();
    write_result(state, out, index, input_line, cmd, elapsed, result).await
}

fn handle_without_engine(
    state: &mut ReplState<'_>,
    cmd: &Command,
    res: &Resolved,
    index: usize,
) -> Option<Result<Option<serde_json::Value>, StepFailure>> {
    if state.engine.is_some() {
        return None;
    }

    match cmd.command_type {
        TokenType::Set => {
            evaluator::apply_setting(&mut state.settings, cmd, state.quiet);
            Some(Ok(None))
        }
        TokenType::Env => {
            state
                .spawn_env
                .push((cmd.options.clone(), cmd.args.clone()));
            Some(Ok(None))
        }
        TokenType::Output => Some(add_output(state, res, index, false)),
        TokenType::Require => Some(require_ok(state, cmd)),
        _ => None,
    }
}

fn handle_after_engine(
    state: &mut ReplState<'_>,
    cmd: &Command,
    res: &Resolved,
    index: usize,
) -> Option<Result<Option<serde_json::Value>, StepFailure>> {
    match cmd.command_type {
        TokenType::Set if !is_runtime_setting(&cmd.options) => Some(Err(StepFailure {
            exit: ExitKind::Runtime,
            reason: None,
            message: format!(
                "Set {} cannot be used in repl after the shell has spawned",
                cmd.options
            ),
            detail: None,
        })),
        TokenType::Output => Some(add_output(state, res, index, true)),
        TokenType::Require => Some(require_ok(state, cmd)),
        _ => None,
    }
}

fn add_output(
    state: &mut ReplState<'_>,
    res: &Resolved,
    index: usize,
    spawned: bool,
) -> Result<Option<serde_json::Value>, StepFailure> {
    let Resolved::OutputTarget { ext, path } = res else {
        return Err(internal_fail("Output resolution mismatch"));
    };

    if spawned && matches!(ext.as_str(), ".txt" | ".ascii" | ".test") {
        return Err(StepFailure {
            exit: ExitKind::Runtime,
            reason: None,
            message: "golden text Output targets must be declared before the repl shell spawns"
                .into(),
            detail: None,
        });
    }

    if spawned
        && ext == ".jsonl"
        && let Some(engine) = state.engine.as_mut()
        && let Err(msg) = engine.add_timeline_output(TAPE_NAME, path)
    {
        return Err(StepFailure {
            exit: ExitKind::Runtime,
            reason: None,
            message: msg,
            detail: None,
        });
    }

    state.outputs.push((ext.clone(), path.clone()));
    Ok(Some(
        serde_json::json!({"path": path, "command_index": index}),
    ))
}

fn require_ok(
    state: &ReplState<'_>,
    cmd: &Command,
) -> Result<Option<serde_json::Value>, StepFailure> {
    let env_path = state
        .spawn_env
        .iter()
        .rev()
        .find(|(k, _)| k == "PATH")
        .map(|(_, v)| v.as_str());
    if evaluator::which(&cmd.args, env_path).is_some() {
        Ok(None)
    } else {
        Err(StepFailure {
            exit: ExitKind::Runtime,
            reason: None,
            message: format!("Require: {} not found in PATH", cmd.args),
            detail: None,
        })
    }
}

async fn ensure_engine(state: &mut ReplState<'_>, out: &mut impl Write) -> Result<(), String> {
    if state.engine.is_some() {
        return Ok(());
    }

    let mut engine = Engine::spawn(
        TAPE_NAME,
        std::mem::take(&mut state.settings),
        std::mem::take(&mut state.spawn_env),
        &state.outputs,
        state.record,
        state.quiet,
        &mut state.report,
    )?;

    match engine.initial_prompt_wait().await {
        Ok(WaitOutcome::ChildExited) => {
            return Err(format!(
                "shell {:?} exited before the repl started (bad Set Shell?)",
                engine.argv()
            ));
        }
        Ok(WaitOutcome::TimedOut) if !state.quiet => {
            eprintln!(
                "vhs-rs: warning: prompt did not match /{}/ within {:?}; continuing",
                engine.settings().wait_pattern.as_str(),
                engine.settings().wait_timeout,
            );
        }
        Err(e) if !state.quiet => {
            eprintln!("vhs-rs: warning: initial prompt wait failed: {e}; continuing");
        }
        _ => {}
    }

    let (cols, rows, shell) = engine.term_info();
    if !write_json_line(
        out,
        &serde_json::json!({"kind": "term", "cols": cols, "rows": rows, "shell": shell}),
    ) {
        return Err("failed to write term response".into());
    }
    state.engine = Some(engine);
    Ok(())
}

async fn run_command_with_deadline(
    engine: &mut Engine,
    index: usize,
    cmd: &Command,
    res: &Resolved,
    deadline: Option<tokio::time::Instant>,
) -> Result<Option<serde_json::Value>, StepFailure> {
    let fut = engine.exec(index, cmd, res);
    match deadline {
        None => fut.await,
        Some(deadline) => tokio::time::timeout_at(deadline, fut)
            .await
            .unwrap_or_else(|_| {
                Err(StepFailure {
                    exit: ExitKind::Runtime,
                    reason: Some("run_timeout"),
                    message: "--timeout exceeded; aborting the repl session".into(),
                    detail: None,
                })
            }),
    }
}

async fn write_result(
    state: &mut ReplState<'_>,
    out: &mut impl Write,
    index: usize,
    input_line: usize,
    cmd: &Command,
    elapsed: Duration,
    result: Result<Option<serde_json::Value>, StepFailure>,
) -> CommandControl {
    let (status, detail, failure, stop_session, ok) = match result {
        Ok(detail) => (CommandStatus::Ok, detail, None, false, true),
        Err(fail) => {
            let reason = fail.reason.unwrap_or_else(|| fail.exit.reason());
            let failure = serde_json::json!({
                "reason": reason,
                "message": fail.message,
            });
            let stop_session = state.strict || reason == "run_timeout";
            if stop_session {
                state.exit = fail.exit;
                state.report.set_failure(
                    Some(index),
                    reason,
                    failure["message"].as_str().unwrap_or(""),
                );
            }
            (
                CommandStatus::Failed,
                fail.detail,
                Some(failure),
                stop_session,
                false,
            )
        }
    };

    if let Some(engine) = state.engine.as_mut() {
        engine.record_marker(index, cmd, ok, elapsed);
    }
    state
        .report
        .record(index, cmd, status, elapsed, detail.clone());
    if status == CommandStatus::Ok
        && let Some(engine) = state.engine.as_mut()
    {
        engine.record_golden_frame().await;
    }

    if stop_session
        && !state.final_forensics_written
        && let Some(engine) = state.engine.as_mut()
    {
        engine.write_forensics();
        state.final_forensics_written = true;
    }

    let response = command_response(index, input_line, cmd, status, elapsed, detail, failure);
    CommandControl {
        wrote_response: write_json_line(out, &response),
        stop_session,
    }
}

fn should_fail_when_child_exited(cmd: &Command) -> bool {
    !matches!(
        cmd.command_type,
        TokenType::Set | TokenType::Output | TokenType::Require | TokenType::Env
    )
}

fn command_response(
    index: usize,
    input_line: usize,
    cmd: &Command,
    status: CommandStatus,
    elapsed: Duration,
    detail: Option<serde_json::Value>,
    failure: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut v = serde_json::json!({
        "kind": "command",
        "index": index,
        "input_line": input_line,
        "command": cmd.to_string(),
        "status": status_name(status),
        "elapsed_ms": elapsed.as_millis() as u64,
    });
    if let Some(detail) = detail {
        v["detail"] = detail;
    }
    if let Some(failure) = failure {
        v["failure"] = failure;
    }
    v
}

fn status_name(status: CommandStatus) -> &'static str {
    match status {
        CommandStatus::Ok => "ok",
        CommandStatus::Failed => "failed",
        CommandStatus::Skipped => "skipped",
    }
}

fn parse_error_response(input_line: usize, errors: &[ParseError]) -> serde_json::Value {
    serde_json::json!({
        "kind": "parse_error",
        "input_line": input_line,
        "errors": parse_errors_json(errors),
    })
}

fn runtime_error_response(input_line: usize, message: String) -> serde_json::Value {
    serde_json::json!({
        "kind": "runtime_error",
        "input_line": input_line,
        "failure": {
            "reason": "runtime_error",
            "message": message,
        }
    })
}

fn parse_errors_json(errors: &[ParseError]) -> Vec<serde_json::Value> {
    errors
        .iter()
        .map(|e| {
            serde_json::json!({
                "line": e.token.line,
                "col": e.token.column,
                "message": e.msg,
            })
        })
        .collect()
}

fn internal_fail(message: &str) -> StepFailure {
    StepFailure {
        exit: ExitKind::Runtime,
        reason: None,
        message: format!("internal error: {message}"),
        detail: None,
    }
}

async fn finish(mut state: ReplState<'_>, out: &mut impl Write) -> i32 {
    if state.exit != ExitKind::Success
        && !state.final_forensics_written
        && let Some(engine) = state.engine.as_mut()
    {
        engine.write_forensics();
        state.final_forensics_written = true;
    }

    if let Some(engine) = state.engine.take() {
        engine
            .finish(&state.outputs, &mut state.report, &mut state.exit)
            .await;
    }

    let report = state.report.finish(state.exit);
    let mut value = serde_json::to_value(&report).unwrap_or_else(|e| {
        serde_json::json!({
            "version": 1,
            "status": "runtime_error",
            "exit_code": ExitKind::Runtime as i32,
            "failure": {
                "reason": "runtime_error",
                "message": format!("report serialization failed: {e}"),
            }
        })
    });
    value["kind"] = "report".into();
    let _ = write_json_line(out, &value);
    report.exit_code
}

fn write_json_line(out: &mut impl Write, value: &serde_json::Value) -> bool {
    if let Err(e) = serde_json::to_writer(&mut *out, value) {
        eprintln!("vhs-rs: failed to write repl response: {e}");
        return false;
    }
    if let Err(e) = writeln!(out) {
        eprintln!("vhs-rs: failed to write repl newline: {e}");
        return false;
    }
    if let Err(e) = out.flush() {
        eprintln!("vhs-rs: failed to flush repl response: {e}");
        return false;
    }
    true
}
