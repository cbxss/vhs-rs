//! Parser for the VHS-compatible tape language.
//!
//! Faithful port of vhs/parser/parser.go (two-token lookahead recursive
//! descent), extended with `Assert` and `Capture`, plus a `validate` pass that
//! rejects things VHS accepts but vterm cannot execute (video outputs,
//! mid-tape geometry changes) so `vterm check` catches them before a run.

use crate::command::Command;
use crate::error::ParseError;
use crate::lexer::Lexer;
use crate::token::{Token, TokenType, is_modifier, is_setting, lookup_identifier};
use crate::util::parse_duration;
use std::path::Path;

pub struct Parser<'a> {
    lexer: Lexer<'a>,
    errors: Vec<ParseError>,
    cur: Token,
    peek: Token,
}

impl<'a> Parser<'a> {
    pub fn new(input: &'a str) -> Self {
        let placeholder = Token {
            token_type: TokenType::Eof,
            literal: String::new(),
            line: 0,
            column: 0,
        };
        let mut p = Parser {
            lexer: Lexer::new(input),
            errors: Vec::new(),
            cur: placeholder.clone(),
            peek: placeholder,
        };
        // Read two tokens, so cur and peek are both set.
        p.next_token();
        p.next_token();
        p
    }

    /// Parses the input into a list of commands.
    pub fn parse(&mut self) -> Vec<Command> {
        let mut cmds = Vec::new();
        while self.cur.token_type != TokenType::Eof {
            if self.cur.token_type == TokenType::Comment {
                self.next_token();
                continue;
            }
            cmds.extend(self.parse_command());
            self.next_token();
        }
        cmds
    }

    pub fn errors(&self) -> &[ParseError] {
        &self.errors
    }

    pub fn into_errors(self) -> Vec<ParseError> {
        self.errors
    }

    fn next_token(&mut self) {
        self.cur = std::mem::replace(&mut self.peek, self.lexer.next_token());
    }

    fn error(&mut self, token: Token, msg: impl Into<String>) {
        self.errors.push(ParseError::new(token, msg));
    }

    fn parse_command(&mut self) -> Vec<Command> {
        use TokenType::*;
        match self.cur.token_type {
            Space | Backspace | Delete | Insert | Enter | Escape | Tab | Down | Left | Right
            | Up | PageUp | PageDown | ScrollUp | ScrollDown | Home | End => {
                vec![self.parse_keypress(self.cur.token_type)]
            }
            Set => vec![self.parse_set()],
            Output => vec![self.parse_output()],
            Sleep => vec![self.parse_sleep()],
            Type => vec![self.parse_type()],
            Ctrl => vec![self.parse_ctrl()],
            Alt => vec![self.parse_alt()],
            Shift => vec![self.parse_shift()],
            Hide => vec![Command::new(Hide, self.cur.clone())],
            Require => vec![self.parse_require()],
            Show => vec![Command::new(Show, self.cur.clone())],
            Wait => vec![self.parse_wait()],
            Source => self.parse_source(),
            Screenshot => vec![self.parse_screenshot()],
            Copy => vec![self.parse_copy()],
            Paste => vec![Command::new(Paste, self.cur.clone())],
            Env => vec![self.parse_env()],
            Assert => vec![self.parse_assert()],
            Capture => vec![self.parse_capture()],
            _ => {
                self.error(
                    self.cur.clone(),
                    format!("Invalid command: {}", self.cur.literal),
                );
                vec![Command::new(Illegal, self.cur.clone())]
            }
        }
    }

