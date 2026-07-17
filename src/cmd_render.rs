//! `vhs-rs render`: turn a recorded timeline (`.jsonl` native, `.cast`
//! asciicast v2/v3) into artifacts without executing anything.
//!
//! The recorded **grid** is authoritative: the emulator is sized from the
//! header's cols × rows and the canvas derives from the grid
//! ([`Renderer::for_grid`]). Recorded pixel dimensions are metadata — a
//! font-metrics change across vhs_rs versions may change output dimensions,
//! never content.
//!
//! Retiming: `Hide`→`Show` spans are always cut to zero (VHS semantics),
//! `--idle-limit` caps silent gaps, `--speed` divides delays. Transforms
//! apply to every requested output, `.cast` conversions included.

use std::path::Path;
use std::time::Duration;

use crate::encode::{cast, png, txt};
use crate::error::ExitKind;
use crate::render::{RenderOptions, Renderer};
use crate::replay::{self, ReplaySpec};
use crate::snapshot::{SessionEvent, SessionEventKind};
use crate::term::Term;
use crate::theme::{self, Theme};
use crate::timeline::{self, read_timeline};

/// Everything `vhs-rs render` was asked to do (mirrors the CLI flags).
#[derive(Debug)]
pub struct RenderRequest {
    /// Input timeline: `.jsonl` or `.cast`.
    pub input: String,
    /// Output paths; the extension picks the encoder
    /// (`.gif`/`.png`/`.txt`/`.cast`).
    pub outputs: Vec<String>,
    /// Theme override (builtin name or inline JSON). Overrides the recorded
    /// theme *and* mutes recorded mid-run theme changes.
    pub theme: Option<String>,
    /// Cap on silent gaps between events.
    pub idle_limit: Option<Duration>,
    /// Playback speed override (delays divided by this).
    pub speed: Option<f64>,
    /// GIF frame-rate cap override (clamped to 50, like `Set Framerate`).
    pub framerate: Option<f64>,
    /// Font size override (the canvas re-derives from the grid).
    pub font_size: Option<f32>,
    pub quiet: bool,
}

/// A loaded timeline, normalized across input formats.
struct Loaded {
    cols: usize,
    rows: usize,
    render: RenderOptions,
    theme: Theme,
    theme_timeline: Vec<(Duration, Theme)>,
    cursor_blink: bool,
    max_fps: f64,
    playback_speed: f64,
    loop_offset: Option<f64>,
    shell: Option<String>,
    events: Vec<SessionEvent>,
    /// Non-fatal reader notes (truncated line, skipped codes).
    warning_list: Vec<String>,
}

