//! Agent-contract regression suite.
//!
//! Every test here encodes a bug found by adversarially attacking the
//! binary (see the v0.1.1/v0.1.2 fixes): false success on a dead shell,
//! schema drift in `--json`, misclassified failures, panics on hostile
//! durations, lost reports on signals. Each drives the compiled binary the
//! way an agent would and asserts on the stable API: exit codes, the JSON
//! run-report shape, `failure.reason`, and forensics files on disk.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

mod common;
use common::TempTape;

fn vhs_rs() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vhs-rs"))
}

/// Fresh scratch directory (cwd for runs, so forensics land somewhere
/// inspectable and isolated).
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vhs_rs-contract-{tag}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear stale scratch");
    }
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

/// Runs `vhs-rs run --json -` with `tape` on stdin, cwd'd to `dir`.
fn run_json_in(dir: &Path, tape: &str, extra_args: &[&str]) -> Output {
    let mut cmd = vhs_rs();
    cmd.arg("run").arg("--json");
    cmd.args(extra_args);
    cmd.arg("-").current_dir(dir);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vhs-rs");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(tape.as_bytes())
        .expect("write tape");
    child.wait_with_output().expect("wait")
}

/// Parses stdout as the run-report JSON, panicking with context on failure.
fn report(out: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not valid JSON ({e})\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// Asserts the invariants every run report must satisfy, whatever the
/// outcome: version, status/exit_code consistency, failure.reason presence.
fn assert_report_invariants(r: &serde_json::Value, out: &Output) {
    assert_eq!(r["version"], 1);
    let exit_code = r["exit_code"].as_i64().expect("exit_code is an integer");
    assert_eq!(
        out.status.code(),
        Some(exit_code as i32),
        "process exit must equal report exit_code"
    );
    let status = r["status"].as_str().expect("status is a string");
    if status == "success" {
        assert!(r["failure"].is_null(), "success must carry no failure");
    } else {
        assert!(
            r["failure"]["reason"].is_string(),
            "failed run must carry failure.reason; report: {r}"
        );
    }
}

// ---- Exit taxonomy & JSON schema on every exit path -------------------------

#[test]
fn parse_error_under_run_json_is_a_run_report() {
    let dir = scratch("parse-err");
    let out = run_json_in(&dir, "Frobnicate 3\n", &[]);
    let r = report(&out);
    assert_report_invariants(&r, &out);
    assert_eq!(r["status"], "parse_error");
    assert_eq!(r["exit_code"], 2);
    assert_eq!(r["failure"]["reason"], "parse_error");
    let errors = r["errors"].as_array().expect("top-level errors array");
    assert!(!errors.is_empty());
    assert!(errors[0]["line"].is_u64() && errors[0]["message"].is_string());
}

#[test]
fn unreadable_tape_under_run_json_is_a_run_report() {
    let out = vhs_rs()
        .args(["run", "--json", "definitely-missing.tape"])
        .output()
        .expect("run");
    let r = report(&out);
    assert_report_invariants(&r, &out);
    assert_eq!(r["status"], "runtime_error");
    assert_eq!(r["exit_code"], 4);
}

#[test]
fn wait_timeout_is_exit_three() {
    let dir = scratch("wait-timeout");
    let out = run_json_in(&dir, "Wait@300ms /never-matches-xyz/\n", &[]);
    let r = report(&out);
    assert_report_invariants(&r, &out);
    assert_eq!(r["status"], "wait_timeout");
    assert_eq!(r["exit_code"], 3);
    assert_eq!(r["failure"]["reason"], "wait_timeout");
}

#[test]
fn success_is_exit_zero() {
    let dir = scratch("success");
    let out = run_json_in(&dir, "Type \"echo ok\"\nEnter\nWait\n", &[]);
    let r = report(&out);
    assert_report_invariants(&r, &out);
    assert_eq!(r["status"], "success");
    assert_eq!(r["exit_code"], 0);
}

// ---- Child death must never masquerade as success or timeout ----------------

#[test]
fn broken_shell_is_child_exited_not_success() {
    let dir = scratch("broken-shell");
    let out = run_json_in(
        &dir,
        "Set Shell \"definitely-not-a-shell-xyz\"\nType \"hello\"\nEnter\n",
        &[],
    );
    let r = report(&out);
    assert_report_invariants(&r, &out);
    assert_eq!(
        r["exit_code"], 4,
        "a dead shell must not exit 0; report: {r}"
    );
    assert_eq!(r["failure"]["reason"], "child_exited");
}

#[test]
fn wait_after_child_exit_is_child_exited_not_timeout() {
    let dir = scratch("wait-dead");
    let out = run_json_in(&dir, "Type \"exit\"\nEnter\nWait\n", &[]);
    let r = report(&out);
    assert_report_invariants(&r, &out);
    assert_eq!(r["exit_code"], 4);
    assert_eq!(r["failure"]["reason"], "child_exited");
    let msg = r["failure"]["message"].as_str().unwrap();
    assert!(
        msg.contains("child exited"),
        "message must name the real cause, got: {msg}"
    );
}

// ---- Failed checks: evidence quality, skips, forensics -----------------------

#[test]
fn assert_failure_reports_full_evidence_and_forensics() {
    let dir = scratch("assert-fail");
    let tape = "Type \"echo first\"\nEnter\nWait\nAssert+Screen /never-on-screen-xyz/\nCapture after.txt\n";
    let out = run_json_in(&dir, tape, &[]);
    let r = report(&out);
    assert_report_invariants(&r, &out);
    assert_eq!(r["status"], "assert_failed");
    assert_eq!(r["exit_code"], 1);

    let cmds = r["commands"].as_array().unwrap();
    let failed = cmds.iter().find(|c| c["status"] == "failed").unwrap();
    let screen = failed["detail"]["screen_text"].as_str().unwrap();
    assert!(
        screen.contains("echo first"),
        "screen_text must carry the actual screen"
    );
    // Commands after the failure never ran but stay in the report.
    assert!(cmds.iter().any(|c| c["status"] == "skipped"));

    // Forensics written next to the (stdin) tape stem, and reported.
    assert!(dir.join("stdin.failure.txt").is_file());
    assert!(dir.join("stdin.failure.png").is_file());
    let kinds: Vec<&str> = r["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"failure_text") && kinds.contains(&"failure_png"));
}

#[test]
fn line_scoped_failure_carries_line_text_and_full_screen() {
    let dir = scratch("line-scope");
    let tape = "Type \"echo one\"\nEnter\nWait\nWait+Line@300ms /never-xyz/\n";
    let out = run_json_in(&dir, tape, &[]);
    let r = report(&out);
    let failed = r["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["status"] == "failed")
        .unwrap();
    assert!(
        failed["detail"]["line_text"].is_string(),
        "Line scope must include line_text"
    );
    let screen = failed["detail"]["screen_text"].as_str().unwrap();
    assert!(
        screen.lines().count() > 1,
        "screen_text must be the full screen, not one line"
    );
}

// ---- The run must always end in a report ------------------------------------

#[test]
fn run_timeout_fails_cleanly_with_forensics() {
    let dir = scratch("run-timeout");
    let out = run_json_in(
        &dir,
        "Type \"echo up\"\nEnter\nWait\nSleep 30s\n",
        &["--timeout", "2s"],
    );
    let r = report(&out);
    assert_report_invariants(&r, &out);
    assert_eq!(r["exit_code"], 4);
    assert_eq!(r["failure"]["reason"], "run_timeout");
    assert!(dir.join("stdin.failure.txt").is_file());
    assert!(dir.join("stdin.failure.png").is_file());
}

#[test]
fn sigterm_still_emits_a_report() {
    let dir = scratch("sigterm");
    let tape = TempTape::new("sigterm", "Type \"echo up\"\nEnter\nWait\nSleep 30s\n");
    let child = vhs_rs()
        .args(["run", "--json", tape.path()])
        .current_dir(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    // Give it time to get past the prompt and into the Sleep.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("send SIGTERM");
    let out = child.wait_with_output().expect("wait");
    let r = report(&out);
    assert_eq!(r["exit_code"], 4);
    assert_eq!(r["failure"]["reason"], "interrupted");
    assert!(
        !r["commands"].as_array().unwrap().is_empty(),
        "partial report must carry the commands that ran"
    );
}

// ---- Require ----------------------------------------------------------------

#[test]
fn require_rejects_non_executable_files() {
    let dir = scratch("require-noexec");
    let fake = dir.join("fakebin");
    std::fs::create_dir(&fake).unwrap();
    std::fs::write(fake.join("notexec"), "not a binary").unwrap();
    let path = format!(
        "{}:{}",
        fake.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = vhs_rs();
    cmd.args(["run", "--json", "-"])
        .current_dir(&dir)
        .env("PATH", path);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"Require notexec\nType \"hi\"\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let r = report(&out);
    assert_eq!(r["exit_code"], 4);
    assert!(
        r["failure"]["message"]
            .as_str()
            .unwrap()
            .contains("not found in PATH")
    );
}

#[test]
fn require_honors_env_path_override() {
    // `bash` exists on the real PATH, but the tape's Env PATH points at an
    // empty dir — Require must search the child's PATH, not ours.
    let dir = scratch("require-envpath");
    let empty = dir.join("empty");
    std::fs::create_dir(&empty).unwrap();
    let tape = format!(
        "Env PATH \"{}\"\nRequire bash\nType \"hi\"\n",
        empty.display()
    );
    let out = run_json_in(&dir, &tape, &[]);
    let r = report(&out);
    assert_eq!(r["exit_code"], 4);
    assert!(
        r["failure"]["message"]
            .as_str()
            .unwrap()
            .contains("not found in PATH")
    );
}

// ---- Hostile inputs must be parse errors, never panics -----------------------

#[test]
fn overflowing_durations_and_counts_are_parse_errors_not_panics() {
    const HUGE: &str = "999999999999999999999999";
    let cases = [
        format!("Sleep {HUGE}\n"),
        format!("Set TypingSpeed {HUGE}ms\nType \"x\"\n"),
        format!("Set WaitTimeout {HUGE}s\nType \"x\"\n"),
        format!("Type@{HUGE}s \"x\"\n"),
        format!("Wait@{HUGE}s\n"),
        format!("Enter {HUGE}\n"),
        "Set TypingSpeed banana\nType \"x\"\n".to_string(),
    ];
    for tape in &cases {
        let mut child = vhs_rs()
            .args(["check", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(tape.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(2),
            "tape {tape:?} must be a parse error (code None = killed by signal/panic); stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn crlf_tapes_parse_and_run() {
    let dir = scratch("crlf");
    let out = run_json_in(&dir, "Type \"echo crlf\"\r\nEnter\r\nWait\r\n", &[]);
    let r = report(&out);
    assert_eq!(r["status"], "success", "report: {r}");
}