    /// `Wait[+Line|+Screen][@<timeout>] [/regex/]`
    fn parse_wait(&mut self) -> Command {
        let mut cmd = Command::new(TokenType::Wait, self.cur.clone());

        if self.peek.token_type == TokenType::Plus {
            self.next_token();
            if self.peek.token_type != TokenType::String
                || (self.peek.literal != "Line" && self.peek.literal != "Screen")
            {
                self.error(self.peek.clone(), "Wait+ expects Line or Screen");
                return cmd;
            }
            cmd.args = self.peek.literal.clone();
            self.next_token();
        } else {
            cmd.args = "Line".into();
        }

        cmd.options = self.parse_speed();
        if !cmd.options.is_empty() {
            match parse_duration(&cmd.options) {
                Some(d) if !d.is_zero() => {}
                _ => {
                    self.error(self.peek.clone(), "Wait expects positive duration");
                    return cmd;
                }
            }
        }

        if self.peek.token_type != TokenType::Regex {
            // Fall back to the default WaitPattern.
            return cmd;
        }
        self.next_token();
        if let Err(err) = regex::Regex::new(&self.cur.literal) {
            self.error(
                self.cur.clone(),
                format!(
                    "Invalid regular expression '{}': {}",
                    self.cur.literal,
                    one_line(&err.to_string())
                ),
            );
            return cmd;
        }

        cmd.args.push(' ');
        cmd.args.push_str(&self.cur.literal);
        cmd
    }

    /// `Assert[+Screen|+Line][@<timeout>] /regex/` — vterm extension.
    ///
    /// Default scope is Screen. Without a timeout the check is immediate; with
    /// one it retries event-driven until the deadline. The regex is required.
    fn parse_assert(&mut self) -> Command {
        let mut cmd = Command::new(TokenType::Assert, self.cur.clone());

        if self.peek.token_type == TokenType::Plus {
            self.next_token();
            if self.peek.token_type != TokenType::String
                || (self.peek.literal != "Line" && self.peek.literal != "Screen")
            {
                self.error(self.peek.clone(), "Assert+ expects Line or Screen");
                return cmd;
            }
            cmd.args = self.peek.literal.clone();
            self.next_token();
        } else {
            cmd.args = "Screen".into();
        }

        cmd.options = self.parse_speed();
        if !cmd.options.is_empty() {
            match parse_duration(&cmd.options) {
                Some(d) if !d.is_zero() => {}
                _ => {
                    self.error(self.peek.clone(), "Assert expects positive duration");
                    return cmd;
                }
            }
        }

        if self.peek.token_type != TokenType::Regex {
            self.error(self.cur.clone(), "Assert expects /regex/");
            return cmd;
        }
        self.next_token();
        if let Err(err) = regex::Regex::new(&self.cur.literal) {
            self.error(
                self.cur.clone(),
                format!(
                    "Invalid regular expression '{}': {}",
                    self.cur.literal,
                    one_line(&err.to_string())
                ),
            );
            return cmd;
        }

        cmd.args.push(' ');
        cmd.args.push_str(&self.cur.literal);
        cmd
    }

    /// `@<time>` — optional speed/timeout suffix.
    fn parse_speed(&mut self) -> String {
        if self.peek.token_type == TokenType::At {
            self.next_token();
            self.parse_time()
        } else {
            String::new()
        }
    }

    /// Optional repeat count, defaults to "1".
    fn parse_repeat(&mut self) -> String {
        if self.peek.token_type == TokenType::Number {
            let count = self.peek.literal.clone();
            self.next_token();
            count
        } else {
            "1".into()
        }
    }

    /// `<number>[ms|s|m]` — bare numbers default to seconds.
    fn parse_time(&mut self) -> String {
        let mut t;
        if self.peek.token_type == TokenType::Number {
            t = self.peek.literal.clone();
            self.next_token();
        } else {
            self.error(
                self.cur.clone(),
                format!("Expected time after {}", self.cur.literal),
            );
            return String::new();
        }

        if matches!(
            self.peek.token_type,
            TokenType::Milliseconds | TokenType::Seconds | TokenType::Minutes
        ) {
            t.push_str(&self.peek.literal);
            self.next_token();
        } else {
            t.push('s');
        }
        t
    }

