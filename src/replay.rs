//! Timeline replay: turns a recorded session event log back into pixels.
//!
//! This is the shared back half of every GIF vhs_rs produces — the batch
//! evaluator and the `render` subcommand both feed a `Vec<SessionEvent>`
//! (live or read back from a `.jsonl`/`.cast` timeline) through
//! [`encode_gif`], which replays it through a fresh emulator, rendering each
//! visible state change into the styled frame and streaming it to the
//! encoder.

use std::path::Path;
use std::time::Duration;

use crate::encode::gif;
use crate::render::Renderer;
use crate::snapshot::{Cursor, GridSnapshot, SessionEvent, SessionEventKind};
use crate::term::Term;
use crate::theme::Theme;

/// Half-period of the synthesized cursor blink (xterm-ish cadence).
pub const BLINK_HALF_PERIOD: Duration = Duration::from_millis(530);
/// Idle gaps longer than this get no synthesized blink frames (degenerate
/// tapes would otherwise balloon the GIF).
pub const BLINK_MAX_GAP: Duration = Duration::from_secs(30);

/// Everything a replay needs beyond the events themselves: the encode-time
/// settings (`Set Framerate/PlaybackSpeed/LoopOffset/CursorBlink` in a tape,
/// header fields in a recorded timeline) and the theme story.
#[derive(Debug, Clone)]
pub struct ReplaySpec {
    /// GIF frame-rate cap (frames closer than `1/max_fps` coalesce).
    pub max_fps: f64,
    /// Delay divisor (`Set PlaybackSpeed` / `render --speed`).
    pub playback_speed: f64,
    /// GIF loop start offset as a percentage, when set.
    pub loop_offset: Option<f64>,
    /// Whether to synthesize cursor blink frames.
    pub cursor_blink: bool,
    /// Theme at t = 0.
    pub initial_theme: Theme,
    /// Mid-run theme changes, in time order.
    pub theme_timeline: Vec<(Duration, Theme)>,
}

