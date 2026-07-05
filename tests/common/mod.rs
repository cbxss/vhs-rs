//! Helpers shared across integration test binaries.

// Each test binary compiles this module independently and uses a different
// subset of it.
#![allow(dead_code)]

/// RAII temp tape file with a unique absolute path (per process and thread,
/// so parallel test binaries and threads never collide).
pub struct TempTape(std::path::PathBuf);

impl TempTape {
    /// Creates a `.tape` temp file holding `contents`.
    pub fn new(tag: &str, contents: &str) -> Self {
        Self::with_ext(tag, "tape", contents)
    }

    /// Creates a temp file with an explicit extension holding `contents`.
    pub fn with_ext(tag: &str, ext: &str, contents: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "vhs_rs_test_{}_{}_{tag}.{ext}",
            std::process::id(),
            std::thread::current()
                .name()
                .unwrap_or("t")
                .replace("::", "_"),
        ));
        std::fs::write(&path, contents).expect("write temp tape");
        TempTape(path)
    }

    pub fn path(&self) -> &str {
        self.0.to_str().expect("utf-8 temp path")
    }
}

impl Drop for TempTape {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
