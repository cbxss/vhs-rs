//! asciicast v3 writer.
//!
//! Format reference: asciinema's `src/asciicast/v3.rs`. Line 1 is a JSON
//! header (`{"version":3,"term":{"cols":..,"rows":..,"type":".."},...}`,
//! optional fields omitted when empty; no timestamp, for deterministic
//! output). Each following line is a JSON array `[interval, code, data]`
//! where `interval` is the time in seconds since the *previous* event
//! (v3 uses relative times), formatted with 6 decimal places and quantized
//! to whole microseconds with error carry, like asciinema's `Quantizer`.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use crate::snapshot::{SessionEvent, SessionEventKind};
use crate::util::ensure_parent;

/// Session metadata for the asciicast header.
#[derive(Debug, Clone, Default)]
pub struct CastMeta {
    pub cols: usize,
    pub rows: usize,
    /// Command that was recorded (header `command`, omitted when `None`).
    pub command: Option<String>,
    /// Recording title (header `title`, omitted when `None`).
    pub title: Option<String>,
    /// Captured environment variables (header `env`, omitted when empty).
    /// Written in the order given, so output stays deterministic.
    pub env: Vec<(String, String)>,
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).expect("string serialization is infallible")
}

/// Builds the v3 header line (without the trailing newline). Keys are emitted
/// in a fixed order for byte-deterministic output.
fn header_line(meta: &CastMeta) -> String {
    let mut header = format!(
        "{{\"version\":3,\"term\":{{\"cols\":{},\"rows\":{},\"type\":\"xterm-256color\"}}",
        meta.cols, meta.rows
    );

    if let Some(command) = &meta.command {
        header.push_str(",\"command\":");
        header.push_str(&json_str(command));
    }

    if let Some(title) = &meta.title {
        header.push_str(",\"title\":");
        header.push_str(&json_str(title));
    }

    if !meta.env.is_empty() {
        header.push_str(",\"env\":{");
        for (i, (key, value)) in meta.env.iter().enumerate() {
            if i > 0 {
                header.push(',');
            }
            header.push_str(&json_str(key));
            header.push(':');
            header.push_str(&json_str(value));
        }
        header.push('}');
    }

    header.push('}');
    header
}

/// Quantizes intervals to whole microseconds, carrying the rounding error
/// into the next interval so total time never drifts (asciinema's approach).
struct MicroQuantizer {
    error_nanos: i128,
}

impl MicroQuantizer {
    fn new() -> Self {
        Self { error_nanos: 0 }
    }

    fn next_micros(&mut self, dt: Duration) -> u128 {
        let corrected = dt.as_nanos() as i128 + self.error_nanos;
        let micros = (corrected + 500) / 1000; // round to nearest µs
        self.error_nanos = corrected - micros * 1000;
        micros as u128
    }
}

/// Formats a microsecond interval as seconds with 6 decimal places
/// (e.g. `0.500000`).
fn format_interval(micros: u128) -> String {
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

/// Writes an asciicast v3 file to `path` (creating parent directories as
/// needed): the header line, then one `[interval, code, data]` line per
/// event. `Visibility` events are skipped; time between written events is
/// preserved.
///
/// # Errors
/// Returns any I/O error from creating parent directories or writing `path`.
pub fn write_cast(path: &Path, meta: &CastMeta, events: &[SessionEvent]) -> io::Result<()> {
    ensure_parent(path)?;

    let mut out = BufWriter::new(fs::File::create(path)?);
    out.write_all(header_line(meta).as_bytes())?;
    out.write_all(b"\n")?;

    let mut prev_time = Duration::ZERO;
    let mut quantizer = MicroQuantizer::new();

    for event in events {
        let (code, data) = match &event.kind {
            SessionEventKind::Output(data) => ('o', json_str(data)),
            SessionEventKind::Resize(cols, rows) => ('r', json_str(&format!("{cols}x{rows}"))),
            SessionEventKind::Exit => ('x', json_str("")),
            SessionEventKind::Visibility(_) => continue,
        };

        let dt = event.time.saturating_sub(prev_time);
        prev_time = event.time;
        let interval = format_interval(quantizer.next_micros(dt));

        writeln!(out, "[{interval}, \"{code}\", {data}]")?;
    }

    out.flush()
}

// ---- Reader (v2 + v3 import) --------------------------------------------------

/// An imported asciicast, reduced to what vhs_rs replays: the grid and the
/// session events. Codes vhs_rs has no use for (`i` input, `m` markers,
/// unknown extensions) are skipped and noted in `warnings`.
#[derive(Debug)]
pub struct CastImport {
    pub cols: usize,
    pub rows: usize,
    pub events: Vec<SessionEvent>,
    pub warnings: Vec<String>,
}

/// Reading can fail on I/O or a malformed header/event line.
#[derive(Debug, thiserror::Error)]
pub enum CastError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("line {0}: {1}")]
    Parse(usize, String),
}

