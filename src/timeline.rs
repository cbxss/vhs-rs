//! The `.jsonl` session timeline: vhs_rs's native, streaming, lossless
//! recording format.
//!
//! Line 1 is a header object (`"kind":"header"`) carrying everything a later
//! replay needs — grid size, theme, render options, and the encode-time
//! settings (`Framerate`, `PlaybackSpeed`, `LoopOffset`, `CursorBlink`).
//! Every following line is one event object tagged by `"kind"`: the session
//! events (`output`/`resize`/`visibility`/`exit`), mid-run `theme` changes,
//! and `command` boundary markers. Timestamps are absolute microseconds
//! since session start (`t_us`).
//!
//! The format is append-only and flushed per batch, so a killed process
//! still leaves a renderable file: [`read_timeline`] tolerates a truncated
//! final line and skips event kinds it doesn't know (files from a newer
//! vhs_rs render on an older one, minus the new kinds).
//!
//! [`collapse_hidden`] and [`cap_idle`] are the retime transforms applied
//! before rendering (`Hide`/`Show` spans cut to nothing; `--idle-limit`).

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::render::RenderOptions;
use crate::snapshot::{SessionEvent, SessionEventKind};
use crate::theme::Theme;
use crate::util::ensure_parent;

/// The format version this build writes and the newest it reads.
pub const TIMELINE_VERSION: u32 = 1;

/// Line 1 of a timeline: everything replay needs beyond the events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineHeader {
    pub version: u32,
    pub cols: usize,
    pub rows: usize,
    pub shell: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tape: Option<String>,
    pub theme: Theme,
    pub render: RenderOptions,
    pub cursor_blink: bool,
    pub max_fps: f64,
    pub playback_speed: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_offset: Option<f64>,
}

/// A `command` boundary marker: which tape command produced the events
/// before it, and how it went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandMarker {
    pub index: usize,
    pub line: usize,
    pub command: String,
    pub status: String,
    pub elapsed_ms: u64,
}

/// One timeline line after the header, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TimelineEvent {
    Output {
        t_us: u64,
        data: String,
    },
    Resize {
        t_us: u64,
        cols: usize,
        rows: usize,
    },
    Visibility {
        t_us: u64,
        visible: bool,
    },
    Exit {
        t_us: u64,
    },
    Theme {
        t_us: u64,
        theme: Theme,
    },
    Command {
        t_us: u64,
        index: usize,
        line: usize,
        command: String,
        status: String,
        elapsed_ms: u64,
    },
}

fn t_us(time: Duration) -> u64 {
    time.as_micros() as u64
}

fn from_us(t: u64) -> Duration {
    Duration::from_micros(t)
}

impl TimelineEvent {
    fn from_session(ev: &SessionEvent) -> Self {
        let t = t_us(ev.time);
        match &ev.kind {
            SessionEventKind::Output(data) => Self::Output {
                t_us: t,
                data: data.clone(),
            },
            SessionEventKind::Resize(cols, rows) => Self::Resize {
                t_us: t,
                cols: *cols,
                rows: *rows,
            },
            SessionEventKind::Visibility(visible) => Self::Visibility {
                t_us: t,
                visible: *visible,
            },
            SessionEventKind::Exit => Self::Exit { t_us: t },
        }
    }
}

/// Streaming timeline writer: header at creation, one JSON line per event,
/// flushed on every [`sync`](TimelineWriter::sync) so a killed process
/// loses at most the current batch.
#[derive(Debug)]
pub struct TimelineWriter {
    out: BufWriter<fs::File>,
    /// How many session events have been written (watermark into the
    /// session's event log).
    written: usize,
}

impl TimelineWriter {
    /// Creates `path` (and parents) and writes the header line.
    ///
    /// # Errors
    /// Any I/O error creating or writing the file, or serializing the header.
    pub fn create(path: &Path, header: &TimelineHeader) -> io::Result<Self> {
        ensure_parent(path)?;
        let mut out = BufWriter::new(fs::File::create(path)?);
        let mut v = serde_json::to_value(header).map_err(io::Error::other)?;
        v["kind"] = "header".into();
        writeln!(out, "{v}")?;
        out.flush()?;
        Ok(Self { out, written: 0 })
    }