/// Runs the render; returns the process exit code (0 success, 2 bad
/// input/usage, 4 output write failure).
pub fn render(req: &RenderRequest) -> i32 {
    // Validate the request before doing any work.
    if req.outputs.is_empty() {
        eprintln!("vhs-rs: render needs at least one -o output (.gif/.png/.txt/.cast)");
        return ExitKind::Parse as i32;
    }
    for out in &req.outputs {
        if !matches!(extension(out), ".gif" | ".png" | ".txt" | ".cast") {
            eprintln!(
                "vhs-rs: unsupported render output {out}: expected .gif, .png, .txt, or .cast"
            );
            return ExitKind::Parse as i32;
        }
    }
    let theme_override = match &req.theme {
        None => None,
        Some(spec) => match parse_theme(spec) {
            Some(t) => Some(t),
            None => {
                eprintln!("vhs-rs: unknown theme {spec:?} (builtin name or inline JSON)");
                return ExitKind::Parse as i32;
            }
        },
    };

    let mut loaded = match load(&req.input) {
        Ok(l) => l,
        Err(msg) => {
            eprintln!("vhs-rs: {}: {msg}", req.input);
            return ExitKind::Parse as i32;
        }
    };
    if !req.quiet {
        for w in &loaded.warning_list {
            eprintln!("vhs-rs: warning: {}: {w}", req.input);
        }
    }

    // Apply overrides.
    if let Some(t) = theme_override {
        loaded.theme = t;
        loaded.theme_timeline.clear();
    }
    if let Some(fs) = req.font_size
        && fs > 0.0
    {
        loaded.render.font_size = fs;
    }
    if let Some(s) = req.speed
        && s > 0.0
    {
        loaded.playback_speed = s;
    }
    if let Some(f) = req.framerate
        && f > 0.0
    {
        loaded.max_fps = f.min(50.0);
    }

    // Retime: hidden spans always cut; idle gaps capped on request.
    let mut events = timeline::collapse_hidden(&loaded.events);
    if let Some(limit) = req.idle_limit {
        events = timeline::cap_idle(&events, limit);
    }

    let mut renderer = Renderer::for_grid(
        loaded.render.clone(),
        loaded.theme.clone(),
        loaded.cols,
        loaded.rows,
    );

    let mut exit = ExitKind::Success;
    for out in &req.outputs {
        let path = Path::new(out);
        let result = match extension(out) {
            ".gif" => {
                let spec = ReplaySpec {
                    max_fps: loaded.max_fps,
                    playback_speed: loaded.playback_speed,
                    loop_offset: loaded.loop_offset,
                    cursor_blink: loaded.cursor_blink,
                    initial_theme: loaded.theme.clone(),
                    theme_timeline: loaded.theme_timeline.clone(),
                };
                replay::encode_gif(
                    path,
                    &spec,
                    &mut renderer,
                    &events,
                    (loaded.cols, loaded.rows),
                )
            }
            ".png" => {
                let term = final_screen(&events, loaded.cols, loaded.rows);
                renderer.set_theme(loaded.final_theme());
                png::write_png(path, renderer.render(&term.snapshot()))
            }
            ".txt" => {
                let term = final_screen(&events, loaded.cols, loaded.rows);
                txt::write_capture(path, &term.text())
            }
            ".cast" => cast::write_cast(
                path,
                &cast::CastMeta {
                    cols: loaded.cols,
                    rows: loaded.rows,
                    command: loaded.shell.clone(),
                    title: None,
                    env: vec![("TERM".into(), "xterm-256color".into())],
                },
                &events,
            ),
            _ => unreachable!("outputs validated above"),
        };
        match result {
            Ok(()) => {
                if !req.quiet {
                    eprintln!("  wrote {out}");
                }
            }
            Err(e) => {
                eprintln!("vhs-rs: failed to write {out}: {e}");
                exit = ExitKind::Runtime;
            }
        }
    }

    exit as i32
}

impl Loaded {
    /// The theme in effect at the end of the timeline (final frames).
    fn final_theme(&self) -> Theme {
        self.theme_timeline
            .last()
            .map_or_else(|| self.theme.clone(), |(_, t)| t.clone())
    }
}

/// Feeds the (retimed) events through a fresh emulator and returns the final
/// screen.
fn final_screen(events: &[SessionEvent], cols: usize, rows: usize) -> Term {
    let mut term = Term::new(cols, rows);
    for ev in events {
        match &ev.kind {
            SessionEventKind::Output(s) => term.feed(s),
            SessionEventKind::Resize(c, r) => term.resize(*c, *r),
            SessionEventKind::Visibility(_) | SessionEventKind::Exit => {}
        }
    }
    term
}

fn extension(path: &str) -> &str {
    path.rfind('.').map_or("", |i| &path[i..])
}

fn parse_theme(spec: &str) -> Option<Theme> {
    if spec.trim_start().starts_with('{') {
        theme::from_json(spec).ok()
    } else {
        theme::load_builtin(spec)
    }
}

/// Loads either input format into the normalized shape. `.cast` files carry
/// no style metadata, so they get vhs_rs defaults (override with `--theme`
/// etc.).
fn load(input: &str) -> Result<Loaded, String> {
    match extension(input) {
        ".jsonl" => {
            let t = read_timeline(Path::new(input)).map_err(|e| e.to_string())?;
            Ok(Loaded {
                cols: t.header.cols,
                rows: t.header.rows,
                render: t.header.render,
                theme: t.header.theme,
                theme_timeline: t.theme_timeline,
                cursor_blink: t.header.cursor_blink,
                max_fps: t.header.max_fps,
                playback_speed: t.header.playback_speed,
                loop_offset: t.header.loop_offset,
                shell: Some(t.header.shell),
                events: t.events,
                warning_list: t.warnings,
            })
        }
        ".cast" => {
            let c = cast::read_cast(Path::new(input)).map_err(|e| e.to_string())?;
            Ok(Loaded {
                cols: c.cols,
                rows: c.rows,
                render: RenderOptions::default(),
                theme: theme::default_theme(),
                theme_timeline: Vec::new(),
                cursor_blink: true,
                max_fps: 50.0,
                playback_speed: 1.0,
                loop_offset: None,
                shell: None,
                events: c.events,
                warning_list: c.warnings,
            })
        }
        other => Err(format!(
            "unsupported input format {other:?}: expected .jsonl or .cast"
        )),
    }
}