/// Reads an asciicast v2 or v3 file. The two differ in header keys
/// (v2 `width`/`height`, v3 `term.cols`/`term.rows`) and time semantics
/// (v2 events carry absolute seconds, v3 carries intervals since the
/// previous event); both reduce to absolute-time [`SessionEvent`]s.
///
/// # Errors
/// [`CastError`] on I/O failure or a malformed header or event line.
pub fn read_cast(path: &Path) -> Result<CastImport, CastError> {
    let raw = fs::read_to_string(path)?;
    let mut lines = raw.lines().enumerate();

    let (_, header_line) = lines
        .next()
        .ok_or_else(|| CastError::Parse(1, "empty file: no header line".into()))?;
    let header: serde_json::Value = serde_json::from_str(header_line)
        .map_err(|e| CastError::Parse(1, format!("header is not JSON: {e}")))?;
    let version = header["version"].as_u64().unwrap_or(0);
    let (cols, rows) = match version {
        2 => (&header["width"], &header["height"]),
        3 => (&header["term"]["cols"], &header["term"]["rows"]),
        v => {
            return Err(CastError::Parse(
                1,
                format!("unsupported asciicast version {v} (vhs-rs reads v2 and v3)"),
            ));
        }
    };
    let (cols, rows) = match (cols.as_u64(), rows.as_u64()) {
        (Some(c), Some(r)) if c > 0 && r > 0 => (c as usize, r as usize),
        _ => {
            return Err(CastError::Parse(
                1,
                "header carries no terminal size".into(),
            ));
        }
    };

    let mut import = CastImport {
        cols,
        rows,
        events: Vec::new(),
        warnings: Vec::new(),
    };
    let mut clock = Duration::ZERO; // v3: running absolute time

    for (i, line) in lines {
        if line.trim().is_empty() {
            continue;
        }
        let ev: (f64, String, serde_json::Value) = serde_json::from_str(line)
            .map_err(|e| CastError::Parse(i + 1, format!("not an event triple: {e}")))?;
        let (t, code, data) = ev;
        if !t.is_finite() || t < 0.0 {
            return Err(CastError::Parse(i + 1, format!("bad event time {t}")));
        }
        let time = if version == 2 {
            Duration::from_secs_f64(t)
        } else {
            clock += Duration::from_secs_f64(t);
            clock
        };

        let kind = match code.as_str() {
            "o" => {
                let Some(s) = data.as_str() else {
                    return Err(CastError::Parse(
                        i + 1,
                        "output data is not a string".into(),
                    ));
                };
                SessionEventKind::Output(s.to_string())
            }
            "r" => {
                let size = data.as_str().unwrap_or_default();
                let Some((c, r)) = size
                    .split_once('x')
                    .and_then(|(c, r)| Some((c.parse().ok()?, r.parse().ok()?)))
                else {
                    return Err(CastError::Parse(
                        i + 1,
                        format!("resize data {size:?} is not COLSxROWS"),
                    ));
                };
                SessionEventKind::Resize(c, r)
            }
            "x" => SessionEventKind::Exit,
            other => {
                import
                    .warnings
                    .push(format!("line {}: skipped {other:?} event", i + 1));
                continue;
            }
        };
        import.events.push(SessionEvent { time, kind });
    }

    Ok(import)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn event(time: Duration, kind: SessionEventKind) -> SessionEvent {
        SessionEvent { time, kind }
    }

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("vhs_rs-cast-test-{}", std::process::id()));
        p.push(name); // nested: exercises create_dir_all
        p
    }

    #[test]
    fn golden_output_with_relative_intervals() {
        let path = tmp_path("golden/session.cast");
        let meta = CastMeta {
            cols: 80,
            rows: 24,
            command: None,
            title: None,
            env: Vec::new(),
        };
        let events = vec![
            event(ms(500), SessionEventKind::Output("hello\r\n".into())),
            event(ms(1000), SessionEventKind::Output("world\r\n".into())),
            event(ms(1250), SessionEventKind::Resize(100, 30)),
            event(ms(2000), SessionEventKind::Exit),
        ];

        write_cast(&path, &meta, &events).unwrap();
        let got = fs::read_to_string(&path).unwrap();

        let expected = concat!(
            "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24,\"type\":\"xterm-256color\"}}\n",
            "[0.500000, \"o\", \"hello\\r\\n\"]\n",
            "[0.500000, \"o\", \"world\\r\\n\"]\n",
            "[0.250000, \"r\", \"100x30\"]\n",
            "[0.750000, \"x\", \"\"]\n",
        );
        assert_eq!(got, expected);

        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn every_line_roundtrips_as_json() {
        let path = tmp_path("roundtrip/session.cast");
        let meta = CastMeta {
            cols: 120,
            rows: 40,
            command: Some("bash --noprofile --norc".into()),
            title: Some("demo \"quoted\"".into()),
            env: vec![
                ("TERM".into(), "xterm-256color".into()),
                ("SHELL".into(), "/bin/bash".into()),
            ],
        };
        let events = vec![
            event(
                ms(100),
                SessionEventKind::Output("a\u{1b}[31m✓\u{1b}[0m".into()),
            ),
            event(ms(100), SessionEventKind::Visibility(false)),
            event(ms(350), SessionEventKind::Output("b".into())),
            event(ms(400), SessionEventKind::Exit),
        ];

        write_cast(&path, &meta, &events).unwrap();
        let got = fs::read_to_string(&path).unwrap();
        let mut lines = got.lines();

        // Header parses as JSON with the required v3 fields.
        let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(header["version"], 3);
        assert_eq!(header["term"]["cols"], 120);
        assert_eq!(header["term"]["rows"], 40);
        assert_eq!(header["term"]["type"], "xterm-256color");
        assert_eq!(header["command"], "bash --noprofile --norc");
        assert_eq!(header["title"], "demo \"quoted\"");
        assert_eq!(header["env"]["TERM"], "xterm-256color");
        assert_eq!(header["env"]["SHELL"], "/bin/bash");
        assert!(header.get("timestamp").is_none());

        // Events: Visibility skipped; intervals relative and gap-preserving.
        let event_lines: Vec<Value> = lines.map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(event_lines.len(), 3);

        assert_eq!(event_lines[0][0], 0.1);
        assert_eq!(event_lines[0][1], "o");
        assert_eq!(event_lines[0][2], "a\u{1b}[31m✓\u{1b}[0m");

        // 100ms → 350ms: the skipped Visibility event does not eat time.
        assert_eq!(event_lines[1][0], 0.25);
        assert_eq!(event_lines[1][1], "o");
        assert_eq!(event_lines[1][2], "b");

        assert_eq!(event_lines[2][0], 0.05);
        assert_eq!(event_lines[2][1], "x");
        assert_eq!(event_lines[2][2], "");

        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn header_omits_empty_optionals() {
        let meta = CastMeta {
            cols: 80,
            rows: 24,
            command: None,
            title: None,
            env: Vec::new(),
        };
        let line = header_line(&meta);
        assert_eq!(
            line,
            "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24,\"type\":\"xterm-256color\"}}"
        );
    }

    #[test]
    fn read_cast_v3_round_trips_the_writer() {
        let path = tmp_path("readv3/session.cast");
        let meta = CastMeta {
            cols: 80,
            rows: 24,
            command: None,
            title: None,
            env: Vec::new(),
        };
        let events = vec![
            event(ms(500), SessionEventKind::Output("hello".into())),
            event(ms(1000), SessionEventKind::Resize(100, 30)),
            event(ms(1250), SessionEventKind::Exit),
        ];
        write_cast(&path, &meta, &events).unwrap();

        let import = read_cast(&path).unwrap();
        assert_eq!((import.cols, import.rows), (80, 24));
        assert!(import.warnings.is_empty());
        assert_eq!(import.events, events);

        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn read_cast_v2_absolute_times_and_skipped_codes() {
        let path = tmp_path("readv2/session.cast");
        let v2 = concat!(
            "{\"version\":2,\"width\":20,\"height\":5}\n",
            "[0.5, \"o\", \"hi\"]\n",
            "[0.75, \"i\", \"typed\"]\n",
            "[1.0, \"m\", \"marker\"]\n",
            "[1.5, \"o\", \"bye\"]\n",
            "[2.0, \"r\", \"30x10\"]\n",
        );
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, v2).unwrap();

        let import = read_cast(&path).unwrap();
        assert_eq!((import.cols, import.rows), (20, 5));
        assert_eq!(import.warnings.len(), 2, "{:?}", import.warnings);
        assert_eq!(
            import.events,
            vec![
                event(ms(500), SessionEventKind::Output("hi".into())),
                event(ms(1500), SessionEventKind::Output("bye".into())),
                event(ms(2000), SessionEventKind::Resize(30, 10)),
            ]
        );

        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn read_cast_rejects_garbage() {
        let path = tmp_path("readbad/session.cast");
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        fs::write(&path, "{\"version\":1,\"width\":20,\"height\":5}\n").unwrap();
        assert!(matches!(read_cast(&path), Err(CastError::Parse(1, _))));

        fs::write(&path, "{\"version\":2}\n").unwrap();
        assert!(matches!(read_cast(&path), Err(CastError::Parse(1, _))));

        fs::write(
            &path,
            "{\"version\":2,\"width\":20,\"height\":5}\n[0.5, \"o\"]\n",
        )
        .unwrap();
        assert!(matches!(read_cast(&path), Err(CastError::Parse(2, _))));

        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn interval_quantization_carries_sub_microsecond_error() {
        let mut q = MicroQuantizer::new();
        // 1500ns rounds to 2µs with -500ns carried; the next 1500ns becomes
        // 1000ns corrected → 1µs. Total 3µs == exact sum.
        assert_eq!(q.next_micros(Duration::from_nanos(1500)), 2);
        assert_eq!(q.next_micros(Duration::from_nanos(1500)), 1);

        assert_eq!(format_interval(500_000), "0.500000");
        assert_eq!(format_interval(1_000_000), "1.000000");
        assert_eq!(format_interval(123_456_789), "123.456789");
        assert_eq!(format_interval(0), "0.000000");
    }
}