    /// Writes every session event not yet written (tracks its own watermark
    /// into `events`, which only ever grows) and flushes.
    ///
    /// # Errors
    /// Any I/O error writing the file.
    pub fn sync(&mut self, events: &[SessionEvent]) -> io::Result<()> {
        if self.written >= events.len() {
            return Ok(());
        }
        for ev in &events[self.written..] {
            self.write_line(&TimelineEvent::from_session(ev))?;
        }
        self.written = events.len();
        self.out.flush()
    }

    /// Writes a mid-run theme change and flushes.
    ///
    /// # Errors
    /// Any I/O error writing the file.
    pub fn write_theme(&mut self, time: Duration, theme: &Theme) -> io::Result<()> {
        self.write_line(&TimelineEvent::Theme {
            t_us: t_us(time),
            theme: theme.clone(),
        })?;
        self.out.flush()
    }

    /// Writes a command boundary marker and flushes.
    ///
    /// # Errors
    /// Any I/O error writing the file.
    pub fn write_command(&mut self, time: Duration, marker: &CommandMarker) -> io::Result<()> {
        self.write_line(&TimelineEvent::Command {
            t_us: t_us(time),
            index: marker.index,
            line: marker.line,
            command: marker.command.clone(),
            status: marker.status.clone(),
            elapsed_ms: marker.elapsed_ms,
        })?;
        self.out.flush()
    }

    fn write_line(&mut self, ev: &TimelineEvent) -> io::Result<()> {
        let line = serde_json::to_string(ev).map_err(io::Error::other)?;
        writeln!(self.out, "{line}")
    }
}

/// A fully read timeline, split back into the shapes the replay machinery
/// consumes ([`crate::replay::encode_gif`] takes events + theme timeline).
#[derive(Debug)]
pub struct Timeline {
    pub header: TimelineHeader,
    pub events: Vec<SessionEvent>,
    pub theme_timeline: Vec<(Duration, Theme)>,
    pub markers: Vec<(Duration, CommandMarker)>,
    /// Non-fatal oddities encountered while reading (truncated final line,
    /// unknown event kinds). The caller decides whether to surface them.
    pub warnings: Vec<String>,
}

/// Reading can fail on I/O, a bad header, or a corrupt (non-final) line.
#[derive(Debug, thiserror::Error)]
pub enum TimelineError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("line 1: not a timeline header: {0}")]
    Header(String),
    #[error(
        "timeline version {0} is newer than this vhs-rs supports ({TIMELINE_VERSION}); upgrade vhs-rs"
    )]
    Version(u32),
    #[error("line {0}: corrupt timeline line: {1}")]
    Corrupt(usize, String),
    #[error("empty file: no header line")]
    Empty,
}

/// Reads a `.jsonl` timeline. Tolerates a truncated final line (a killed
/// recorder) and unknown event kinds (a newer writer) — both become
/// [`Timeline::warnings`] instead of errors.
///
/// # Errors
/// [`TimelineError`] on I/O failure, a missing/invalid header, an
/// unsupported version, or a corrupt line that is not the final one.
pub fn read_timeline(path: &Path) -> Result<Timeline, TimelineError> {
    let raw = fs::read_to_string(path)?;
    let lines: Vec<&str> = raw.lines().collect();
    let last_index = lines.len().saturating_sub(1);

    let mut iter = lines.iter().enumerate();
    let Some((_, header_line)) = iter.next() else {
        return Err(TimelineError::Empty);
    };
    let header_value: serde_json::Value =
        serde_json::from_str(header_line).map_err(|e| TimelineError::Header(e.to_string()))?;
    if header_value["kind"] != "header" {
        return Err(TimelineError::Header(format!(
            "kind is {}, expected \"header\"",
            header_value["kind"]
        )));
    }
    let header: TimelineHeader =
        serde_json::from_value(header_value).map_err(|e| TimelineError::Header(e.to_string()))?;
    if header.version > TIMELINE_VERSION {
        return Err(TimelineError::Version(header.version));
    }

    let mut timeline = Timeline {
        header,
        events: Vec::new(),
        theme_timeline: Vec::new(),
        markers: Vec::new(),
        warnings: Vec::new(),
    };

    for (i, line) in iter {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                if i == last_index {
                    timeline
                        .warnings
                        .push(format!("dropped truncated final line {}", i + 1));
                    break;
                }
                return Err(TimelineError::Corrupt(i + 1, e.to_string()));
            }
        };
        match serde_json::from_value::<TimelineEvent>(value.clone()) {
            Ok(ev) => timeline.push(ev),
            Err(e) => {
                // A kind we don't know is forward-compat skippable; a known
                // kind that fails to parse is corruption.
                let kind = value["kind"].as_str().unwrap_or("<missing>");
                let known = matches!(
                    kind,
                    "output" | "resize" | "visibility" | "exit" | "theme" | "command"
                );
                if known {
                    if i == last_index {
                        timeline
                            .warnings
                            .push(format!("dropped truncated final line {}", i + 1));
                        break;
                    }
                    return Err(TimelineError::Corrupt(i + 1, e.to_string()));
                }
                timeline.warnings.push(format!(
                    "line {}: skipped unknown event kind {kind:?}",
                    i + 1
                ));
            }
        }
    }

    Ok(timeline)
}

