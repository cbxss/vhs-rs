//! Lexer for the VHS-compatible tape language.
//!
//! Byte-wise port of vhs/lexer/lexer.go. Operates on bytes; all delimiters and
//! keyword characters are ASCII, so multi-byte UTF-8 content inside strings,
//! regexes, JSON, and comments passes through untouched. Line/column numbers
//! are byte-based, matching VHS.

use crate::token::{Token, TokenType, lookup_identifier};

#[derive(Debug)]
pub struct Lexer<'a> {
    input: &'a [u8],
    src: &'a str,
    ch: u8,
    pos: usize,
    next_pos: usize,
    line: usize,
    column: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        let mut l = Lexer {
            input: input.as_bytes(),
            src: input,
            ch: 0,
            pos: 0,
            next_pos: 0,
            line: 1,
            column: 0,
        };
        l.read_char();
        l
    }

    fn read_char(&mut self) {
        self.column += 1;
        self.ch = self.peek_char();
        self.pos = self.next_pos;
        self.next_pos += 1;
    }

    fn peek_char(&self) -> u8 {
        if self.next_pos >= self.input.len() {
            0
        } else {
            self.input[self.next_pos]
        }
    }

    pub fn next_token(&mut self) -> Token {
        self.skip_whitespace();

        let line = self.line;
        let column = self.column;
        let tok = |token_type, literal: String| Token {
            token_type,
            literal,
            line,
            column,
        };

        match self.ch {
            0 => tok(TokenType::Eof, "\0".into()),
            b'@' => {
                self.read_char();
                tok(TokenType::At, "@".into())
            }
            b'=' => {
                self.read_char();
                tok(TokenType::Equal, "=".into())
            }
            b']' => {
                self.read_char();
                tok(TokenType::RightBracket, "]".into())
            }
            b'[' => {
                self.read_char();
                tok(TokenType::LeftBracket, "[".into())
            }
            b'-' => {
                self.read_char();
                tok(TokenType::Minus, "-".into())
            }
            b'%' => {
                self.read_char();
                tok(TokenType::Percent, "%".into())
            }
            b'^' => {
                self.read_char();
                tok(TokenType::Caret, "^".into())
            }
            b'\\' => {
                self.read_char();
                tok(TokenType::Backslash, "\\".into())
            }
            b'#' => {
                let literal = self.read_comment();
                tok(TokenType::Comment, literal)
            }
            b'+' => {
                self.read_char();
                tok(TokenType::Plus, "+".into())
            }
            b'{' => {
                let literal = format!("{{{}}}", self.read_json());
                self.read_char();
                tok(TokenType::Json, literal)
            }
            b'`' => {
                let literal = self.read_string(b'`');
                self.read_char();
                tok(TokenType::String, literal)
            }
            b'\'' => {
                let literal = self.read_string(b'\'');
                self.read_char();
                tok(TokenType::String, literal)
            }
            b'"' => {
                let literal = self.read_string(b'"');
                self.read_char();
                tok(TokenType::String, literal)
            }
            b'/' => {
                let literal = self.read_regex(b'/');
                self.read_char();
                tok(TokenType::Regex, literal)
            }
            ch => {
                if is_digit(ch) || (is_dot(ch) && is_digit(self.peek_char())) {
                    let literal = self.read_number();
                    tok(TokenType::Number, literal)
                } else if is_letter(ch) || is_dot(ch) {
                    let literal = self.read_identifier();
                    tok(lookup_identifier(&literal), literal)
                } else {
                    self.read_char();
                    tok(TokenType::Illegal, (ch as char).to_string())
                }
            }
        }
    }

    /// Reads a comment: `# Foo` => `Foo` (up to end of line).
    fn read_comment(&mut self) -> String {
        let start = self.pos + 1;
        loop {
            self.read_char();
            if is_new_line(self.ch) || self.ch == 0 {
                break;
            }
        }
        self.src[start..self.pos].to_string()
    }

    /// Reads a string literal delimited by `end_char` (no escapes, single line).
    fn read_string(&mut self, end_char: u8) -> String {
        let start = self.pos + 1;
        loop {
            self.read_char();
            if self.ch == end_char || self.ch == 0 || is_new_line(self.ch) {
                break;
            }
        }
        self.src[start..self.pos].to_string()
    }

    /// Reads a regex pattern, handling escaped delimiters: an odd number of
    /// consecutive backslashes escapes the delimiter, an even number does not.
    fn read_regex(&mut self, end_char: u8) -> String {
        let start = self.pos + 1;
        loop {
            self.read_char();
            if self.ch == 0 || is_new_line(self.ch) {
                break;
            }

            if self.ch == b'\\' {
                let mut backslash_count = 0;
                while self.ch == b'\\' && self.pos < self.input.len() {
                    backslash_count += 1;
                    self.read_char();
                }
                if self.ch == end_char {
                    if backslash_count % 2 == 1 {
                        continue;
                    }
                    // Even number of backslashes: delimiter is NOT escaped.
                    break;
                }
                continue;
            }

            if self.ch == end_char {
                break;
            }
        }
        self.src[start..self.pos].to_string()
    }

    /// Reads a JSON object body (naive: up to the first `}`, like VHS).
    fn read_json(&mut self) -> String {
        let start = self.pos + 1;
        loop {
            self.read_char();
            if self.ch == b'}' || self.ch == 0 {
                break;
            }
        }
        self.src[start..self.pos].to_string()
    }

    fn read_number(&mut self) -> String {
        let start = self.pos;
        while is_digit(self.ch) || is_dot(self.ch) {
            self.read_char();
        }
        self.src[start..self.pos].to_string()
    }

    fn read_identifier(&mut self) -> String {
        let start = self.pos;
        while is_letter(self.ch)
            || is_dot(self.ch)
            || self.ch == b'-'
            || self.ch == b'_'
            || self.ch == b'/'
            || self.ch == b'%'
            || is_digit(self.ch)
        {
            self.read_char();
        }
        self.src[start..self.pos].to_string()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.ch, b' ' | b'\t' | b'\n' | b'\r') {
            // \r\n counts once: only \n bumps the line counter.
            if self.ch == b'\n' {
                self.line += 1;
                self.column = 0;
            }
            self.read_char();
        }
    }
}

fn is_dot(ch: u8) -> bool {
    ch == b'.'
}

fn is_letter(ch: u8) -> bool {
    ch.is_ascii_alphabetic()
}

fn is_digit(ch: u8) -> bool {
    ch.is_ascii_digit()
}

fn is_new_line(ch: u8) -> bool {
    ch == b'\n' || ch == b'\r'
}
