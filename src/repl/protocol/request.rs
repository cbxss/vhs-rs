//! Typed requests and validation before execution.

use crate::command::Command;
use crate::token::{Token, TokenType};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub(super) id: u64,
    pub(super) command: Operation,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Operation {
    Type {
        text: String,
        speed_ms: Option<u32>,
    },
    Press {
        key: String,
        count: Option<u16>,
    },
    Wait {
        pattern: Option<String>,
        scope: Option<MatchScope>,
        timeout_ms: Option<u32>,
    },
    Assert {
        pattern: String,
        scope: Option<MatchScope>,
        timeout_ms: Option<u32>,
    },
    Screen,
    Screenshot {
        path: String,
    },
    Capture {
        path: String,
    },
    Output {
        path: String,
    },
    Configure {
        settings: Configuration,
    },
    Close,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum MatchScope {
    Line,
    Screen,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Configuration {
    shell: Option<String>,
    typing_speed_ms: Option<u32>,
    wait_timeout_ms: Option<u32>,
    width: Option<u16>,
    height: Option<u16>,
    font_size: Option<u16>,
    theme: Option<String>,
}

fn command(kind: TokenType, args: String, options: String) -> Command {
    let mut cmd = Command::new(
        kind,
        Token {
            token_type: kind,
            literal: kind.to_string(),
            line: 1,
            column: 1,
        },
    );
    cmd.args = args;
    cmd.options = options;
    cmd
}

fn duration(ms: Option<u32>) -> String {
    ms.map(|ms| format!("{ms}ms")).unwrap_or_default()
}

pub(super) fn commands(op: Operation) -> Result<Vec<Command>, String> {
    use TokenType::*;
    let cmd = match op {
        Operation::Type { text, speed_ms } => command(Type, text, duration(speed_ms)),
        Operation::Press { key, count } => {
            if count == Some(0) {
                return Err("count must be positive".into());
            }
            // Parse only a bounded key/chord spelling, never arbitrary tape input.
            if key.len() > 64
                || !key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+[]\\^_./?-".contains(c))
            {
                return Err("invalid key spelling".into());
            }
            let (mut parsed, errors) = crate::parse_tape(&key);
            if !errors.is_empty() || parsed.len() != 1 {
                return Err("invalid key or chord".into());
            }
            let mut cmd = parsed.remove(0);
            if !matches!(
                cmd.command_type,
                Enter
                    | Space
                    | Backspace
                    | Delete
                    | Insert
                    | Escape
                    | Tab
                    | Down
                    | Left
                    | Right
                    | Up
                    | PageUp
                    | PageDown
                    | Home
                    | End
                    | ScrollUp
                    | ScrollDown
                    | Ctrl
                    | Alt
                    | Shift
            ) {
                return Err("expected a key or chord".into());
            }
            if matches!(cmd.command_type, Ctrl | Alt | Shift) {
                if count.unwrap_or(1) != 1 {
                    return Err("chords do not support count".into());
                }
            } else {
                cmd.args = count.unwrap_or(1).to_string();
            }
            cmd
        }
        Operation::Wait {
            pattern,
            scope,
            timeout_ms,
        } => {
            let scope = scope.unwrap_or(if pattern.is_some() {
                MatchScope::Screen
            } else {
                MatchScope::Line
            });
            match_command(Wait, pattern, scope, timeout_ms)?
        }
        Operation::Assert {
            pattern,
            scope,
            timeout_ms,
        } => match_command(
            Assert,
            Some(pattern),
            scope.unwrap_or(MatchScope::Screen),
            timeout_ms,
        )?,
        Operation::Screen => command(Screen, Default::default(), Default::default()),
        Operation::Screenshot { path } => path_command(Screenshot, path)?,
        Operation::Capture { path } => path_command(Capture, path)?,
        Operation::Output { path } => {
            let mut cmd = path_command(Output, path)?;
            let ext = std::path::Path::new(&cmd.args)
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if !matches!(
                ext,
                "gif" | "png" | "txt" | "ascii" | "test" | "cast" | "jsonl"
            ) {
                return Err("unsupported output extension".into());
            }
            cmd.options = format!(".{ext}");
            cmd
        }
        Operation::Configure { settings } => return configuration(settings),
        Operation::Close => return Ok(Vec::new()),
    };
    Ok(vec![cmd])
}

fn path_command(kind: TokenType, path: String) -> Result<Command, String> {
    if path.is_empty() || path.contains('\0') {
        return Err("path must be nonempty and contain no NUL".into());
    }
    Ok(command(kind, path, String::new()))
}

fn match_command(
    kind: TokenType,
    pattern: Option<String>,
    scope: MatchScope,
    timeout: Option<u32>,
) -> Result<Command, String> {
    if let Some(pattern) = &pattern {
        regex::Regex::new(pattern).map_err(|e| e.to_string())?;
    }
    let scope = match scope {
        MatchScope::Line => "Line",
        MatchScope::Screen => "Screen",
    };
    let args = pattern.map_or_else(|| scope.into(), |p| format!("{scope} {p}"));
    Ok(command(kind, args, duration(timeout)))
}

fn configuration(settings: Configuration) -> Result<Vec<Command>, String> {
    let mut result = Vec::new();
    let mut set =
        |key: &str, value: String| result.push(command(TokenType::Set, value, key.into()));
    if let Some(shell) = settings.shell {
        if shell.is_empty() || shell.contains(['\0', '\n', '\r']) {
            return Err("invalid shell".into());
        }
        set("Shell", shell);
    }
    if let Some(ms) = settings.typing_speed_ms {
        set("TypingSpeed", duration(Some(ms)));
    }
    if let Some(ms) = settings.wait_timeout_ms {
        set("WaitTimeout", duration(Some(ms)));
    }
    // Bound allocations before constructing a renderer.
    for (key, value, min, max) in [
        ("Width", settings.width, 200, 4096),
        ("Height", settings.height, 200, 4096),
        ("FontSize", settings.font_size, 8, 128),
    ] {
        if let Some(value) = value {
            if !(min..=max).contains(&value) {
                return Err(format!("{key} must be between {min} and {max}"));
            }
            set(key, value.to_string());
        }
    }
    if let Some(theme) = settings.theme {
        if crate::theme::load_builtin(&theme).is_none() {
            return Err(format!("unknown theme {theme:?}"));
        }
        set("Theme", theme);
    }
    Ok(result)
}
