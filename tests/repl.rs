//! CLI-level tests for the line-oriented `vhs-rs repl` protocol.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn vhs_rs() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vhs-rs"))
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vhs_rs-repl-{tag}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear scratch");
    }
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

fn repl_in(dir: &Path, input: &str, extra_args: &[&str]) -> Output {
    let mut cmd = vhs_rs();
    cmd.arg("repl")
        .arg("--quiet")
        .args(extra_args)
        .current_dir(dir);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vhs-rs repl");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write repl input");
    child.wait_with_output().expect("wait for repl")
}

fn json_lines(out: &Output) -> Vec<serde_json::Value> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line:?}")))
        .collect()
}

#[test]
fn happy_path_emits_ready_term_commands_and_report() {
    let dir = scratch("happy");
    let out = repl_in(&dir, "Type \"echo hi\"\nEnter\nWait\n", &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = json_lines(&out);
    assert_eq!(lines[0]["kind"], "ready");
    assert_eq!(lines[1]["kind"], "term");
    let commands: Vec<&serde_json::Value> =
        lines.iter().filter(|v| v["kind"] == "command").collect();
    assert_eq!(commands.len(), 3, "lines: {lines:?}");
    assert!(commands.iter().all(|c| c["status"] == "ok"));
    assert_eq!(lines.last().unwrap()["kind"], "report");
    assert_eq!(lines.last().unwrap()["status"], "success");
}

#[test]
fn parse_error_line_is_atomic_and_session_continues() {
    let dir = scratch("parse");
    let out = repl_in(
        &dir,
        "DefinitelyNotACommand\nType \"echo ok\"\nEnter\nWait\n",
        &[],
    );
    assert_eq!(out.status.code(), Some(0));
    let lines = json_lines(&out);
    assert!(lines.iter().any(|v| v["kind"] == "parse_error"));
    let commands: Vec<&serde_json::Value> =
        lines.iter().filter(|v| v["kind"] == "command").collect();
    assert_eq!(commands.len(), 3, "lines: {lines:?}");
    assert!(commands.iter().all(|c| c["status"] == "ok"));
}

#[test]
fn assert_failure_continues_unless_strict() {
    let dir = scratch("assert-continue");
    let out = repl_in(
        &dir,
        "Assert /never-on-screen-xyz/\nType \"echo after\"\nEnter\nWait\n",
        &[],
    );
    assert_eq!(out.status.code(), Some(0));
    let lines = json_lines(&out);
    assert!(
        lines
            .iter()
            .any(|v| v["kind"] == "command" && v["status"] == "failed")
    );
    assert_eq!(lines.last().unwrap()["status"], "success");

    let strict_dir = scratch("assert-strict");
    let out = repl_in(
        &strict_dir,
        "Assert /never-on-screen-xyz/ Type \"echo after\"\n",
        &["--strict"],
    );
    assert_eq!(out.status.code(), Some(1));
    let lines = json_lines(&out);
    assert_eq!(lines.last().unwrap()["status"], "assert_failed");
    assert!(
        lines
            .iter()
            .any(|v| v["kind"] == "command" && v["status"] == "skipped")
    );
}

#[test]
fn blank_and_multi_command_lines_get_responses() {
    let dir = scratch("blank-multi");
    let out = repl_in(
        &dir,
        "\n# comment only\nType \"echo two\" Enter\nWait\n",
        &[],
    );
    assert_eq!(out.status.code(), Some(0));
    let lines = json_lines(&out);
    assert_eq!(
        lines.iter().filter(|v| v["kind"] == "empty").count(),
        2,
        "lines: {lines:?}"
    );
    let line3: Vec<&serde_json::Value> = lines
        .iter()
        .filter(|v| v["kind"] == "command" && v["input_line"] == 3)
        .collect();
    assert_eq!(line3.len(), 2, "lines: {lines:?}");
}

#[test]
fn dead_child_is_reported_on_next_action() {
    let dir = scratch("dead-child");
    let out = repl_in(
        &dir,
        "Type \"exit\"\nEnter\nSleep 300ms\nType \"echo nope\"\n",
        &[],
    );
    assert_eq!(out.status.code(), Some(0));
    let lines = json_lines(&out);
    let failed = lines
        .iter()
        .find(|v| v["kind"] == "command" && v["status"] == "failed")
        .expect("failed command");
    assert_eq!(failed["failure"]["reason"], "child_exited");
}

#[test]
fn eof_report_and_recorded_timeline_are_renderable() {
    let dir = scratch("record");
    let out = repl_in(
        &dir,
        "Type \"echo timeline\"\nEnter\nWait\n",
        &["--record", "session.jsonl"],
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(dir.join("session.jsonl").is_file(), "timeline missing");
    let render = vhs_rs()
        .args(["render", "session.jsonl", "-o", "screen.txt"])
        .current_dir(&dir)
        .output()
        .expect("render timeline");
    assert_eq!(
        render.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&render.stderr)
    );
    let screen = std::fs::read_to_string(dir.join("screen.txt")).unwrap();
    assert!(screen.contains("timeline"), "screen: {screen}");
}

#[test]
fn timeout_emits_runtime_report() {
    let dir = scratch("timeout");
    let out = repl_in(&dir, "Sleep 30s\n", &["--timeout", "700ms"]);
    assert_eq!(out.status.code(), Some(4));
    let lines = json_lines(&out);
    let report = lines.last().unwrap();
    assert_eq!(report["kind"], "report");
    assert_eq!(report["exit_code"], 4);
    assert_eq!(report["failure"]["reason"], "run_timeout");
}