/// Replays `events` on a fresh `cols × rows` emulator and writes the GIF to
/// `path`. The renderer's theme is left at whatever the timeline ended on;
/// callers that render more afterwards (final PNG, forensics) restore their
/// own theme.
///
/// # Errors
/// Returns any I/O error from creating or writing the GIF.
pub fn encode_gif(
    path: &Path,
    spec: &ReplaySpec,
    renderer: &mut Renderer,
    events: &[SessionEvent],
    size: (usize, usize),
) -> std::io::Result<()> {
    let (cols, rows) = size;
    let opts = renderer.options().clone();
    let mut term = Term::new(cols, rows);
    let mut enc = gif::GifEncoder::create(
        path,
        gif::GifOptions {
            max_fps: spec.max_fps,
            playback_speed: spec.playback_speed,
            ..gif::GifOptions::new(opts.width as u16, opts.height as u16)
        },
    )?;
    if let Some(p) = spec.loop_offset {
        enc.set_loop_offset(p);
    }

    // Start from the initial theme; apply mid-run changes at their time.
    renderer.set_theme(spec.initial_theme.clone());
    let mut theme_idx = 0;
    let mut visible = true;
    let blink = spec.cursor_blink;
    // Double-buffered snapshots: `last_snap` is the grid of the last pushed
    // frame (valid while `last_time` is `Some`; blink frames synthesize from
    // it), `scratch` receives the next snapshot, and the two swap after each
    // push — so per-event snapshotting allocates nothing at steady state.
    let empty_snap = || GridSnapshot {
        cols: 0,
        rows: 0,
        cells: Vec::new(),
        cursor: Cursor {
            col: 0,
            row: 0,
            visible: false,
        },
    };
    let mut last_snap = empty_snap();
    let mut scratch = empty_snap();
    let mut last_time: Option<Duration> = None;

    // Renders idle-gap blink toggles between the last frame and `until`.
    let synth = |renderer: &mut Renderer,
                 enc: &mut gif::GifEncoder,
                 last: Option<Duration>,
                 snap: &GridSnapshot,
                 until: Duration|
     -> std::io::Result<()> {
        let Some(since) = last else {
            return Ok(());
        };
        if !snap.cursor.visible || until.saturating_sub(since) > BLINK_MAX_GAP {
            return Ok(());
        }
        for (t, on) in blink_frames(since, until, BLINK_HALF_PERIOD) {
            let canvas = renderer.render_frame(snap, on);
            enc.push_frame(t, &canvas.buf)?;
        }
        Ok(())
    };

    let mut end_time = Duration::ZERO;
    for ev in events {
        while theme_idx < spec.theme_timeline.len() && spec.theme_timeline[theme_idx].0 <= ev.time {
            renderer.set_theme(spec.theme_timeline[theme_idx].1.clone());
            theme_idx += 1;
        }
        end_time = end_time.max(ev.time);
        match &ev.kind {
            SessionEventKind::Output(s) => {
                if visible && blink {
                    synth(renderer, &mut enc, last_time, &last_snap, ev.time)?;
                }
                term.feed(s);
                if visible {
                    term.snapshot_into(&mut scratch);
                    let cursor_on = !blink || blink_phase_on(ev.time, BLINK_HALF_PERIOD);
                    let canvas = renderer.render_frame(&scratch, cursor_on);
                    enc.push_frame(ev.time, &canvas.buf)?;
                    std::mem::swap(&mut last_snap, &mut scratch);
                    last_time = Some(ev.time);
                }
            }
            SessionEventKind::Resize(c, r) => {
                term.resize(*c, *r);
                last_time = None; // stale grid; don't synthesize from it
            }
            SessionEventKind::Visibility(v) => visible = *v,
            SessionEventKind::Exit => {
                // Blink through the trailing idle gap before the child exits.
                if visible && blink {
                    synth(renderer, &mut enc, last_time, &last_snap, ev.time)?;
                }
                break;
            }
        }
    }

    // The held final frame must end cursor-visible: re-push the last grid
    // with the cursor on (coalesces if the pending frame already shows it).
    if blink && last_time.is_some() {
        let canvas = renderer.render_frame(&last_snap, true);
        enc.push_frame(end_time, &canvas.buf)?;
    }

    enc.finish()?;
    Ok(())
}

/// Blink phase at absolute session time `t`: `true` = cursor shown.
pub fn blink_phase_on(t: Duration, half_period: Duration) -> bool {
    (t.as_millis() / half_period.as_millis()).is_multiple_of(2)
}

