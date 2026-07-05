//! Central artifact bookkeeping for a run.
//!
//! One owner for everything the evaluator writes to disk: which paths the
//! end-of-run `Output` targets will claim (so mid-run writers can dodge
//! them), every artifact actually written (in write order), same-path
//! double-write detection across ALL artifact classes, and the forensics
//! (`<stem>.failure.txt/.png`) naming. At the end of the run the registry
//! drains into the report, which stays the single external view of the
//! artifact list.

use crate::report::{Artifact, ArtifactKind, ReportBuilder};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub struct ArtifactRegistry {
    /// Planned end-of-run golden (`.txt`/`.ascii`/`.test`) output paths.
    /// Mid-run writers consult this: an end-of-run golden write must not be
    /// clobbered by e.g. a screenshot's text sibling.
    golden_targets: HashSet<String>,
    /// `<stem>` of the `<stem>.failure.txt` / `<stem>.failure.png` pair:
    /// the first output path sans extension, else the tape name sans
    /// extension.
    forensics_stem: String,
    /// Every artifact written, in write order.
    records: Vec<Artifact>,
    /// All paths recorded so far, for double-write detection.
    seen: HashSet<String>,
    /// Paths already warned about (one warning per path, however many
    /// times it is rewritten).
    warned: HashSet<String>,
    quiet: bool,
}

impl ArtifactRegistry {
    /// `planned_outputs` are the `(ext, path)` pairs from the `Output`
    /// pre-pass, in tape order.
    pub fn new(planned_outputs: &[(String, String)], tape_name: &str, quiet: bool) -> Self {
        let golden_targets = planned_outputs
            .iter()
            .filter(|(ext, _)| matches!(ext.as_str(), ".txt" | ".ascii" | ".test"))
            .map(|(_, path)| path.clone())
            .collect();

        let base = planned_outputs
            .first().map_or_else(|| Path::new(tape_name), |(_, p)| Path::new(p));
        let forensics_stem = base
            .with_file_name(base.file_stem().unwrap_or(base.as_os_str()))
            .to_string_lossy()
            .into_owned();

        Self {
            golden_targets,
            forensics_stem,
            records: Vec::new(),
            seen: HashSet::new(),
            warned: HashSet::new(),
            quiet,
        }
    }

    /// Whether any end-of-run golden output is planned (drives per-command
    /// golden recording).
    pub fn has_golden_targets(&self) -> bool {
        !self.golden_targets.is_empty()
    }

    /// Whether `path` is claimed by an end-of-run golden `Output` target.
    pub fn is_golden_target(&self, path: &str) -> bool {
        self.golden_targets.contains(path)
    }

    /// Records a written artifact. Warns (once per path) when any two
    /// artifacts — of any class — land on the same path: the later write
    /// clobbers the earlier one on disk, but both stay in the report.
    pub fn record(
        &mut self,
        path: impl Into<String>,
        kind: ArtifactKind,
        command_index: Option<usize>,
    ) {
        let path = path.into();
        if !self.seen.insert(path.clone()) && self.warned.insert(path.clone()) && !self.quiet {
            eprintln!("vhs-rs: warning: multiple artifacts wrote to {path}; the last write wins");
        }
        self.records.push(Artifact {
            path,
            kind,
            command_index,
        });
    }

    /// The failure-forensics pair: `<stem>.failure.txt` / `<stem>.failure.png`.
    pub fn forensics_paths(&self) -> (PathBuf, PathBuf) {
        (
            PathBuf::from(format!("{}.failure.txt", self.forensics_stem)),
            PathBuf::from(format!("{}.failure.png", self.forensics_stem)),
        )
    }