    /// `Ctrl[+Alt][+Shift]+<char>` — modifiers must precede the key.
    fn parse_ctrl(&mut self) -> Command {
        let cmd_token = self.cur.clone();
        let mut args: Vec<String> = Vec::new();
        let mut in_modifier_chain = true;

        while self.peek.token_type == TokenType::Plus {
            self.next_token();
            let peek = self.peek.clone();

            if is_modifier(lookup_identifier(&peek.literal)) {
                if !in_modifier_chain {
                    self.error(
                        self.cur.clone(),
                        "Modifiers must come before other characters",
                    );
                    // Clear args so the error is returned.
                    args.clear();
                    self.next_token();
                    continue;
                }
                args.push(peek.literal);
                self.next_token();
                continue;
            }

            in_modifier_chain = false;

            use TokenType::*;
            match peek.token_type {
                Enter | Space | Backspace | Minus | At | LeftBracket | RightBracket | Caret
                | Backslash | Left | Right | Up | Down => args.push(peek.literal),
                String if peek.literal.len() == 1 => args.push(peek.literal),
                _ => {
                    self.error(self.cur.clone(), "Not a valid modifier");
                    self.error(
                        self.cur.clone(),
                        format!("Invalid control argument: {}", self.cur.literal),
                    );
                }
            }

            self.next_token();
        }

        if args.is_empty() {
            self.error(
                self.cur.clone(),
                format!(
                    "Expected control character with args, got {}",
                    self.cur.literal
                ),
            );
        }

        let mut cmd = Command::new(TokenType::Ctrl, cmd_token);
        cmd.args = args.join(" ");
        cmd
    }

    /// `Alt+<character>`
    fn parse_alt(&mut self) -> Command {
        let cmd_token = self.cur.clone();
        if self.peek.token_type == TokenType::Plus {
            self.next_token();
            if matches!(
                self.peek.token_type,
                TokenType::String
                    | TokenType::Enter
                    | TokenType::LeftBracket
                    | TokenType::RightBracket
                    | TokenType::Tab
            ) {
                let c = self.peek.literal.clone();
                self.next_token();
                let mut cmd = Command::new(TokenType::Alt, cmd_token);
                cmd.args = c;
                return cmd;
            }
        }

        self.error(
            self.cur.clone(),
            format!("Expected alt character, got {}", self.cur.literal),
        );
        Command::new(TokenType::Alt, cmd_token)
    }

    /// `Shift+<char>`
    fn parse_shift(&mut self) -> Command {
        let cmd_token = self.cur.clone();
        if self.peek.token_type == TokenType::Plus {
            self.next_token();
            if matches!(
                self.peek.token_type,
                TokenType::String
                    | TokenType::Enter
                    | TokenType::LeftBracket
                    | TokenType::RightBracket
                    | TokenType::Tab
            ) {
                let c = self.peek.literal.clone();
                self.next_token();
                let mut cmd = Command::new(TokenType::Shift, cmd_token);
                cmd.args = c;
                return cmd;
            }
        }

        self.error(
            self.cur.clone(),
            format!("Expected shift character, got {}", self.cur.literal),
        );
        Command::new(TokenType::Shift, cmd_token)
    }

    /// `Key[@<time>] [count]`
    fn parse_keypress(&mut self, t: TokenType) -> Command {
        let mut cmd = Command::new(t, self.cur.clone());
        cmd.options = self.parse_speed();
        cmd.args = self.parse_repeat();
        cmd
    }

    /// `Output <path>`
    fn parse_output(&mut self) -> Command {
        let mut cmd = Command::new(TokenType::Output, self.cur.clone());

        if self.peek.token_type != TokenType::String {
            self.error(self.cur.clone(), "Expected file path after output");
            return cmd;
        }

        let ext = Path::new(&self.peek.literal)
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        if !ext.is_empty() {
            cmd.options = ext;
        } else {
            cmd.options = ".png".into();
            if !self.peek.literal.ends_with('/') {
                self.error(self.peek.clone(), "Expected folder with trailing slash");
            }
        }

        cmd.args = self.peek.literal.clone();
        self.next_token();
        cmd
    }

