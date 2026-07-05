//! Command-line interface: `vhs_rs run` / `vhs_rs check`.
//!
//! `vhs_rs <tape>` is shorthand for `vhs_rs run <tape>`. Both subcommands accept
//! `-` to read the tape from stdin, and `--json` for machine-readable output
//! (vhs_rs's primary consumer is an AI agent, so exit codes and JSON shape are
//! part of the stable contract).

use std::io::Read as _;

use clap::{Args, Parser, Subcommand};

use crate::error::{ExitKind, ParseError, render_parse_errors};

const EXIT_CODES_HELP: &str = "\
Exit codes:
  0  success
  1  assert failure
  2  parse/validation error
  3  wait timeout
  4  runtime/IO/PTY error";

#[derive(Parser)]
#[command(
    name = "vhs-rs",
    version,
    about = "Agent-first terminal automation: VHS-compatible tapes, screenshots, GIFs — no browser",
    after_help = EXIT_CODES_HELP,
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,

    /// Tape file to run (`-` for stdin); shorthand for `vhs_rs run <tape>`
    #[command(flatten)]
    run: RunArgs,
}

#[derive(Subcommand)]
enum Cmd {
    /// Execute a tape against a real PTY
    #[command(after_help = EXIT_CODES_HELP)]
    Run(RunArgs),

    /// Parse and validate a tape without executing it
    #[command(after_help = EXIT_CODES_HELP)]
    Check(CheckArgs),
}

#[derive(Args)]
struct RunArgs {
    /// Tape file to run (`-` for stdin)
    tape: Option<String>,

    /// Emit a machine-readable JSON run report on stdout
    #[arg(long)]
    json: bool,

    /// Suppress progress output (errors are still printed)
    #[arg(long)]
    quiet: bool,
}

#[derive(Args)]
struct CheckArgs {
    /// Tape file to check (`-` for stdin)
    tape: Option<String>,

    /// Emit a machine-readable JSON result on stdout
    #[arg(long)]
    json: bool,
}

/// CLI entry point; returns the process exit code (see [`ExitKind`]).
pub fn main() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let code = if err.use_stderr() { 2 } else { 0 };
            let _ = err.print();
            return code;
        }
    };

    match cli.command {
        Some(Cmd::Run(args)) => run(args),
        Some(Cmd::Check(args)) => check(args),
        None => run(cli.run),
    }
}

/// Reads a tape from a file path, or from stdin when the path is `-`.
fn read_tape(path: &str) -> Result<String, String> {
    if path == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("vhs-rs: failed to read tape from stdin: {e}"))?;
        Ok(buf)
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("vhs-rs: failed to read tape {path}: {e}"))
    }
}

/// JSON object for check/parse results: exactly one object on stdout.
fn parse_result_json(commands: usize, errors: &[ParseError]) -> serde_json::Value {
    serde_json::json!({
        "ok": errors.is_empty(),
        "commands": commands,
        "errors": errors
            .iter()
            .map(|e| {
                serde_json::json!({
                    "line": e.token.line,
                    "col": e.token.column,
                    "message": e.msg,
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn run(args: RunArgs) -> i32 {
    let Some(path) = args.tape.as_deref() else {
        eprintln!("vhs-rs: no tape given; usage: vhs_rs run <tape|-> (see --help)");
        return ExitKind::Parse as i32;
    };

    let tape = match read_tape(path) {
        Ok(t) => t,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitKind::Runtime as i32;
        }
    };

    let (commands, errors) = crate::parse_tape(&tape);
    if !errors.is_empty() {
        if args.json {
            println!("{}", parse_result_json(commands.len(), &errors));
        } else {
            eprintln!("{}", render_parse_errors(&tape, &errors));
        }
        return ExitKind::Parse as i32;
    }

    crate::evaluator::run(path, &commands, args.json, args.quiet)
}

fn check(args: CheckArgs) -> i32 {
    let Some(path) = args.tape.as_deref() else {
        eprintln!("vhs-rs: no tape given; usage: vhs_rs check <tape|-> (see --help)");
        return ExitKind::Parse as i32;
    };

    let tape = match read_tape(path) {
        Ok(t) => t,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitKind::Runtime as i32;
        }
    };

    let (commands, errors) = crate::parse_tape(&tape);

    if args.json {
        println!("{}", parse_result_json(commands.len(), &errors));
        return if errors.is_empty() {
            ExitKind::Success as i32
        } else {
            ExitKind::Parse as i32
        };
    }

    if errors.is_empty() {
        println!("OK: {} commands", commands.len());
        ExitKind::Success as i32
    } else {
        eprintln!("{}", render_parse_errors(&tape, &errors));
        ExitKind::Parse as i32
    }
}