impl Timeline {
    fn push(&mut self, ev: TimelineEvent) {
        match ev {
            TimelineEvent::Output { t_us, data } => self.events.push(SessionEvent {
                time: from_us(t_us),
                kind: SessionEventKind::Output(data),
            }),
            TimelineEvent::Resize { t_us, cols, rows } => self.events.push(SessionEvent {
                time: from_us(t_us),
                kind: SessionEventKind::Resize(cols, rows),
            }),
            TimelineEvent::Visibility { t_us, visible } => self.events.push(SessionEvent {
                time: from_us(t_us),
                kind: SessionEventKind::Visibility(visible),
            }),
            TimelineEvent::Exit { t_us } => self.events.push(SessionEvent {
                time: from_us(t_us),
                kind: SessionEventKind::Exit,
            }),
            TimelineEvent::Theme { t_us, theme } => {
                self.theme_timeline.push((from_us(t_us), theme));
            }
            TimelineEvent::Command {
                t_us,
                index,
                line,
                command,
                status,
                elapsed_ms,
            } => self.markers.push((
                from_us(t_us),
                CommandMarker {
                    index,
                    line,
                    command,
                    status,
                    elapsed_ms,
                },
            )),
        }
    }
}

// ---- Retime transforms -------------------------------------------------------

/// Cuts every `Visibility(false)` → `Visibility(true)` span to zero duration
/// (VHS semantics: `Hide`/`Show` sections don't exist in the output). Events
/// inside a hidden span keep their order but land at the span's start; an
/// unclosed `Hide` pins everything after it. Times only ever shift earlier,
/// so ordering is preserved.
pub fn collapse_hidden(events: &[SessionEvent]) -> Vec<SessionEvent> {
    let mut out = Vec::with_capacity(events.len());
    let mut shift = Duration::ZERO;
    // Start of the current hidden span in *original* time, if inside one.
    let mut hide_start: Option<Duration> = None;

    for ev in events {
        let adjusted = match hide_start {
            Some(h0) => h0 - shift,
            None => ev.time.saturating_sub(shift),
        };
        match &ev.kind {
            SessionEventKind::Visibility(false) if hide_start.is_none() => {
                hide_start = Some(ev.time);
            }
            SessionEventKind::Visibility(true) => {
                if let Some(h0) = hide_start.take() {
                    shift += ev.time.saturating_sub(h0);
                }
            }
            _ => {}
        }
        out.push(SessionEvent {
            time: adjusted,
            kind: ev.kind.clone(),
        });
    }

    out
}