    /// `Set <setting> <value>`
    fn parse_set(&mut self) -> Command {
        let mut cmd = Command::new(TokenType::Set, self.cur.clone());

        if is_setting(self.peek.token_type) {
            cmd.options = self.peek.literal.clone();
        } else {
            self.error(
                self.peek.clone(),
                format!("Unknown setting: {}", self.peek.literal),
            );
        }
        self.next_token();

        match self.cur.token_type {
            TokenType::WaitTimeout => {
                cmd.args = self.parse_time();
            }
            TokenType::WaitPattern => {
                cmd.args = self.peek.literal.clone();
                if regex::Regex::new(&self.peek.literal).is_err() {
                    self.error(
                        self.peek.clone(),
                        format!("Invalid regexp pattern: {}", self.peek.literal),
                    );
                }
                self.next_token();
            }
            TokenType::LoopOffset => {
                cmd.args = self.peek.literal.clone();
                self.next_token();
                // Allow LoopOffset without '%': Set LoopOffset 20
                cmd.args.push('%');
                if self.peek.token_type == TokenType::Percent {
                    self.next_token();
                }
            }
            TokenType::TypingSpeed => {
                cmd.args = self.peek.literal.clone();
                self.next_token();
                // Allow TypingSpeed to have bare units: Set TypingSpeed 10ms
                if matches!(
                    self.peek.token_type,
                    TokenType::Milliseconds | TokenType::Seconds
                ) {
                    cmd.args.push_str(&self.peek.literal);
                    self.next_token();
                } else if cmd.options == "TypingSpeed" {
                    cmd.args.push('s');
                }
            }
            TokenType::WindowBar => {
                cmd.args = self.peek.literal.clone();
                self.next_token();
                let window_bar = self.cur.literal.clone();
                if !is_valid_window_bar(&window_bar) {
                    self.error(
                        self.cur.clone(),
                        format!("{} is not a valid bar style.", window_bar),
                    );
                }
            }
            TokenType::MarginFill => {
                cmd.args = self.peek.literal.clone();
                self.next_token();
                let margin_fill = self.cur.literal.clone();
                // Check if margin color is a valid hex string.
                if let Some(hex) = margin_fill.strip_prefix('#') {
                    if u64::from_str_radix(hex, 16).is_err() || margin_fill.len() != 7 {
                        self.error(
                            self.cur.clone(),
                            format!("\"{}\" is not a valid color.", margin_fill),
                        );
                    }
                }
            }
            TokenType::CursorBlink => {
                cmd.args = self.peek.literal.clone();
                self.next_token();
                if self.cur.token_type != TokenType::Boolean {
                    self.error(self.cur.clone(), "expected boolean value.");
                }
            }
            _ => {
                cmd.args = self.peek.literal.clone();
                self.next_token();
            }
        }

        cmd
    }

    /// `Sleep <time>`
    fn parse_sleep(&mut self) -> Command {
        let mut cmd = Command::new(TokenType::Sleep, self.cur.clone());
        cmd.args = self.parse_time();
        cmd
    }

    /// `Require <binary>`
    fn parse_require(&mut self) -> Command {
        let mut cmd = Command::new(TokenType::Require, self.cur.clone());

        if self.peek.token_type != TokenType::String {
            self.error(
                self.peek.clone(),
                format!("{} expects one string", self.cur.literal),
            );
        }

        cmd.args = self.peek.literal.clone();
        self.next_token();
        cmd
    }

