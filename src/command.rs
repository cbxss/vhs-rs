//! The parsed command model.
//!
//! Mirrors VHS's `parser.Command` (stringly options/args keep the grammar port
//! 1:1 and the ported tests valid); vhs_rs additionally carries the originating
//! token so runtime errors and JSON reports can cite line:column.

use crate::token::{Token, TokenType};

/// A single tape command. `options` and `args` follow VHS conventions per
/// command type (e.g. for keypresses options is the speed, args the repeat
/// count; for Wait/Assert args is "<Scope> <regex>").
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub command_type: TokenType,
    pub options: String,
    pub args: String,
    /// Path of the tape this command came from via `Source` (empty otherwise).
    pub source: String,
    /// Token that started this command, for line:column reporting.
    pub token: Token,
}

impl Command {
    pub fn new(command_type: TokenType, token: Token) -> Self {
        Command {
            command_type,
            options: String::new(),
            args: String::new(),
            source: String::new(),
            token,
        }
    }
}

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.command_type)?;
        if !self.options.is_empty() {
            write!(f, " {}", self.options)?;
        }
        if !self.args.is_empty() {
            write!(f, " {}", self.args)?;
        }
        Ok(())
    }
}
