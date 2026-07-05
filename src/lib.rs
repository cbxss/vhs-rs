//! vhs_rs — agent-first terminal automation.
//!
//! Executes VHS-compatible `.tape` scripts against a real PTY, models the
//! screen with an offscreen terminal emulator, supports event-driven `Wait`
//! and `Assert`, and renders PNG screenshots and GIFs natively. No browser,
//! no ffmpeg.

pub mod command;
pub mod error;
pub mod lexer;
pub mod parser;
pub mod snapshot;
pub mod token;
pub mod util;

pub mod keys;
pub mod pty;
pub mod session;
pub mod term;

pub mod theme;
pub mod render {
    pub mod chrome;
    pub mod font;
    pub mod grid;
    mod renderer;
    pub use renderer::*;
}
pub mod encode {
    pub mod cast;
    pub mod gif;
    pub mod png;
    pub mod txt;
}

pub mod cli;
pub mod evaluator;
pub mod report;

use error::ParseError;

/// Parses a tape and runs vhs_rs's validation pass; returns the commands and
/// all parse + validation errors (empty when the tape is clean).
pub fn parse_tape(src: &str) -> (Vec<command::Command>, Vec<ParseError>) {
    let mut p = parser::Parser::new(src);
    let cmds = p.parse();
    let mut errs = p.into_errors();
    errs.extend(parser::validate(&cmds));
    (cmds, errs)
}