    /// `Type[@<time>] "string"...`
    fn parse_type(&mut self) -> Command {
        let cmd_token = self.cur.clone();
        let mut cmd = Command::new(TokenType::Type, cmd_token);

        cmd.options = self.parse_speed();

        if self.peek.token_type != TokenType::String {
            self.error(
                self.peek.clone(),
                format!("{} expects string", self.cur.literal),
            );
        }

        while self.peek.token_type == TokenType::String {
            self.next_token();
            cmd.args.push_str(&self.cur.literal);
            // Adjacent string literals are joined with a single space; tokens
            // must be whitespace-separated, so this is what the user intended.
            if self.peek.token_type == TokenType::String {
                cmd.args.push(' ');
            }
        }

        cmd
    }

    /// `Copy "string"...`
    fn parse_copy(&mut self) -> Command {
        let mut cmd = Command::new(TokenType::Copy, self.cur.clone());

        if self.peek.token_type != TokenType::String {
            self.error(
                self.peek.clone(),
                format!("{} expects string", self.cur.literal),
            );
        }
        while self.peek.token_type == TokenType::String {
            self.next_token();
            cmd.args.push_str(&self.cur.literal);
            if self.peek.token_type == TokenType::String {
                cmd.args.push(' ');
            }
        }
        cmd
    }

    /// `Env KEY "value"`
    fn parse_env(&mut self) -> Command {
        let mut cmd = Command::new(TokenType::Env, self.cur.clone());

        cmd.options = self.peek.literal.clone();
        self.next_token();

        if self.peek.token_type != TokenType::String {
            self.error(
                self.peek.clone(),
                format!("{} expects string", self.cur.literal),
            );
        }

        cmd.args = self.peek.literal.clone();
        self.next_token();
        cmd
    }

    /// `Source <path>.tape` — inlines the referenced tape (one level deep;
    /// `Source` and `Output` commands inside it are filtered out).
    fn parse_source(&mut self) -> Vec<Command> {
        let cmd = Command::new(TokenType::Source, self.cur.clone());

        if self.peek.token_type != TokenType::String {
            self.error(self.cur.clone(), "Expected path after Source");
            self.next_token();
            return vec![cmd];
        }

        let src_path = self.peek.literal.clone();

        if Path::new(&src_path)
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            != Some("tape".into())
        {
            self.error(self.peek.clone(), "Expected file with .tape extension");
            self.next_token();
            return vec![cmd];
        }

        if !Path::new(&src_path).exists() {
            self.error(self.peek.clone(), format!("File {} not found", src_path));
            self.next_token();
            return vec![cmd];
        }

        let src_tape = match std::fs::read_to_string(&src_path) {
            Ok(s) => s,
            Err(_) => {
                self.error(
                    self.peek.clone(),
                    format!("Unable to read file: {}", src_path),
                );
                self.next_token();
                return vec![cmd];
            }
        };

        if src_tape.is_empty() {
            self.error(
                self.peek.clone(),
                format!("Source tape: {} is empty", src_path),
            );
            self.next_token();
            return vec![cmd];
        }

        let mut src_parser = Parser::new(&src_tape);
        let src_cmds = src_parser.parse();

        // No nested Source.
        if src_cmds.iter().any(|c| c.command_type == TokenType::Source) {
            self.error(self.peek.clone(), "Nested Source detected");
            self.next_token();
            return vec![cmd];
        }

        let src_errors = src_parser.errors();
        if !src_errors.is_empty() {
            self.error(
                self.peek.clone(),
                format!("{} has {} errors", src_path, src_errors.len()),
            );
            self.next_token();
            return vec![cmd];
        }

        let filtered: Vec<Command> = src_cmds
            .into_iter()
            .filter(|c| {
                // Output is filtered to avoid overwriting the parent tape's output.
                c.command_type != TokenType::Source && c.command_type != TokenType::Output
            })
            .map(|mut c| {
                c.source = src_path.clone();
                c
            })
            .collect();

        self.next_token();
        filtered
    }

