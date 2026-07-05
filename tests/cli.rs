//! CLI-level integration tests: run the compiled `vterm` binary and check
//! exit codes, stdout/stderr shape, --json output, and stdin (`-`) tapes.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

const OK_TAPE: &str = "Output demo.gif\nType \"echo hi\"\nEnter\nWait\n";
const BAD_TAPE: &str = "Foo\nSleep Bar\n";

fn vterm(args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vterm"));
    cmd.args(args);
    cmd
}

fn run_with_stdin(args: &[&str], stdin: &str) -> Output {
    let mut child = vterm(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vterm");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write tape to stdin");
    child.wait_with_output().expect("wait for vterm")
}

/// RAII temp tape file with a unique absolute path.
struct TempTape(std::path::PathBuf);

impl TempTape {
    fn new(tag: &str, contents: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("vterm_cli_test_{}_{tag}.tape", std::process::id()));
        std::fs::write(&path, contents).expect("write temp tape");
        TempTape(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().expect("utf-8 temp path")
    }
}

impl Drop for TempTape {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn check_ok_tape_exits_zero() {
    let tape = TempTape::new("check_ok", OK_TAPE);
    let out = vterm(&["check", tape.path()]).output().expect("run vterm");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout.trim(), "OK: 4 commands");
}

#[test]
fn check_bad_tape_exits_two_with_caret_errors() {
    let tape = TempTape::new("check_bad", BAD_TAPE);
    let out = vterm(&["check", tape.path()]).output().expect("run vterm");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr.contains('^'),
        "expected caret underline in stderr: {stderr}"
    );
    assert!(
        stderr.contains("Invalid command: Foo"),
        "expected error message in stderr: {stderr}"
    );
}

#[test]
fn check_json_reports_errors_with_positions() {
    let tape = TempTape::new("check_json", BAD_TAPE);
    let out = vterm(&["check", "--json", tape.path()])
        .output()
        .expect("run vterm");
    assert_eq!(out.status.code(), Some(2));

    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be one JSON object");
    assert_eq!(v["ok"], serde_json::json!(false));
    assert!(v["commands"].is_u64());
    let errors = v["errors"].as_array().expect("errors array");
    assert!(!errors.is_empty());
    assert_eq!(errors[0]["line"], serde_json::json!(1));
    assert_eq!(errors[0]["col"], serde_json::json!(1));
    assert!(
        errors[0]["message"]
            .as_str()
            .expect("message string")
            .contains("Invalid command: Foo")
    );
}

#[test]
fn check_json_ok_tape() {
    let tape = TempTape::new("check_json_ok", OK_TAPE);
    let out = vterm(&["check", "--json", tape.path()])
        .output()
        .expect("run vterm");
    assert_eq!(out.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be one JSON object");
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["commands"], serde_json::json!(4));
    assert_eq!(v["errors"], serde_json::json!([]));
}

#[test]
fn check_reads_tape_from_stdin() {
    let out = run_with_stdin(&["check", "-"], OK_TAPE);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout.trim(), "OK: 4 commands");

    let out = run_with_stdin(&["check", "-"], BAD_TAPE);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn run_reads_tape_from_stdin() {
    // Runtime is not wired up yet: a clean tape must reach the (stubbed)
    // evaluator hand-off and exit 4, not die earlier with a different code.
    let out = run_with_stdin(&["run", "-"], OK_TAPE);
    assert_eq!(out.status.code(), Some(4));
}

#[test]
fn run_clean_tape_exits_four_for_now() {
    let tape = TempTape::new("run_clean", OK_TAPE);
    let out = vterm(&["run", tape.path()]).output().expect("run vterm");
    assert_eq!(out.status.code(), Some(4));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not implemented yet"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn run_bad_tape_exits_two() {
    let tape = TempTape::new("run_bad", BAD_TAPE);
    let out = vterm(&["run", tape.path()]).output().expect("run vterm");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains('^'),
        "expected caret underline in stderr: {stderr}"
    );
}

#[test]
fn bare_tape_argument_defaults_to_run() {
    let tape = TempTape::new("default_run", OK_TAPE);
    let out = vterm(&[tape.path()]).output().expect("run vterm");
    assert_eq!(out.status.code(), Some(4));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not implemented yet"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn missing_tape_file_exits_four() {
    let out = vterm(&["check", "/nonexistent/definitely-not-here.tape"])
        .output()
        .expect("run vterm");
    assert_eq!(out.status.code(), Some(4));
}

#[test]
fn help_documents_exit_codes() {
    let out = vterm(&["--help"]).output().expect("run vterm");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "Exit codes:",
        "assert failure",
        "parse/validation error",
        "wait timeout",
    ] {
        assert!(
            stdout.contains(needle),
            "--help missing {needle:?}: {stdout}"
        );
    }
}

#[test]
fn version_prints_cargo_version() {
    let out = vterm(&["--version"]).output().expect("run vterm");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().ends_with(env!("CARGO_PKG_VERSION")),
        "unexpected --version output: {stdout}"
    );
}
