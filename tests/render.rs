//! End-to-end tests for `vhs-rs render`: record a real session, render it
//! back, and check the artifacts — plus asciicast import and the error
//! taxonomy (exit 2 bad input, exit 4 write failure).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn vhs_rs() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vhs-rs"))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vhs_rs-render-{tag}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear stale scratch");
    }
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

/// Runs a tape from stdin with `--record session.jsonl` in `dir`.
fn record_session(dir: &Path, tape: &str) -> Output {
    let mut child = vhs_rs()
        .args(["run", "--quiet", "--record", "session.jsonl", "-"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vhs-rs run");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(tape.as_bytes())
        .unwrap();
    child.wait_with_output().expect("wait")
}

fn render(dir: &Path, args: &[&str]) -> Output {
    vhs_rs()
        .arg("render")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn vhs-rs render")
}

fn decode_gif(path: &Path) -> (u16, u16, usize) {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut decoder = options
        .read_info(std::fs::File::open(path).expect("gif exists"))
        .expect("gif decodes");
    let (w, h) = (decoder.width(), decoder.height());
    let mut frames = 0;
    while decoder.read_next_frame().expect("frame decodes").is_some() {
        frames += 1;
    }
    (w, h, frames)
}

#[test]
fn record_then_render_reproduces_the_final_screen() {
    let dir = scratch("roundtrip");
    let tape = "Type \"echo render-roundtrip-42\"\nEnter\nWait\nCapture live.txt\n";
    let out = record_session(&dir, tape);
    assert!(out.status.success(), "run failed: {out:?}");

    let out = render(
        &dir,
        &[
            "session.jsonl",
            "-o",
            "replayed.txt",
            "-o",
            "replayed.gif",
            "-o",
            "replayed.png",
            "-o",
            "replayed.cast",
        ],
    );
    assert!(
        out.status.success(),
        "render failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The replayed final screen is byte-identical to what the live run saw.
    let live = std::fs::read_to_string(dir.join("live.txt")).unwrap();
    let replayed = std::fs::read_to_string(dir.join("replayed.txt")).unwrap();
    assert!(live.contains("render-roundtrip-42"));
    assert_eq!(live, replayed, "replayed screen differs from the live one");

    // GIF decodes with the canvas derived from the recorded grid.
    let (w, h, frames) = decode_gif(&dir.join("replayed.gif"));
    assert!(w > 0 && h > 0 && frames > 0);

    // The cast conversion is a valid asciicast (header + JSON event lines).
    let cast = std::fs::read_to_string(dir.join("replayed.cast")).unwrap();
    let header: serde_json::Value = serde_json::from_str(cast.lines().next().unwrap()).unwrap();
    assert_eq!(header["version"], 3);

    // PNG exists and has the same dimensions as the GIF.
    let png = std::fs::File::open(dir.join("replayed.png")).unwrap();
    let reader = png::Decoder::new(std::io::BufReader::new(png))
        .read_info()
        .unwrap();
    assert_eq!(
        (reader.info().width, reader.info().height),
        (w as u32, h as u32)
    );
}

#[test]
fn render_is_deterministic() {
    let dir = scratch("determinism");
    let out = record_session(&dir, "Type \"echo stable\"\nEnter\nWait\n");
    assert!(out.status.success(), "run failed: {out:?}");

    for name in ["a", "b"] {
        let out = render(
            &dir,
            &[
                "session.jsonl",
                "-o",
                &format!("{name}.gif"),
                "-o",
                &format!("{name}.txt"),
            ],
        );
        assert!(out.status.success());
    }
    assert_eq!(
        std::fs::read(dir.join("a.gif")).unwrap(),
        std::fs::read(dir.join("b.gif")).unwrap(),
        "two renders of one timeline must be byte-identical"
    );
    assert_eq!(
        std::fs::read(dir.join("a.txt")).unwrap(),
        std::fs::read(dir.join("b.txt")).unwrap()
    );
}

#[test]
fn idle_limit_and_speed_shrink_the_gif() {
    let dir = scratch("retime");
    // A 2.5s sleep between commands = a long idle gap in the timeline.
    let out = record_session(&dir, "Type \"echo slow\"\nEnter\nWait\nSleep 2500ms\n");
    assert!(out.status.success(), "run failed: {out:?}");

    let full = render(&dir, &["session.jsonl", "-o", "full.gif"]);
    assert!(full.status.success());
    let capped = render(
        &dir,
        &["session.jsonl", "-o", "capped.gif", "--idle-limit", "200ms"],
    );
    assert!(capped.status.success());

    let gif_duration = |path: &Path| {
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = options
            .read_info(std::fs::File::open(path).unwrap())
            .unwrap();
        let mut total = 0u64;
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            total += u64::from(frame.delay);
        }
        total
    };
    let (full_cs, capped_cs) = (
        gif_duration(&dir.join("full.gif")),
        gif_duration(&dir.join("capped.gif")),
    );
    assert!(
        capped_cs + 150 < full_cs,
        "idle cap must shorten playback: full {full_cs}cs capped {capped_cs}cs"
    );
}

#[test]
fn cast_import_renders_v2_and_v3() {
    let dir = scratch("cast-import");

    let v2 = concat!(
        "{\"version\":2,\"width\":30,\"height\":6}\n",
        "[0.1, \"o\", \"from-a-v2-cast\"]\n",
        "[0.2, \"i\", \"ignored input\"]\n",
    );
    std::fs::write(dir.join("v2.cast"), v2).unwrap();
    let out = render(&dir, &["v2.cast", "-o", "v2.txt", "-o", "v2.gif"]);
    assert!(
        out.status.success(),
        "v2 render failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let txt = std::fs::read_to_string(dir.join("v2.txt")).unwrap();
    assert!(txt.contains("from-a-v2-cast"), "{txt:?}");
    // The skipped "i" event is noted on stderr.
    assert!(String::from_utf8_lossy(&out.stderr).contains("skipped"));

    let v3 = concat!(
        "{\"version\":3,\"term\":{\"cols\":30,\"rows\":6}}\n",
        "[0.1, \"o\", \"v3 line one\\r\\n\"]\n",
        "[0.4, \"o\", \"v3 line two\"]\n",
        "[0.1, \"x\", \"\"]\n",
    );
    std::fs::write(dir.join("v3.cast"), v3).unwrap();
    let out = render(&dir, &["v3.cast", "-o", "v3.txt"]);
    assert!(out.status.success());
    let txt = std::fs::read_to_string(dir.join("v3.txt")).unwrap();
    assert!(txt.contains("v3 line one") && txt.contains("v3 line two"));
}

#[test]
fn bad_inputs_exit_two_write_failures_exit_four() {
    let dir = scratch("errors");

    // Missing input file.
    let out = render(&dir, &["missing.jsonl", "-o", "x.gif"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");

    // Corrupt header.
    std::fs::write(dir.join("garbage.jsonl"), "not json at all\n").unwrap();
    let out = render(&dir, &["garbage.jsonl", "-o", "x.gif"]);
    assert_eq!(out.status.code(), Some(2));

    // Unsupported input extension.
    std::fs::write(dir.join("input.mp4"), "").unwrap();
    let out = render(&dir, &["input.mp4", "-o", "x.gif"]);
    assert_eq!(out.status.code(), Some(2));

    // Unsupported output extension: usage error before any work.
    let out = record_session(&dir, "Type \"echo e\"\nEnter\nWait\n");
    assert!(out.status.success());
    let out = render(&dir, &["session.jsonl", "-o", "x.webm"]);
    assert_eq!(out.status.code(), Some(2));

    // Unknown theme name.
    let out = render(&dir, &["session.jsonl", "-o", "x.gif", "--theme", "nope"]);
    assert_eq!(out.status.code(), Some(2));

    // Unwritable output path (a file used as a directory) → exit 4.
    let out = render(&dir, &["session.jsonl", "-o", "session.jsonl/x.txt"]);
    assert_eq!(out.status.code(), Some(4), "{out:?}");
}
