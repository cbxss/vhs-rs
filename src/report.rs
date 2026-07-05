//! The machine-readable run report — the core of vhs_rs's agent contract.
//!
//! One JSON object describing everything that happened in a run: per-command
//! status and timing, Wait/Assert outcomes with the actual screen text at
//! check time, every artifact produced, and the failure (if any). An agent
//! consumes this instead of parsing human prose. Written to stdout with
//! `--json`, assembled incrementally so it survives mid-run failures.

use crate::command::Command;
use crate::error::ExitKind;
use serde::Serialize;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Ok,
    Failed,
    Skipped,
}

#[derive(Debug, Serialize)]
pub struct CommandRecord {
    pub index: usize,
    pub line: usize,
    pub col: usize,
    /// Human-readable rendering of the command, e.g. `Assert Screen foo`.
    pub command: String,
    pub status: CommandStatus,
    pub elapsed_ms: u64,
    /// Command-specific detail: Wait/Assert include {regex, scope, matched,
    /// screen_text (on failure)}; artifacts include their path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Gif,
    Png,
    Text,
    Golden,
    Cast,
    FailureText,
    FailurePng,
}

#[derive(Debug, Serialize)]
pub struct Artifact {
    pub path: String,
    pub kind: ArtifactKind,
    /// Index of the command that produced it (None for end-of-run outputs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_index: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct Failure {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_index: Option<usize>,
    /// Machine-stable reason: assert_failed | wait_timeout | runtime_error |
    /// child_exited | parse_error.
    pub reason: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct TermInfo {
    pub cols: usize,
    pub rows: usize,
    pub shell: String,
}

#[derive(Debug, Serialize)]
pub struct RunReport {
    pub version: u32,
    pub tape: String,
    /// success | assert_failed | wait_timeout | runtime_error | parse_error
    pub status: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub term: Option<TermInfo>,
    pub commands: Vec<CommandRecord>,
    pub artifacts: Vec<Artifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
}

/// Incrementally assembles a `RunReport` during evaluation.
pub struct ReportBuilder {
    tape: String,
    started: Instant,
    term: Option<TermInfo>,
    commands: Vec<CommandRecord>,
    artifacts: Vec<Artifact>,
    failure: Option<Failure>,
}

impl ReportBuilder {
    pub fn new(tape: impl Into<String>) -> Self {
        ReportBuilder {
            tape: tape.into(),
            started: Instant::now(),
            term: None,
            commands: Vec::new(),
            artifacts: Vec::new(),
            failure: None,
        }
    }

    pub fn set_term(&mut self, cols: usize, rows: usize, shell: impl Into<String>) {
        self.term = Some(TermInfo {
            cols,
            rows,
            shell: shell.into(),
        });
    }

    /// Records an executed command. `detail` carries per-command structure
    /// (Wait elapsed/matched, Assert screen text on failure, artifact paths).
    pub fn record(
        &mut self,
        index: usize,
        cmd: &Command,
        status: CommandStatus,
        elapsed: Duration,
        detail: Option<serde_json::Value>,
    ) {
        self.commands.push(CommandRecord {
            index,
            line: cmd.token.line,
            col: cmd.token.column,
            command: cmd.to_string(),
            status,
            elapsed_ms: elapsed.as_millis() as u64,
            detail,
        });
    }

    pub fn add_artifact(
        &mut self,
        path: impl Into<String>,
        kind: ArtifactKind,
        command_index: Option<usize>,
    ) {
        self.artifacts.push(Artifact {
            path: path.into(),
            kind,
            command_index,
        });
    }

    pub fn set_failure(
        &mut self,
        command_index: Option<usize>,
        reason: impl Into<String>,
        message: impl Into<String>,
    ) {
        // First failure wins; later teardown errors don't mask the root cause.
        if self.failure.is_none() {
            self.failure = Some(Failure {
                command_index,
                reason: reason.into(),
                message: message.into(),
            });
        }
    }

    pub fn has_failure(&self) -> bool {
        self.failure.is_some()
    }

    pub fn finish(self, exit: ExitKind) -> RunReport {
        let status = match exit {
            ExitKind::Success => "success",
            ExitKind::AssertFailed => "assert_failed",
            ExitKind::Parse => "parse_error",
            ExitKind::WaitTimeout => "wait_timeout",
            ExitKind::Runtime => "runtime_error",
        };
        RunReport {
            version: 1,
            tape: self.tape,
            status: status.into(),
            exit_code: exit as i32,
            duration_ms: self.started.elapsed().as_millis() as u64,
            term: self.term,
            commands: self.commands,
            artifacts: self.artifacts,
            failure: self.failure,
        }
    }
}

impl RunReport {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|e| {
            format!(
                "{{\"version\":1,\"status\":\"runtime_error\",\"error\":\"report serialization failed: {}\"}}",
                e
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::{Token, TokenType};

    #[test]
    fn report_shape() {
        let mut b = ReportBuilder::new("demo.tape");
        b.set_term(80, 24, "bash");
        let cmd = Command::new(
            TokenType::Assert,
            Token {
                token_type: TokenType::Assert,
                literal: "Assert".into(),
                line: 3,
                column: 1,
            },
        );
        b.record(
            0,
            &cmd,
            CommandStatus::Failed,
            Duration::from_millis(12),
            Some(serde_json::json!({"regex": "foo", "scope": "screen", "matched": false})),
        );
        b.set_failure(Some(0), "assert_failed", "Assert /foo/ did not match");
        b.set_failure(Some(9), "runtime_error", "should not overwrite");
        let report = b.finish(ExitKind::AssertFailed);

        let v: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["status"], "assert_failed");
        assert_eq!(v["exit_code"], 1);
        assert_eq!(v["term"]["cols"], 80);
        assert_eq!(v["commands"][0]["line"], 3);
        assert_eq!(v["commands"][0]["status"], "failed");
        assert_eq!(v["failure"]["reason"], "assert_failed");
        assert_eq!(v["failure"]["command_index"], 0);
    }
}
