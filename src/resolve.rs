//! Typed post-parse resolution — the side table the evaluator executes from.
//!
//! The stringly [`Command`] stays untouched (VHS-port fidelity: the ported
//! test suites assert on its strings, and reports render from it). But the
//! evaluator used to re-parse those strings at execution time: re-splitting
//! Wait/Assert args, re-compiling regexes per execution, re-parsing durations
//! and repeat counts. This module lifts everything that was re-parsed into a
//! [`Resolved`] table built ONCE right after parse+validate, aligned with the
//! command list by index. Data that is used verbatim (chord characters, Set
//! values applied through `apply_setting`) is deliberately not duplicated
//! here — those arms get [`Resolved::Passthrough`].
//!
//! Resolution failures are internal errors: the parser already validated
//! every duration and regex, so any failure here means the two passes
//! disagree. Callers surface it as a runtime error, never a panic.

use crate::command::Command;
use crate::term::Term;
use crate::token::TokenType;
use crate::util::parse_duration;
use regex::Regex;
use std::time::Duration;

/// What a Wait/Assert looks at: the current line or the whole screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Line,
    Screen,
}

impl Scope {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Line => "Line",
            Self::Screen => "Screen",
        }
    }

    pub fn text(&self, term: &Term) -> String {
        match self {
            Self::Line => term.current_line(),
            Self::Screen => term.text(),
        }
    }
}

/// Typed execution data for one command, index-aligned with the command list.
#[derive(Debug)]
pub enum Resolved {
    /// Repeatable key tokens (Enter, arrows, Home/End, ScrollUp/Down, ...).
    /// `speed: None` means the runtime TypingSpeed setting.
    Keypress {
        speed: Option<Duration>,
        count: usize,
    },
    /// `Type[@speed] "text"`.
    TypeText {
        speed: Option<Duration>,
        text: String,
    },
    /// `Copy "text"`.
    CopyText { text: String },
    /// `Sleep <duration>`.
    Sleep { duration: Duration },
    /// Wait and Assert. `regex: None` falls back to the runtime WaitPattern
    /// setting; `timeout: None` means the default WaitTimeout (Wait) or an
    /// immediate, non-retrying check (Assert).
    WaitLike {
        scope: Scope,
        regex: Option<Regex>,
        timeout: Option<Duration>,
    },
    /// End-of-run `Output` target: extension (as parsed, e.g. `.gif`) + path.
    OutputTarget { ext: String, path: String },
    /// Mid-run path command: Screenshot / Capture.
    PathCommand { path: String },
    /// Commands whose strings are used verbatim or not at all at execution
    /// time (chords, Set, Env, Hide/Show, Paste, ...).
    Passthrough,
}

/// Builds the side table for a validated command list. `Err` carries an
/// internal-error message (post-validate, nothing here can legitimately
/// fail).
///
/// # Errors
/// Returns a message describing the malformed command — reachable only if
/// resolution drifts out of sync with parse/validate (an internal bug, not
/// a user error).
pub fn resolve_commands(commands: &[Command]) -> Result<Vec<Resolved>, String> {
    commands.iter().map(resolve_one).collect()
}

fn resolve_one(cmd: &Command) -> Result<Resolved, String> {
    use TokenType::*;
    Ok(match cmd.command_type {
        Type => Resolved::TypeText {
            speed: optional_duration(cmd)?,
            text: cmd.args.clone(),
        },
        Copy => Resolved::CopyText {
            text: cmd.args.clone(),
        },
        Enter | Space | Backspace | Delete | Insert | Escape | Tab | Down | Left | Right | Up
        | PageUp | PageDown | Home | End | ScrollUp | ScrollDown => Resolved::Keypress {
            speed: optional_duration(cmd)?,
            count: cmd.args.parse().unwrap_or(1),
        },
        Sleep => Resolved::Sleep {
            duration: parse_duration(&cmd.args)
                .ok_or_else(|| internal(cmd, format!("bad duration {:?}", cmd.args)))?,
        },
        Wait | Assert => {
            // Parser convention: args is "<Scope>" or "<Scope> <regex>", the
            // scope never contains a space.
            let (scope_str, pattern) = match cmd.args.split_once(' ') {
                Some((s, re)) => (s, Some(re)),
                None => (cmd.args.as_str(), None),
            };
            let scope = match scope_str {
                "Screen" => Scope::Screen,
                _ => Scope::Line,
            };
            let regex = match pattern {
                // NB: plain `Regex` here would be `TokenType::Regex` (glob
                // import above), hence the full path.
                Some(re) => Some(
                    regex::Regex::new(re)
                        .map_err(|e| internal(cmd, format!("invalid regex /{re}/: {e}")))?,
                ),
                None => None,
            };
            Resolved::WaitLike {
                scope,
                regex,
                timeout: optional_duration(cmd)?,
            }
        }
        Screenshot | Capture => Resolved::PathCommand {
            path: cmd.args.clone(),
        },
        Output => Resolved::OutputTarget {
            ext: cmd.options.clone(),
            path: cmd.args.clone(),
        },
        _ => Resolved::Passthrough,
    })
}

/// The optional `@duration` suffix carried in `cmd.options` (keypress/Type
/// speed, Wait/Assert timeout). Empty means "use the runtime setting".
fn optional_duration(cmd: &Command) -> Result<Option<Duration>, String> {
    if cmd.options.is_empty() {
        Ok(None)
    } else {
        parse_duration(&cmd.options)
            .map(Some)
            .ok_or_else(|| internal(cmd, format!("bad duration {:?}", cmd.options)))
    }
}