/// Caps every inter-event gap (including the lead-in before the first
/// event) at `limit`, shifting all later events earlier (`--idle-limit`).
pub fn cap_idle(events: &[SessionEvent], limit: Duration) -> Vec<SessionEvent> {
    let mut out = Vec::with_capacity(events.len());
    let mut shift = Duration::ZERO;
    let mut prev = Duration::ZERO;

    for ev in events {
        let gap = ev.time.saturating_sub(prev);
        if gap > limit {
            shift += gap - limit;
        }
        prev = ev.time;
        out.push(SessionEvent {
            time: ev.time.saturating_sub(shift),
            kind: ev.kind.clone(),
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{self, Rgb};

    fn header() -> TimelineHeader {
        TimelineHeader {
            version: TIMELINE_VERSION,
            cols: 77,
            rows: 21,
            shell: "bash".into(),
            tape: Some("demo.tape".into()),
            theme: theme::default_theme(),
            render: RenderOptions::default(),
            cursor_blink: true,
            max_fps: 50.0,
            playback_speed: 1.0,
            loop_offset: None,
        }
    }

    fn ev(ms: u64, kind: SessionEventKind) -> SessionEvent {
        SessionEvent {
            time: Duration::from_millis(ms),
            kind,
        }
    }

    fn out(ms: u64, s: &str) -> SessionEvent {
        ev(ms, SessionEventKind::Output(s.into()))
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("vhs_rs-timeline-{}-{name}", std::process::id()))
    }

    #[test]
    fn write_read_round_trip() {
        let path = tmp("roundtrip.jsonl");
        let events = vec![
            out(0, "hello"),
            ev(100, SessionEventKind::Resize(100, 30)),
            ev(200, SessionEventKind::Visibility(false)),
            ev(300, SessionEventKind::Visibility(true)),
            out(400, "w\u{1b}[31m✓\u{1b}[0m\"quoted\""),
            ev(500, SessionEventKind::Exit),
        ];

        let mut w = TimelineWriter::create(&path, &header()).unwrap();
        w.sync(&events[..2]).unwrap();
        let mut dracula = theme::load_builtin("dracula").unwrap();
        dracula.cursor = Rgb(1, 2, 3);
        w.write_theme(Duration::from_millis(150), &dracula).unwrap();
        let marker = CommandMarker {
            index: 2,
            line: 7,
            command: "Wait Line".into(),
            status: "ok".into(),
            elapsed_ms: 42,
        };
        w.write_command(Duration::from_millis(160), &marker)
            .unwrap();
        w.sync(&events).unwrap(); // watermark: only the tail is appended
        drop(w);

        let t = read_timeline(&path).unwrap();
        assert!(t.warnings.is_empty(), "{:?}", t.warnings);
        assert_eq!(t.header.cols, 77);
        assert_eq!(t.header.tape.as_deref(), Some("demo.tape"));
        assert_eq!(t.events, events);
        assert_eq!(t.theme_timeline.len(), 1);
        assert_eq!(t.theme_timeline[0].0, Duration::from_millis(150));
        assert_eq!(t.theme_timeline[0].1, dracula);
        assert_eq!(t.markers, vec![(Duration::from_millis(160), marker)]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn truncated_final_line_is_dropped_with_warning() {
        let path = tmp("truncated.jsonl");
        let mut w = TimelineWriter::create(&path, &header()).unwrap();
        w.sync(&[out(0, "a"), out(10, "b")]).unwrap();
        drop(w);

        // Simulate a kill mid-write: chop the last line in half.
        let mut raw = std::fs::read_to_string(&path).unwrap();
        raw.truncate(raw.len() - 15);
        std::fs::write(&path, raw).unwrap();

        let t = read_timeline(&path).unwrap();
        assert_eq!(t.events, vec![out(0, "a")]);
        assert_eq!(t.warnings.len(), 1, "{:?}", t.warnings);
        assert!(t.warnings[0].contains("truncated"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn unknown_kinds_skip_with_warning_but_corrupt_lines_error() {
        let path = tmp("unknown.jsonl");
        let mut w = TimelineWriter::create(&path, &header()).unwrap();
        w.sync(&[out(0, "a")]).unwrap();
        drop(w);

        // A future event kind mid-file: skipped, not fatal.
        let mut raw = std::fs::read_to_string(&path).unwrap();
        raw.push_str("{\"t_us\":5,\"kind\":\"hologram\",\"x\":1}\n");
        raw.push_str("{\"t_us\":9,\"kind\":\"exit\"}\n");
        std::fs::write(&path, &raw).unwrap();
        let t = read_timeline(&path).unwrap();
        assert_eq!(t.events.len(), 2);
        assert!(t.warnings[0].contains("hologram"));

        // A known kind with garbage fields mid-file: corrupt.
        let bad = raw.replace(
            "{\"t_us\":5,\"kind\":\"hologram\",\"x\":1}",
            "{\"t_us\":\"NaN\",\"kind\":\"output\"}",
        );
        std::fs::write(&path, bad).unwrap();
        assert!(matches!(
            read_timeline(&path),
            Err(TimelineError::Corrupt(3, _))
        ));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn header_validation() {
        let path = tmp("header.jsonl");

        std::fs::write(&path, "").unwrap();
        assert!(matches!(read_timeline(&path), Err(TimelineError::Empty)));

        std::fs::write(&path, "{\"kind\":\"output\"}\n").unwrap();
        assert!(matches!(
            read_timeline(&path),
            Err(TimelineError::Header(_))
        ));

        let mut h = header();
        h.version = TIMELINE_VERSION + 1;
        let w = TimelineWriter::create(&path, &h).unwrap();
        drop(w);
        assert!(matches!(
            read_timeline(&path),
            Err(TimelineError::Version(v)) if v == TIMELINE_VERSION + 1
        ));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn collapse_hidden_cuts_spans_to_zero() {
        let ms = Duration::from_millis;
        let events = vec![
            out(0, "before"),
            ev(1000, SessionEventKind::Visibility(false)),
            out(3000, "secret setup"), // inside: lands at the span start
            ev(5000, SessionEventKind::Visibility(true)),
            out(6000, "after"), // 4s hidden span removed → 2000
            ev(7000, SessionEventKind::Exit),
        ];
        let c = collapse_hidden(&events);
        let times: Vec<Duration> = c.iter().map(|e| e.time).collect();
        assert_eq!(
            times,
            vec![ms(0), ms(1000), ms(1000), ms(1000), ms(2000), ms(3000)]
        );
        // Kinds and order are untouched.
        assert_eq!(c[2].kind, events[2].kind);

        // Unclosed Hide pins the tail at the span start.
        let events = vec![
            out(0, "a"),
            ev(1000, SessionEventKind::Visibility(false)),
            out(9000, "hidden forever"),
            ev(10000, SessionEventKind::Exit),
        ];
        let c = collapse_hidden(&events);
        assert_eq!(c[2].time, ms(1000));
        assert_eq!(c[3].time, ms(1000));
    }

    #[test]
    fn cap_idle_caps_gaps_and_lead_in() {
        let ms = Duration::from_millis;
        let events = vec![
            out(5000, "slow start"),   // 5s lead-in → capped to 1s
            out(5100, "quick"),        // 100ms gap: untouched
            out(20000, "after think"), // 14.9s gap → 1s
            ev(20050, SessionEventKind::Exit),
        ];
        let c = cap_idle(&events, ms(1000));
        let times: Vec<Duration> = c.iter().map(|e| e.time).collect();
        assert_eq!(times, vec![ms(1000), ms(1100), ms(2100), ms(2150)]);

        // A generous limit changes nothing.
        let c = cap_idle(&events, Duration::from_secs(60));
        assert_eq!(c, events);
    }

    #[test]
    fn header_json_shape_is_stable() {
        // The documented wire format: kind/version at the top level, theme as
        // the hex object, margin_fill null for theme-background.
        let path = tmp("shape.jsonl");
        let w = TimelineWriter::create(&path, &header()).unwrap();
        drop(w);
        let raw = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert_eq!(v["kind"], "header");
        assert_eq!(v["version"], 1);
        assert_eq!(v["cols"], 77);
        assert_eq!(v["render"]["width"], 1200);
        assert_eq!(v["render"]["margin_fill"], serde_json::Value::Null);
        assert_eq!(v["theme"]["background"], "#171717");
        assert!(v["loop_offset"].is_null() || v.get("loop_offset").is_none());

        std::fs::remove_file(&path).ok();
    }
}
