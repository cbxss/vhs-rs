//! Version 1 SDK transport. One JSON request produces one result; execution
//! is shared with the tape REPL. EOF means the client has departed (use close
//! to drain and finalize normally).

use super::*;
use serde_json::{Value, json};
use tokio::sync::watch;

mod request;
use request::{Operation, Request, commands};

const MAX_REQUEST: usize = 1024 * 1024;
const MAX_ID: u64 = 9_007_199_254_740_991;

// A bounded blocking reader keeps stdin compatible with both pipes and files.
// The separate EOF notification interrupts an active wait when its owner dies.
fn input() -> (
    mpsc::Receiver<Result<String, String>>,
    watch::Receiver<bool>,
) {
    let (tx, rx) = mpsc::channel(8);
    let (gone_tx, gone_rx) = watch::channel(false);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        loop {
            let mut bytes = Vec::new();
            let mut limited = std::io::Read::take(&mut reader, (MAX_REQUEST + 1) as u64);
            match limited.read_until(b'\n', &mut bytes) {
                Ok(0) => break,
                Ok(_) if bytes.len() > MAX_REQUEST => {
                    let _ = tx.blocking_send(Err("request exceeds 1 MiB".into()));
                    break;
                }
                Ok(_) => {
                    let line = String::from_utf8(bytes).map_err(|e| e.to_string());
                    if tx.blocking_send(line).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e.to_string()));
                    break;
                }
            }
        }
        let _ = gone_tx.send(true);
    });
    (rx, gone_rx)
}

async fn departed(gone: &mut watch::Receiver<bool>) {
    if *gone.borrow() {
        return;
    }
    let _ = gone.changed().await;
}

fn failure(id: Value, reason: &str, message: impl Into<String>) -> Value {
    json!({"kind":"result", "id":id, "status":"failed", "failure":{"reason":reason,"message":message.into()}})
}