fn internal(cmd: &Command, what: String) -> String {
    format!(
        "internal error: line {}: {what} (parser should have rejected this)",
        cmd.token.line
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::Token;

    /// Parses a single tape line and resolves it.
    fn resolve_line(src: &str) -> Resolved {
        let (cmds, errs) = crate::parse_tape(src);
        assert!(errs.is_empty(), "{src}: parse errors {errs:?}");
        assert_eq!(cmds.len(), 1, "{src}: expected exactly one command");
        resolve_commands(&cmds).unwrap().pop().unwrap()
    }

    /// Table: each command string form -> expected `Resolved` (via its Debug
    /// rendering, which pins every field including compiled regex patterns).
    #[test]
    fn resolution_table() {
        let cases: &[(&str, &str)] = &[
            // Type: optional speed, text verbatim.
            (
                r#"Type "hi there""#,
                r#"TypeText { speed: None, text: "hi there" }"#,
            ),
            (
                r#"Type@100ms "hi""#,
                r#"TypeText { speed: Some(100ms), text: "hi" }"#,
            ),
            (
                r#"Type@1s "hi""#,
                r#"TypeText { speed: Some(1s), text: "hi" }"#,
            ),
            // Keypresses: optional speed, repeat count (default 1).
            ("Enter", "Keypress { speed: None, count: 1 }"),
            ("Enter@50ms 3", "Keypress { speed: Some(50ms), count: 3 }"),
            ("Down 2", "Keypress { speed: None, count: 2 }"),
            ("Home", "Keypress { speed: None, count: 1 }"),
            ("End", "Keypress { speed: None, count: 1 }"),
            ("PageUp 4", "Keypress { speed: None, count: 4 }"),
            ("ScrollDown 5", "Keypress { speed: None, count: 5 }"),
            // Sleep: bare numbers are seconds (parser appends the unit).
            ("Sleep 2", "Sleep { duration: 2s }"),
            ("Sleep 500ms", "Sleep { duration: 500ms }"),
            // Wait: defaults to Line scope, WaitPattern fallback, no timeout.
            (
                "Wait",
                "WaitLike { scope: Line, regex: None, timeout: None }",
            ),
            (
                "Wait /foo/",
                r#"WaitLike { scope: Line, regex: Some(Regex("foo")), timeout: None }"#,
            ),
            (
                "Wait+Screen@5s /sp ace/",
                r#"WaitLike { scope: Screen, regex: Some(Regex("sp ace")), timeout: Some(5s) }"#,
            ),
            // Assert: defaults to Screen scope; regex is mandatory.
            (
                "Assert /x/",
                r#"WaitLike { scope: Screen, regex: Some(Regex("x")), timeout: None }"#,
            ),
            (
                "Assert+Line@1s /y/",
                r#"WaitLike { scope: Line, regex: Some(Regex("y")), timeout: Some(1s) }"#,
            ),
            // Copy / paths / outputs.
            (r#"Copy "clip""#, r#"CopyText { text: "clip" }"#),
            ("Screenshot shot.png", r#"PathCommand { path: "shot.png" }"#),
            ("Capture cap.txt", r#"PathCommand { path: "cap.txt" }"#),
            (
                "Output demo.gif",
                r#"OutputTarget { ext: ".gif", path: "demo.gif" }"#,
            ),
            (
                "Output golden.txt",
                r#"OutputTarget { ext: ".txt", path: "golden.txt" }"#,
            ),
            // Verbatim/no-data commands pass through.
            ("Hide", "Passthrough"),
            ("Show", "Passthrough"),
            ("Ctrl+C", "Passthrough"),
            ("Alt+x", "Passthrough"),
            ("Paste", "Passthrough"),
            (r#"Require bash"#, "Passthrough"),
            ("Set TypingSpeed 10ms", "Passthrough"),
        ];
        for (src, want) in cases {
            assert_eq!(&format!("{:?}", resolve_line(src)), want, "source: {src}");
        }
    }

    fn raw(tt: TokenType, options: &str, args: &str, line: usize) -> Command {
        let mut cmd = Command::new(
            tt,
            Token {
                token_type: tt,
                literal: tt.as_str().into(),
                line,
                column: 1,
            },
        );
        cmd.options = options.into();
        cmd.args = args.into();
        cmd
    }

    /// Residual failures (impossible from the parser) are internal errors
    /// with a position, not panics.
    #[test]
    fn residual_failures_are_internal_errors() {
        let err = resolve_commands(&[raw(TokenType::Sleep, "", "garbage", 7)]).unwrap_err();
        assert!(err.contains("internal error"), "{err}");
        assert!(err.contains("line 7"), "{err}");
        assert!(err.contains("bad duration"), "{err}");

        let err = resolve_commands(&[raw(TokenType::Enter, "nope", "1", 3)]).unwrap_err();
        assert!(err.contains("line 3"), "{err}");

        let err = resolve_commands(&[raw(TokenType::Wait, "", "Line [", 9)]).unwrap_err();
        assert!(err.contains("invalid regex"), "{err}");
    }

    /// The table is index-aligned with the command list.
    #[test]
    fn table_is_index_aligned() {
        let (cmds, errs) = crate::parse_tape("Output d.gif\nType \"x\"\nEnter\nWait\n");
        assert!(errs.is_empty());
        let resolved = resolve_commands(&cmds).unwrap();
        assert_eq!(resolved.len(), cmds.len());
        assert!(matches!(resolved[0], Resolved::OutputTarget { .. }));
        assert!(matches!(resolved[1], Resolved::TypeText { .. }));
        assert!(matches!(resolved[2], Resolved::Keypress { .. }));
        assert!(matches!(resolved[3], Resolved::WaitLike { .. }));
    }
}