/// Blink toggle boundaries strictly inside `(start, end)`, each paired with
/// the phase that begins there (`true` = cursor shown). Pure so the boundary
/// math is unit-testable; phases align to absolute time, not the gap start,
/// so cadence stays continuous across frames.
pub fn blink_frames(
    start: Duration,
    end: Duration,
    half_period: Duration,
) -> Vec<(Duration, bool)> {
    let half_ms = half_period.as_millis() as u64;
    let mut frames = Vec::new();
    if half_ms == 0 || end <= start {
        return frames;
    }
    let start_ms = start.as_millis() as u64;
    let end_ms = end.as_millis() as u64;
    let mut k = start_ms / half_ms + 1; // first boundary after `start`
    while k * half_ms < end_ms {
        frames.push((Duration::from_millis(k * half_ms), k.is_multiple_of(2)));
        k += 1;
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::RenderOptions;
    use crate::theme;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn blink_phase_follows_half_periods() {
        let half = ms(530);
        // [0, 530) on, [530, 1060) off, [1060, 1590) on, ...
        assert!(blink_phase_on(ms(0), half));
        assert!(blink_phase_on(ms(529), half));
        assert!(!blink_phase_on(ms(530), half));
        assert!(!blink_phase_on(ms(1059), half));
        assert!(blink_phase_on(ms(1060), half));
        assert!(!blink_phase_on(ms(530 * 3), half));
    }

    #[test]
    fn blink_frames_boundary_math() {
        let half = ms(530);

        // Gap shorter than the half-period: no synthesized frames.
        assert!(blink_frames(ms(0), ms(529), half).is_empty());
        assert!(blink_frames(ms(600), ms(1000), half).is_empty());

        // Empty and inverted ranges.
        assert!(blink_frames(ms(100), ms(100), half).is_empty());
        assert!(blink_frames(ms(200), ms(100), half).is_empty());

        // A 2s gap from t=0 crosses boundaries at 530/1060/1590 with
        // alternating phases (off, on, off).
        let frames = blink_frames(ms(0), ms(2000), half);
        assert_eq!(
            frames,
            vec![(ms(530), false), (ms(1060), true), (ms(1590), false)]
        );

        // Boundaries are exclusive at both ends.
        let frames = blink_frames(ms(530), ms(1060), half);
        assert!(frames.is_empty(), "got {frames:?}");

        // Phase aligns to absolute time, not to the gap start: a gap starting
        // mid-phase still toggles at global boundaries.
        let frames = blink_frames(ms(700), ms(1700), half);
        assert_eq!(frames, vec![(ms(1060), true), (ms(1590), false)]);
    }

    /// Replaying the same event log twice must produce byte-identical GIFs,
    /// and the file's structure (dimensions, frame count, delays) must match
    /// what the blink/coalescing rules predict independently.
    #[test]
    fn encode_gif_replay_is_deterministic() {
        let at = |ms: u64, kind: SessionEventKind| SessionEvent {
            time: Duration::from_millis(ms),
            kind,
        };
        // Blink half-period is 530ms: the 100→700ms gap synthesizes an
        // off-frame at 530, the 700→1200 gap an on-frame at 1060; the final
        // cursor-on re-push at 1200 is identical to the pending frame at
        // 1060 and coalesces.
        let events = vec![
            at(0, SessionEventKind::Output("hello".into())),
            at(100, SessionEventKind::Output("x".into())),
            at(700, SessionEventKind::Output("y".into())),
            at(1200, SessionEventKind::Exit),
        ];

        let render = RenderOptions {
            width: 200,
            height: 100,
            padding: 10,
            font_size: 16.0,
            ..RenderOptions::default()
        };
        let spec = ReplaySpec {
            max_fps: 50.0,
            playback_speed: 1.0,
            loop_offset: None,
            cursor_blink: true,
            initial_theme: theme::default_theme(),
            theme_timeline: Vec::new(),
        };
        let mut renderer = Renderer::new(render, theme::default_theme());

        let path = |run: usize| {
            std::env::temp_dir().join(format!(
                "vhs_rs-replay-gif-determinism-{}-{run}.gif",
                std::process::id()
            ))
        };
        for run in 0..2 {
            encode_gif(&path(run), &spec, &mut renderer, &events, (16, 5)).unwrap();
        }
        let bytes0 = std::fs::read(path(0)).unwrap();
        let bytes1 = std::fs::read(path(1)).unwrap();
        assert_eq!(bytes0, bytes1, "two replays produced different files");

        // Structure: frames at 0/100/530/700/1060 written (the 1200 re-push
        // coalesces), delays = successor gaps in centiseconds + 1s hold.
        // `::gif` is the external decoder crate (the local `gif` name is the
        // encoder module imported above).
        let mut options = ::gif::DecodeOptions::new();
        options.set_color_output(::gif::ColorOutput::RGBA);
        let mut decoder = options
            .read_info(std::fs::File::open(path(0)).unwrap())
            .unwrap();
        assert_eq!((decoder.width(), decoder.height()), (200, 100));
        let mut delays = Vec::new();
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            delays.push(frame.delay);
        }
        assert_eq!(delays, vec![10, 43, 17, 36, 100]);

        for run in 0..2 {
            std::fs::remove_file(path(run)).ok();
        }
    }
}