pub(super) async fn run(req: &ReplRequest) -> i32 {
    let mut out = std::io::stdout().lock();
    if !write_json_line(
        &mut out,
        &json!({"kind":"ready","version":1,"binary_version":env!("CARGO_PKG_VERSION")}),
    ) {
        return 4;
    }
    let mut state = ReplState::new(req);
    let (mut lines, mut gone) = input();
    let mut sigterm = signal(SignalKind::terminate()).ok();
    let mut sigint = signal(SignalKind::interrupt()).ok();
    let deadline = req.timeout.map(|d| tokio::time::Instant::now() + d);
    let mut last_id = 0;
    let mut close_id = None;
    loop {
        let active = state.engine.as_ref().is_some_and(|e| !e.exited());
        let next = tokio::select! {
            line = lines.recv() => Next::Line(line),
            changed = wait_pty(state.engine.as_mut()), if active => Next::Pty(changed),
            _ = wait_deadline(deadline), if deadline.is_some() => Next::Timeout,
            _ = recv_signal(&mut sigterm) => Next::Signal("SIGTERM"),
            _ = recv_signal(&mut sigint) => Next::Signal("SIGINT"),
        };
        let line = match next {
            Next::Line(Some(Ok(line))) => line,
            Next::Pty(Ok(_)) => continue,
            Next::Line(None) => {
                stop(&mut state, "interrupted", "client disconnected");
                break;
            }
            Next::Line(Some(Err(e))) => {
                write_json_line(&mut out, &failure(Value::Null, "protocol_error", &e));
                stop(&mut state, "protocol_error", &e);
                break;
            }
            Next::Pty(Err(e)) => {
                stop(&mut state, "runtime_error", &e.to_string());
                break;
            }
            Next::Timeout => {
                stop(&mut state, "run_timeout", "session timeout");
                break;
            }
            Next::Signal(sig) => {
                stop(&mut state, "interrupted", sig);
                break;
            }
        };
        let raw: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(e) => {
                if !write_json_line(
                    &mut out,
                    &failure(Value::Null, "invalid_request", e.to_string()),
                ) {
                    return 4;
                }
                continue;
            }
        };
        let id = raw.get("id").cloned().unwrap_or(Value::Null);
        let request: Request = match serde_json::from_value(raw) {
            Ok(request) => request,
            Err(e) => {
                if !write_json_line(&mut out, &failure(id, "invalid_request", e.to_string())) {
                    return 4;
                }
                continue;
            }
        };
        if request.id <= last_id || request.id > MAX_ID {
            if !write_json_line(
                &mut out,
                &failure(
                    id,
                    "invalid_request",
                    "IDs must be increasing positive safe integers",
                ),
            ) {
                return 4;
            }
            continue;
        }
        last_id = request.id;
        if matches!(request.command, Operation::Close) {
            close_id = Some(request.id);
            break;
        }
        if matches!(request.command, Operation::Configure { .. }) && state.engine.is_some() {
            if !write_json_line(
                &mut out,
                &failure(
                    id,
                    "invalid_request",
                    "configure must precede session actions",
                ),
            ) {
                return 4;
            }
            continue;
        }
        let cmds = match commands(request.command) {
            Ok(cmds) => cmds,
            Err(e) => {
                if !write_json_line(&mut out, &failure(id, "invalid_request", e)) {
                    return 4;
                }
                continue;
            }
        };
        let resolved = match resolve_commands(&cmds) {
            Ok(resolved) => resolved,
            Err(e) => {
                if !write_json_line(&mut out, &failure(id, "invalid_request", e)) {
                    return 4;
                }
                continue;
            }
        };
        let mut events = Vec::new();
        let mut interrupted = None;
        for (cmd, res) in cmds.iter().zip(&resolved) {
            let outcome = tokio::select! {
                result = handle_command(&mut state, &mut events, 1, cmd, res, deadline) => { if result.is_none() { Some(("protocol_error", "could not encode command result")) } else { None } },
                _ = departed(&mut gone) => Some(("interrupted", "client disconnected")),
                _ = recv_signal(&mut sigterm) => Some(("interrupted", "SIGTERM")),
                _ = recv_signal(&mut sigint) => Some(("interrupted", "SIGINT")),
                _ = wait_deadline(deadline), if deadline.is_some() => Some(("run_timeout", "session timeout")),
            };
            if let Some((reason, message)) = outcome {
                stop(&mut state, reason, message);
                interrupted = Some(failure(id.clone(), reason, message));
                break;
            }
            if state.exit != ExitKind::Success {
                break;
            }
        }
        let response = match interrupted {
            Some(response) => response,
            None => match command_result(id.clone(), &events) {
                Ok(response) => response,
                Err(error) => {
                    stop(&mut state, "protocol_error", &error.to_string());
                    failure(id, "protocol_error", error.to_string())
                }
            },
        };
        if !write_json_line(&mut out, &response) {
            return 4;
        }
        if state.exit != ExitKind::Success {
            break;
        }
    }
    if let Some(id) = close_id {
        let mut bytes = Vec::new();
        let code = finish(state, &mut bytes).await;
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(report) => {
                let response = json!({"kind":"result","id":id,"status":if code == 0 {"ok"} else {"failed"},"failure":report.get("failure"),"report":report});
                if !write_json_line(&mut out, &response) {
                    return 4;
                }
            }
            Err(e) => {
                write_json_line(
                    &mut out,
                    &failure(json!(id), "protocol_error", e.to_string()),
                );
                return 4;
            }
        }
        code
    } else {
        finish(state, &mut out).await
    }
}

fn stop(state: &mut ReplState<'_>, reason: &str, message: &str) {
    state.exit = ExitKind::Runtime;
    state.report.set_failure(None, reason, message);
}

fn command_result(id: Value, events: &[u8]) -> Result<Value, serde_json::Error> {
    let records = events
        .split(|b| *b == b'\n')
        .filter(|s| !s.is_empty())
        .map(serde_json::from_slice::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let mut response = json!({"kind":"result","id":id,"status":"ok","events":records});
    if let Some(failed) = records.iter().find(|e| e["status"] == "failed") {
        response["status"] = "failed".into();
        response["failure"] = failed["failure"].clone();
        if let Some(detail) = failed.get("detail") {
            response["detail"] = detail.clone();
        }
    } else if let Some(detail) = records.last().and_then(|e| e.get("detail")) {
        response["detail"] = detail.clone();
    }
    Ok(response)
}