    /// Hands the artifact list to the report at end of run.
    pub fn drain_into(self, report: &mut ReportBuilder) {
        report.extend_artifacts(self.records);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ExitKind;

    fn reg(outputs: &[(&str, &str)], tape: &str) -> ArtifactRegistry {
        let planned: Vec<(String, String)> = outputs
            .iter()
            .map(|(e, p)| (e.to_string(), p.to_string()))
            .collect();
        ArtifactRegistry::new(&planned, tape, true)
    }

    /// (planned outputs, tape name, expected txt, expected png)
    type ForensicsCase<'a> = (&'a [(&'a str, &'a str)], &'a str, &'a str, &'a str);

    #[test]
    fn forensics_naming() {
        let cases: &[ForensicsCase<'_>] = &[
            // Stem comes from the first output path, extension stripped.
            (
                &[(".png", "report.png")],
                "t.tape",
                "report.failure.txt",
                "report.failure.png",
            ),
            // Only the LAST extension is stripped: `report.png.png` keeps
            // its inner `.png` (the file_stem edge from the review).
            (
                &[(".png", "report.png.png")],
                "t.tape",
                "report.png.failure.txt",
                "report.png.failure.png",
            ),
            // Directories are preserved; later outputs don't matter.
            (
                &[(".gif", "out/movie.gif"), (".txt", "g.txt")],
                "t.tape",
                "out/movie.failure.txt",
                "out/movie.failure.png",
            ),
            // No outputs: fall back to the tape path sans extension.
            (
                &[],
                "examples/demo.tape",
                "examples/demo.failure.txt",
                "examples/demo.failure.png",
            ),
            // Tape name without an extension is used as-is.
            (&[], "noext", "noext.failure.txt", "noext.failure.png"),
        ];
        for (outputs, tape, txt, png) in cases {
            let r = reg(outputs, tape);
            let (t, p) = r.forensics_paths();
            assert_eq!(t, PathBuf::from(txt), "outputs {outputs:?} tape {tape}");
            assert_eq!(p, PathBuf::from(png), "outputs {outputs:?} tape {tape}");
        }
    }

    #[test]
    fn golden_targets_cover_all_text_extensions() {
        let r = reg(
            &[
                (".txt", "a.txt"),
                (".ascii", "b.ascii"),
                (".test", "c.test"),
                (".gif", "d.gif"),
                (".png", "e.png"),
                (".cast", "f.cast"),
            ],
            "t.tape",
        );
        assert!(r.has_golden_targets());
        assert!(r.is_golden_target("a.txt"));
        assert!(r.is_golden_target("b.ascii"));
        assert!(r.is_golden_target("c.test"));
        assert!(!r.is_golden_target("d.gif"));
        assert!(!r.is_golden_target("e.png"));
        assert!(!r.is_golden_target("f.cast"));
        assert!(!r.is_golden_target("unrelated.txt"));

        assert!(!reg(&[(".gif", "d.gif")], "t.tape").has_golden_targets());
        assert!(!reg(&[], "t.tape").has_golden_targets());
    }

    /// Collision matrix: every same-path double-write is flagged, whatever
    /// the artifact classes involved, exactly once per path — and every
    /// write is still recorded.
    #[test]
    fn collision_matrix_warns_once_per_path_and_records_all() {
        let mut r = reg(&[(".txt", "golden.txt")], "t.tape");

        // Distinct paths: no collisions.
        r.record("shot.png", ArtifactKind::Png, Some(0));
        r.record("shot.txt", ArtifactKind::Text, Some(0));
        assert!(r.warned.is_empty());

        // screenshot vs screenshot.
        r.record("shot.png", ArtifactKind::Png, Some(2));
        assert!(r.warned.contains("shot.png"));

        // Third write to the same path: still a single warned entry.
        r.record("shot.png", ArtifactKind::Png, Some(3));
        assert_eq!(r.warned.len(), 1);

        // capture vs golden (different classes, same path).
        r.record("golden.txt", ArtifactKind::Text, Some(4));
        r.record("golden.txt", ArtifactKind::Golden, None);
        assert!(r.warned.contains("golden.txt"));

        // screenshot text sibling vs capture.
        r.record("shot.txt", ArtifactKind::Text, Some(6));
        assert!(r.warned.contains("shot.txt"));

        // Recording always happens; the warning is advisory.
        assert_eq!(r.records.len(), 7);
    }

    #[test]
    fn drains_into_report_in_write_order() {
        let mut r = reg(&[], "t.tape");
        r.record("a.png", ArtifactKind::Png, Some(0));
        r.record("a.txt", ArtifactKind::Text, Some(0));
        r.record("t.failure.txt", ArtifactKind::FailureText, None);

        let mut builder = ReportBuilder::new("t.tape");
        r.drain_into(&mut builder);
        let report = builder.finish(ExitKind::Success);

        let paths: Vec<&str> = report.artifacts.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(paths, ["a.png", "a.txt", "t.failure.txt"]);
        assert_eq!(report.artifacts[0].kind, ArtifactKind::Png);
        assert_eq!(report.artifacts[0].command_index, Some(0));
        assert_eq!(report.artifacts[2].kind, ArtifactKind::FailureText);
        assert_eq!(report.artifacts[2].command_index, None);
    }
}