    /// `Screenshot <path>.png`
    fn parse_screenshot(&mut self) -> Command {
        let mut cmd = Command::new(TokenType::Screenshot, self.cur.clone());

        if self.peek.token_type != TokenType::String {
            self.error(self.cur.clone(), "Expected path after Screenshot");
            self.next_token();
            return cmd;
        }

        let path = self.peek.literal.clone();
        if !path.ends_with(".png") {
            self.error(self.peek.clone(), "Expected file with .png extension");
            self.next_token();
            return cmd;
        }

        cmd.args = path;
        self.next_token();
        cmd
    }

    /// `Capture <path>.txt` — vterm extension: dump the screen as plain text.
    fn parse_capture(&mut self) -> Command {
        let mut cmd = Command::new(TokenType::Capture, self.cur.clone());

        if self.peek.token_type != TokenType::String {
            self.error(self.cur.clone(), "Expected path after Capture");
            self.next_token();
            return cmd;
        }

        let path = self.peek.literal.clone();
        if !path.ends_with(".txt") {
            self.error(self.peek.clone(), "Expected file with .txt extension");
            self.next_token();
            return cmd;
        }

        cmd.args = path;
        self.next_token();
        cmd
    }
}

fn is_valid_window_bar(w: &str) -> bool {
    matches!(
        w,
        "" | "Colorful" | "ColorfulRight" | "Rings" | "RingsRight"
    )
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Output extensions vterm can produce.
const SUPPORTED_OUTPUTS: &[&str] = &[".gif", ".png", ".txt", ".ascii", ".test", ".cast"];

/// Settings that may change mid-tape (everything else is frozen once the
/// terminal has spawned, because canvas geometry is fixed).
fn is_runtime_setting(name: &str) -> bool {
    matches!(
        name,
        "TypingSpeed" | "WaitTimeout" | "WaitPattern" | "PlaybackSpeed" | "Theme"
    )
}

/// Post-parse validation: catches VHS-grammar-valid constructs that vterm
/// cannot execute, so `vterm check` fails fast with precise positions.
pub fn validate(commands: &[Command]) -> Vec<ParseError> {
    let mut errors = Vec::new();
    let mut started = false;

    for cmd in commands {
        match cmd.command_type {
            TokenType::Output => {
                let ext = cmd.options.as_str();
                if cmd.args.ends_with('/') {
                    errors.push(ParseError::new(
                        cmd.token.clone(),
                        "PNG frame directories are not supported by vterm",
                    ));
                } else if ext == ".mp4" || ext == ".webm" {
                    errors.push(ParseError::new(
                        cmd.token.clone(),
                        format!(
                            "video output ({}) requires ffmpeg; vterm supports {}",
                            ext,
                            SUPPORTED_OUTPUTS.join("/")
                        ),
                    ));
                } else if !SUPPORTED_OUTPUTS.contains(&ext) {
                    errors.push(ParseError::new(
                        cmd.token.clone(),
                        format!(
                            "unsupported output format {}; vterm supports {}",
                            ext,
                            SUPPORTED_OUTPUTS.join("/")
                        ),
                    ));
                }
            }
            TokenType::Set => {
                if started && !is_runtime_setting(&cmd.options) {
                    errors.push(ParseError::new(
                        cmd.token.clone(),
                        format!(
                            "Set {} cannot appear after commands have started \
                             (terminal geometry is fixed once the shell spawns); \
                             move it to the top of the tape",
                            cmd.options
                        ),
                    ));
                }
            }
            TokenType::Require | TokenType::Env | TokenType::Illegal => {}
            _ => {
                started = true;
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_matches_vhs_format() {
        let tok = Token {
            token_type: TokenType::String,
            literal: "Foo".into(),
            line: 4,
            column: 1,
        };
        let err = ParseError::new(tok, "Invalid command: Foo");
        assert_eq!(err.to_string(), " 4:1  │ Invalid command: Foo");
    }
}
