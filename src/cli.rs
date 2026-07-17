//! Command-line interface: `vhs_rs run` / `vhs_rs check`.
//!
//! `vhs_rs <tape>` is shorthand for `vhs_rs run <tape>`. Both subcommands accept
//! `-` to read the tape from stdin, and `--json` for machine-readable output
//! (vhs_rs's primary consumer is an AI agent, so exit codes and JSON shape are
//! part of the stable contract).

use std::io::Read as _;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

use crate::command::Command;
use crate::error::{ExitKind, ParseError, render_parse_errors};
use crate::report::ReportBuilder;

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

    /// Render artifacts from a recorded timeline (.jsonl or .cast) without
    /// executing anything
    #[command(after_help = EXIT_CODES_HELP)]
    Render(RenderArgs),
}

#[derive(Args)]
struct RenderArgs {
    /// Recorded timeline: a vhs-rs .jsonl or an asciicast .cast (v2/v3)
    input: String,

    /// Output path, repeatable; the extension picks the format
    /// (.gif, .png, .txt, .cast)
    #[arg(short, long = "output", value_name = "PATH", required = true)]
    output: Vec<String>,

    /// Theme override: builtin name or inline JSON (mutes recorded mid-run
    /// theme changes)
    #[arg(long)]
    theme: Option<String>,

    /// Cap silent gaps between events (e.g. 2s) — agent thinking pauses
    /// stop being GIF freeze-frames
    #[arg(long, value_parser = parse_timeout, value_name = "DURATION")]
    idle_limit: Option<Duration>,

    /// Playback speed multiplier (delays divided by this)
    #[arg(long)]
    speed: Option<f64>,

    /// GIF frame-rate cap (max 50)
    #[arg(long)]
    framerate: Option<f64>,

    /// Font size override (the canvas re-derives from the recorded grid)
    #[arg(long)]
    font_size: Option<f32>,

    /// Suppress warnings and progress output (errors are still printed)
    #[arg(long)]
    quiet: bool,
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

    /// Whole-run wall-clock budget (e.g. 30s, 2m). On expiry the run fails
    /// with exit 4, reason `run_timeout`, and still writes report + forensics
    #[arg(long, value_parser = parse_timeout)]
    timeout: Option<Duration>,

    /// Stream the session to a .jsonl timeline as it runs (crash-safe;
    /// render it later with `vhs-rs render`)
    #[arg(long, value_name = "PATH")]
    record: Option<String>,
}

fn parse_timeout(s: &str) -> Result<Duration, String> {
    crate::util::parse_duration(s)
        .ok_or_else(|| format!("invalid duration {s:?} (examples: 500ms, 30s, 2m)"))
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
        Some(Cmd::Render(args)) => crate::cmd_render::render(&crate::cmd_render::RenderRequest {
            input: args.input,
            outputs: args.output,
            theme: args.theme,
            idle_limit: args.idle_limit,
            speed: args.speed,
            framerate: args.framerate,
            font_size: args.font_size,
            quiet: args.quiet,
        }),
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
        std::fs::read_to_string(path)
            .map_err(|e| format!("vhs-rs: failed to read tape {path}: {e}"))
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

/// Which `--json` shape a subcommand emits on load/parse failure: `run`
/// always prints a run report (agents switch on its `status`); `check`
/// prints the lighter parse-result object.
#[derive(Clone, Copy, PartialEq)]
enum JsonShape {
    RunReport,
    ParseResult,
}

/// Run-report JSON for a run that never started (unreadable tape, parse
/// errors): same shape as a real report — `status`, `exit_code`, `failure` —
/// plus a top-level `errors` array with the per-error diagnostics.
fn run_error_json(tape: &str, exit: ExitKind, message: &str, errors: &[ParseError]) -> String {
    let mut builder = ReportBuilder::new(tape);
    builder.set_failure(None, exit.reason(), message);
    let report = builder.finish(exit);
    let mut v = serde_json::to_value(&report).unwrap_or_default();
    if !errors.is_empty() {
        v["errors"] = parse_result_json(0, errors)["errors"].take();
    }
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| report.to_json())
}

/// Shared front half of `run`/`check`: resolve the tape path (`cmd` names the
/// subcommand in the usage message), read the tape, parse it, and emit any
/// errors (`--json` object per `shape`, or caret diagnostics). On failure
/// returns the process exit code; on success, the tape path and parsed
/// commands.
fn load_and_parse(
    cmd: &str,
    tape: Option<&str>,
    json: bool,
    shape: JsonShape,
) -> Result<(String, Vec<Command>), i32> {
    let Some(path) = tape else {
        eprintln!("vhs-rs: no tape given; usage: vhs_rs {cmd} <tape|-> (see --help)");
        return Err(ExitKind::Parse as i32);
    };

    let tape_src = match read_tape(path) {
        Ok(t) => t,
        Err(msg) => {
            if json {
                match shape {
                    JsonShape::RunReport => {
                        println!("{}", run_error_json(path, ExitKind::Runtime, &msg, &[]));
                    }
                    JsonShape::ParseResult => {
                        println!(
                            "{}",
                            serde_json::json!({
                                "ok": false, "commands": 0,
                                "errors": [{"line": 0, "col": 0, "message": msg}],
                            })
                        );
                    }
                }
            }
            eprintln!("{msg}");
            return Err(ExitKind::Runtime as i32);
        }
    };

    let (commands, errors) = crate::parse_tape(&tape_src);
    if !errors.is_empty() {
        if json {
            match shape {
                JsonShape::RunReport => {
                    let message = format!("{} parse error(s)", errors.len());
                    println!(
                        "{}",
                        run_error_json(path, ExitKind::Parse, &message, &errors)
                    );
                }
                JsonShape::ParseResult => {
                    println!("{}", parse_result_json(commands.len(), &errors));
                }
            }
        } else {
            eprintln!("{}", render_parse_errors(&tape_src, &errors));
        }
        return Err(ExitKind::Parse as i32);
    }

    Ok((path.to_string(), commands))
}

fn run(args: RunArgs) -> i32 {
    let (path, commands) =
        match load_and_parse("run", args.tape.as_deref(), args.json, JsonShape::RunReport) {
            Ok(parsed) => parsed,
            Err(code) => return code,
        };

    crate::evaluator::run(
        &path,
        &commands,
        args.json,
        args.quiet,
        args.timeout,
        args.record.as_deref(),
    )
}

fn check(args: CheckArgs) -> i32 {
    let (_, commands) = match load_and_parse(
        "check",
        args.tape.as_deref(),
        args.json,
        JsonShape::ParseResult,
    ) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };

    if args.json {
        println!("{}", parse_result_json(commands.len(), &[]));
    } else {
        println!("OK: {} commands", commands.len());
    }
    ExitKind::Success as i32
}
