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

/// A minimal fast tape writing its golden to an absolute temp path.
fn runtime_tape(tag: &str) -> (String, std::path::PathBuf) {
    let out = std::env::temp_dir().join(format!("vterm_cli_{}_{}.txt", tag, std::process::id()));
    let tape = format!(
        "Output \"{}\"\nSet TypingSpeed 5ms\nType \"echo hi\"\nEnter\nWait\nAssert+Screen /hi/\n",
        out.display()
    );
    (tape, out)
}

#[test]
fn run_reads_tape_from_stdin() {
    let (tape, out_path) = runtime_tape("stdin");
    let out = run_with_stdin(&["run", "-"], &tape);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let golden = std::fs::read_to_string(&out_path).expect("golden written");
    assert!(golden.contains("echo hi"), "golden content: {golden}");
    let _ = std::fs::remove_file(out_path);
}

#[test]
fn run_clean_tape_executes_end_to_end() {
    let (tape_src, out_path) = runtime_tape("run");
    let tape = TempTape::new("run_clean", &tape_src);
    let out = vterm(&["run", tape.path()]).output().expect("run vterm");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out_path.exists(), "golden artifact missing");
    let _ = std::fs::remove_file(out_path);
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
    let (tape_src, out_path) = runtime_tape("bare");
    let tape = TempTape::new("default_run", &tape_src);
    let out = vterm(&[tape.path()]).output().expect("run vterm");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out_path.exists(), "golden artifact missing");
    let _ = std::fs::remove_file(out_path);
}

#[test]
fn run_failing_assert_exits_one_with_forensics_and_json() {
    let dir = std::env::temp_dir().join(format!("vterm_forensics_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out_txt = dir.join("f.txt");
    let tape_src = format!(
        "Output \"{}\"\nSet TypingSpeed 5ms\nType \"echo hi\"\nEnter\nWait\nAssert+Screen /never matches xyz/\n",
        out_txt.display()
    );
    let tape = TempTape::new("run_fail", &tape_src);
    let out = vterm(&["run", "--json", tape.path()])
        .output()
        .expect("run vterm");
    assert_eq!(out.status.code(), Some(1));

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is a JSON report");
    assert_eq!(v["status"], "assert_failed");
    assert_eq!(v["exit_code"], 1);
    let last = v["commands"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["status"], "failed");
    assert!(
        last["detail"]["screen_text"]
            .as_str()
            .unwrap()
            .contains("echo hi"),
        "failure detail must embed the actual screen"
    );
    assert!(dir.join("f.failure.txt").exists(), "failure text forensics");
    assert!(dir.join("f.failure.png").exists(), "failure png forensics");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn run_wait_timeout_exits_three() {
    let tape_src =
        "Set TypingSpeed 5ms\nType \"echo hi\"\nEnter\nWait+Screen@1s /never matches xyz/\n";
    let tape = TempTape::new("run_timeout", tape_src);
    let out = vterm(&["run", "--quiet", tape.path()])
        .output()
        .expect("run vterm");
    assert_eq!(out.status.code(), Some(3));
    // Forensics stem falls back to the tape path.
    let stem = tape.path().trim_end_matches(".tape").to_string();
    assert!(std::path::Path::new(&format!("{stem}.failure.txt")).exists());
    let _ = std::fs::remove_file(format!("{stem}.failure.txt"));
    let _ = std::fs::remove_file(format!("{stem}.failure.png"));
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
