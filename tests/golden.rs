//! Byte-exact golden tests for vhs_rs's deterministic `.txt` output.
//!
//! For every `tests/fixtures/golden/<name>.tape`, run the built binary in a
//! scratch directory and compare the produced `<name>.txt` byte-for-byte
//! against the checked-in `<name>.golden.txt`. This is the determinism
//! contract under test: two runs of the same tape must produce identical
//! text artifacts.
//!
//! Regenerating goldens (e.g. after a font-metrics change shifts the
//! terminal grid size):
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test --test golden
//! ```
//!
//! PNG artifacts are checked structurally (exists, decodes, expected
//! dimensions) rather than byte-pinned, so glyph rendering can evolve
//! without churning fixtures.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden")
}

/// Fresh scratch directory for one test run (std-only; no tempfile dep).
fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vhs_rs-golden-{}-{}", name, std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("clear stale scratch dir");
    }
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Runs `<name>.tape` with the built binary, cwd'd into a scratch dir so the
/// tape's relative `Output` paths land there. Returns the scratch dir.
fn run_tape(name: &str) -> PathBuf {
    let tape = fixture_dir().join(format!("{name}.tape"));
    let out = scratch_dir(name);
    let output = Command::new(env!("CARGO_BIN_EXE_vhs-rs"))
        .arg("run")
        .arg("--quiet")
        .arg(&tape)
        .current_dir(&out)
        .output()
        .expect("failed to spawn vhs_rs");
    assert!(
        output.status.success(),
        "vhs_rs run {name}.tape exited {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    out
}

/// Compares `<scratch>/<name>.txt` against `<name>.golden.txt`, or rewrites
/// the golden when UPDATE_GOLDEN=1.
fn check_golden(name: &str) -> PathBuf {
    let out = run_tape(name);
    let produced = out.join(format!("{name}.txt"));
    let actual = fs::read(&produced)
        .unwrap_or_else(|e| panic!("missing output {}: {e}", produced.display()));
    let golden_path = fixture_dir().join(format!("{name}.golden.txt"));

    if std::env::var("UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        fs::write(&golden_path, &actual).expect("write golden");
        return out;
    }

    let expected = fs::read(&golden_path).unwrap_or_else(|e| {
        panic!(
            "missing golden {}: {e}\nrun `UPDATE_GOLDEN=1 cargo test --test golden` to create it",
            golden_path.display()
        )
    });
    assert_eq!(
        String::from_utf8_lossy(&expected),
        String::from_utf8_lossy(&actual),
        "golden mismatch for {name}.tape \
         (regen with `UPDATE_GOLDEN=1 cargo test --test golden` if intentional)"
    );
    assert_eq!(expected, actual, "golden byte mismatch for {name}.tape");
    out
}

/// Structural PNG check: exists, decodes, has the expected pixel dimensions.
/// Deliberately not byte-pinned so the embedded font can change freely.
fn check_png(path: &Path, width: u32, height: u32) {
    let file = fs::File::open(path)
        .unwrap_or_else(|e| panic!("missing screenshot {}: {e}", path.display()));
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().expect("PNG header should decode");
    let info = reader.info();
    assert_eq!(
        (info.width, info.height),
        (width, height),
        "unexpected dimensions for {}",
        path.display()
    );
    let size = reader
        .output_buffer_size()
        .expect("PNG output size should fit in memory");
    let mut buf = vec![0u8; size];
    reader
        .next_frame(&mut buf)
        .expect("PNG image data should decode");
}

#[test]
fn golden_colors() {
    let out = check_golden("colors");
    // colors.tape sets Width 800 / Height 400 and takes a screenshot.
    check_png(&out.join("colors-shot.png"), 800, 400);
    // Screenshot always writes a plain-text sibling next to the PNG.
    let sibling = out.join("colors-shot.txt");
    assert!(sibling.exists(), "missing screenshot text sibling");
    let _ = fs::remove_dir_all(&out);
}

#[test]
fn golden_editing() {
    let out = check_golden("editing");
    let _ = fs::remove_dir_all(&out);
}

#[test]
fn golden_unicode() {
    let out = check_golden("unicode");
    let _ = fs::remove_dir_all(&out);
}
