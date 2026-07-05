//! Error taxonomy and the caret-underline parse error renderer.
//!
//! Exit codes are part of vhs_rs's agent contract:
//! 0 success · 1 assert failure · 2 parse/validation error · 3 wait timeout ·
//! 4 runtime/IO error.

use crate::token::Token;
use std::fmt::Write as _;

/// Process exit codes, stable API for agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    Success = 0,
    AssertFailed = 1,
    Parse = 2,
    WaitTimeout = 3,
    Runtime = 4,
}

impl ExitKind {
    /// Machine-stable reason string, used both as the report `status` and as
    /// the failure `reason` — the single source of truth for the taxonomy.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::AssertFailed => "assert_failed",
            Self::Parse => "parse_error",
            Self::WaitTimeout => "wait_timeout",
            Self::Runtime => "runtime_error",
        }
    }
}

/// A parse or validation error anchored to a source token.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub token: Token,
    pub msg: String,
}

impl ParseError {
    pub fn new(token: Token, msg: impl Into<String>) -> Self {
        Self {
            token,
            msg: msg.into(),
        }
    }
}

impl std::fmt::Display for ParseError {
    // Matches VHS: fmt.Sprintf("%2d:%-2d │ %s", line, column, msg)
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:2}:{:<2} │ {}",
            self.token.line, self.token.column, self.msg
        )
    }
}

impl std::error::Error for ParseError {}

/// Number of columns the line-number gutter occupies (VHS's ErrorColumnOffset).
const ERROR_COLUMN_OFFSET: usize = 5;

/// Renders errors VHS-style: the offending source line, a caret underline
/// beneath the bad token, and the message.
///
/// ```text
///   4 │ Foo
///      ^^^ Invalid command: Foo
/// ```
pub fn render_parse_errors(tape: &str, errors: &[ParseError]) -> String {
    let lines: Vec<&str> = tape.split('\n').collect();
    let mut out = String::new();

    for err in errors {
        let src_line = lines.get(err.token.line.saturating_sub(1)).unwrap_or(&"");
        let _ = writeln!(out, " {:2} │ {}", err.token.line, src_line);
        let _ = writeln!(
            out,
            "{}{} {}",
            " ".repeat(err.token.column + ERROR_COLUMN_OFFSET),
            "^".repeat(err.token.literal.len().max(1)),
            err.msg
        );
        out.push('\n');
    }

    let _ = write!(out, "parser: {} error(s)", errors.len());
    out
}
