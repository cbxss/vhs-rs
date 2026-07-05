//! Plain-text artifacts: `Capture` screen dumps and VHS-compatible golden
//! files for `Output x.txt/.ascii/.test`.
//!
//! Golden semantics are ported from VHS's `testing.go`: after every executed
//! command the full screen buffer is appended, followed by a separator line
//! of 80 `─` (U+2500 BOX DRAWINGS LIGHT HORIZONTAL) characters.

use std::fs;
use std::io;
use std::path::Path;

/// VHS's golden-file separator: exactly 80 `─` characters.
const SEPARATOR: &str =
    "────────────────────────────────────────────────────────────────────────────────";

fn ensure_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

/// Writes the current screen text to `path` (for `Capture x.txt`), creating
/// parent directories as needed. The file always ends with a newline.
pub fn write_capture(path: &Path, screen_text: &str) -> io::Result<()> {
    ensure_parent(path)?;
    let mut content = String::with_capacity(screen_text.len() + 1);
    content.push_str(screen_text);
    if !content.ends_with('\n') {
        content.push('\n');
    }
    fs::write(path, content)
}

/// Accumulates per-command screen snapshots in VHS's golden format
/// (`Output x.txt/.ascii/.test`): each recorded buffer is followed by a
/// separator line of 80 `─` characters.
#[derive(Debug, Default, Clone)]
pub struct GoldenWriter {
    content: String,
}

impl GoldenWriter {
    /// Creates an empty golden accumulator.
    pub fn new() -> Self {
        GoldenWriter::default()
    }

    /// Records the screen buffer after a command: appends the full text, a
    /// newline, then the separator line.
    pub fn record(&mut self, screen_text: &str) {
        self.content.push_str(screen_text);
        self.content.push('\n');
        self.content.push_str(SEPARATOR);
        self.content.push('\n');
    }

    /// True when nothing has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// The accumulated golden content.
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Writes the accumulated content to `path`, creating parent directories
    /// as needed.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        ensure_parent(path)?;
        fs::write(path, &self.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("vterm-txt-test-{}", std::process::id()));
        p.push(name); // nested: exercises create_dir_all
        p
    }

    #[test]
    fn separator_is_eighty_box_drawing_chars() {
        assert_eq!(SEPARATOR.chars().count(), 80);
        assert!(SEPARATOR.chars().all(|c| c == '─'));
    }

    #[test]
    fn capture_writes_text_with_trailing_newline() {
        let path = tmp_path("capture/screen.txt");
        write_capture(&path, "> echo hello\nhello\n>").unwrap();
        let got = fs::read_to_string(&path).unwrap();
        assert_eq!(got, "> echo hello\nhello\n>\n");
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn capture_does_not_double_trailing_newline() {
        let path = tmp_path("capture2/screen.txt");
        write_capture(&path, "already newline terminated\n").unwrap();
        let got = fs::read_to_string(&path).unwrap();
        assert_eq!(got, "already newline terminated\n");
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn golden_accumulation_matches_vhs_format() {
        let mut golden = GoldenWriter::new();
        assert!(golden.is_empty());

        golden.record("> echo one\none\n>");
        golden.record("> echo two\ntwo\n>");
        assert!(!golden.is_empty());

        let sep = "─".repeat(80);
        let expected = format!("> echo one\none\n>\n{sep}\n> echo two\ntwo\n>\n{sep}\n");
        assert_eq!(golden.content(), expected);

        let path = tmp_path("golden/out.test");
        golden.save(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
