//! Streaming GIF encoder with variable per-frame delays, an adaptive global
//! palette, and dirty-rect sub-frames.
//!
//! Frames are pushed with absolute timestamps as plain RGBA buffers; the
//! encoder holds a single pending frame (one-frame lookahead) so each frame's
//! delay is derived from its successor's timestamp. Frames arriving faster
//! than `max_fps` coalesce into the pending slot; identical consecutive
//! frames are dropped, folding their time into the pending delay. Delay
//! quantization to GIF centiseconds uses error carry so total duration drift
//! stays below 10ms regardless of frame count.
//!
//! # Palette strategy
//!
//! Terminal frames draw from a small color set (theme colors plus
//! antialiasing blends), so the encoder first tries a single **global
//! palette** built adaptively as frames stream in: every unique color gets a
//! fixed index and frames are stored *exactly* (direct index lookup — no
//! quantization, no dithering). The gif container writes the global color
//! table in the file header, but our palette only becomes final at the end,
//! so in this mode frames are buffered **indexed** (1 byte/pixel, ~1/4 the
//! RGBA size) and the file is written in [`GifEncoder::finish`]. Peak memory
//! for a 1200x600, 60-frame tape is ~41 MB.
//!
//! Two events abandon buffering and fall back to streaming with per-frame
//! *local* palettes (frames already buffered are flushed with the
//! accumulated palette as their local table, so they stay exact):
//!
//! - the unique-color count exceeds 256 (gradients, images), after which
//!   each frame is indexed exactly when it fits in 256 colors and
//!   NeuQuant-quantized otherwise;
//! - the buffer grows past [`MAX_BUFFERED_BYTES`].
//!
//! # Dirty rects
//!
//! In both modes, consecutive frames are diffed and only the changed-pixel
//! bounding box is encoded (`DisposalMethod::Keep`), unless the box covers
//! more than 60% of the frame or the frame is the first. In buffered mode
//! the diffing happens at write time, *after* any loop-offset rotation, so
//! deltas always composite correctly.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use gif::{DisposalMethod, Encoder, Frame, Repeat};

use crate::util::ensure_parent;

/// Cap on buffered indexed pixel data. Beyond this the encoder falls back to
/// streaming with per-frame local palettes rather than growing memory
/// unboundedly on very long recordings.
const MAX_BUFFERED_BYTES: usize = 256 * 1024 * 1024;

/// A dirty rect covering more than this fraction of the frame area is
/// written as a full frame instead (the rect bookkeeping isn't worth it).
const FULL_FRAME_AREA_FRACTION: f64 = 0.60;

/// Options for [`GifEncoder`].
#[derive(Debug, Clone)]
pub struct GifOptions {
    /// Frame width in pixels.
    pub width: u16,
    /// Frame height in pixels.
    pub height: u16,
    /// Frames arriving closer than `1/max_fps` after the pending frame's
    /// timestamp coalesce into it. Default 50.
    pub max_fps: f64,
    /// Delays are divided by this factor. Default 1.0.
    pub playback_speed: f64,
    /// Delay assigned to the final frame when the stream ends. Default 1s.
    pub last_frame_hold: Duration,
}

impl GifOptions {
    /// Options for a `width`×`height` GIF with default timing (50 fps cap,
    /// 1.0 speed, 1s last-frame hold).
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            max_fps: 50.0,
            playback_speed: 1.0,
            last_frame_hold: Duration::from_secs(1),
        }
    }
}

/// How frames were palettized, reported in [`GifStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    /// Every frame was indexed exactly against a single global color table
    /// holding this many colors; no quantization or dithering occurred.
    Global(usize),
    /// The recording exceeded 256 unique colors (or the frame buffer's
    /// memory cap), so frames carry local palettes: exact where a frame fits
    /// in 256 colors, NeuQuant-quantized otherwise.
    PerFrame,
}

/// Statistics reported by [`GifEncoder::finish`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GifStats {
    /// Frames actually written to the file.
    pub frames_written: usize,
    /// Pushed frames that coalesced into (or were dropped as identical to)
    /// the pending frame instead of being written.
    pub frames_coalesced: usize,
    /// Total playback duration as encoded (sum of written delays).
    pub duration: Duration,
    /// How frames were palettized.
    pub palette: PaletteMode,
}

struct PendingFrame {
    timestamp: Duration,
    rgba: Vec<u8>,
}

/// Adaptive palette: colors get indices in first-seen order, capped at 256.
#[derive(Default)]
struct Palette {
    map: HashMap<[u8; 3], u8>,
    /// Flat `[r, g, b, ...]`, `map.len() * 3` bytes, in index order.
    colors: Vec<u8>,
}

impl Palette {
    fn len(&self) -> usize {
        self.map.len()
    }

    /// Index for `rgb`, inserting it if the table has room. `None` when the
    /// color is new and the table is already full.
    fn index(&mut self, rgb: [u8; 3]) -> Option<u8> {
        if let Some(&i) = self.map.get(&rgb) {
            return Some(i);
        }
        if self.map.len() >= 256 {
            return None;
        }
        let i = self.map.len() as u8;
        self.map.insert(rgb, i);
        self.colors.extend_from_slice(&rgb);
        Some(i)
    }

    /// Maps `rgba` pixels to palette indices, extending the palette with new
    /// colors (alpha is ignored; terminal frames are opaque). `None` if the
    /// palette would exceed 256 colors — the palette may then hold colors
    /// from the partial attempt, which is harmless: existing indices are
    /// unchanged and callers abandon exact indexing at that point.
    fn index_frame(&mut self, rgba: &[u8]) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(rgba.len() / 4);
        for px in rgba.chunks_exact(4) {
            out.push(self.index([px[0], px[1], px[2]])?);
        }
        Some(out)
    }
}

/// A full-size indexed frame buffered until the global palette is final.
struct IndexedFrame {
    /// One palette index per pixel, `width * height` bytes.
    indices: Vec<u8>,
    delay_cs: u16,
}

/// Where encoded frames go. Starts `Buffered`; may fall back to `Streaming`.
enum Sink {
    /// Global-palette mode: the file exists but its header is unwritten
    /// (the gif crate emits the global color table in `Encoder::new`, and
    /// the palette is still accumulating). Frames are buffered indexed and
    /// written in `finish()`.
    Buffered {
        writer: BufWriter<File>,
        palette: Palette,
        frames: Vec<IndexedFrame>,
    },
    /// Fallback: header already written with no global table; frames stream
    /// out immediately, each carrying a local palette.
    Streaming {
        encoder: Encoder<BufWriter<File>>,
        /// Previous frame's RGBA for dirty-rect diffing. `None` right after
        /// the Buffered→Streaming transition, forcing one full frame.
        prev_rgba: Option<Vec<u8>>,
    },
    /// Transient state during the Buffered→Streaming transition; left behind
    /// permanently if that transition fails partway.
    Poisoned,
}

/// Streaming GIF encoder. See module docs for timing and palette semantics.
pub struct GifEncoder {
    sink: Sink,
    opts: GifOptions,
    pending: Option<PendingFrame>,
    /// `Frame::from_rgba_speed` consumes/overwrites its input buffer, so
    /// pixels are copied here before quantization.
    scratch: Vec<u8>,
    /// Recycled buffer of the last written pending frame; the next pending
    /// frame copies into it, so steady state allocates nothing per push.
    spare: Vec<u8>,
    /// Fractional centiseconds owed to the next written delay.
    delay_error: f64,
    frames_written: usize,
    frames_coalesced: usize,
    /// Sum of written delays, in centiseconds.
    total_delay_cs: u64,
    /// `Set LoopOffset` percentage; 0 means no rotation.
    loop_offset_percent: f64,
}

// Not derivable (`gif::Encoder` inside `Sink` has no `Debug` impl); the
// write statistics are the observable state.
impl std::fmt::Debug for GifEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GifEncoder")
            .field("frames_written", &self.frames_written)
            .field("frames_coalesced", &self.frames_coalesced)
            .field("total_delay_cs", &self.total_delay_cs)
            .finish_non_exhaustive()
    }
}

fn to_io(e: gif::EncodingError) -> io::Error {
    match e {
        gif::EncodingError::Io(e) => e,
        other => io::Error::other(other),
    }
}

/// The error every operation returns once the sink is [`Sink::Poisoned`].
fn poisoned_err() -> io::Error {
    io::Error::other("gif encoder unusable after an earlier write failure")
}

/// Bounding box `(left, top, width, height)` of pixels that differ between
/// two equal-sized buffers of `bpp`-byte pixels, or `None` if identical.
fn diff_rect(
    prev: &[u8],
    cur: &[u8],
    width: usize,
    bpp: usize,
) -> Option<(usize, usize, usize, usize)> {
    debug_assert_eq!(prev.len(), cur.len());
    let row_len = width * bpp;
    let rows = prev.len() / row_len;

    let mut top = None;
    let mut bottom = 0;
    for y in 0..rows {
        let r = y * row_len..(y + 1) * row_len;
        if prev[r.clone()] != cur[r] {
            if top.is_none() {
                top = Some(y);
            }
            bottom = y;
        }
    }
    let top = top?;

    let differs = |row_p: &[u8], row_c: &[u8], x: usize| {
        row_p[x * bpp..(x + 1) * bpp] != row_c[x * bpp..(x + 1) * bpp]
    };
    let mut left = width;
    let mut right = 0;
    for y in top..=bottom {
        let rp = &prev[y * row_len..(y + 1) * row_len];
        let rc = &cur[y * row_len..(y + 1) * row_len];
        if rp == rc {
            continue;
        }
        // Dirty row, so both finds succeed.
        let first = (0..width).find(|&x| differs(rp, rc, x)).unwrap();
        let last = (0..width).rev().find(|&x| differs(rp, rc, x)).unwrap();
        left = left.min(first);
        right = right.max(last);
    }

    Some((left, top, right - left + 1, bottom - top + 1))
}

/// Region of `cur` to encode given its predecessor: `None` means write a
/// full frame (first frame, or the changed box covers >60% of the area);
/// otherwise the changed-pixel bounding box. Identical frames — possible at
/// the loop-offset seam — yield a 1x1 rect that carries the delay without
/// resending pixels.
fn dirty_region(
    prev: Option<&[u8]>,
    cur: &[u8],
    width: usize,
    height: usize,
    bpp: usize,
) -> Option<(usize, usize, usize, usize)> {
    let prev = prev?;
    match diff_rect(prev, cur, width, bpp) {
        None => Some((0, 0, 1, 1)),
        Some((l, t, w, h)) => {
            if (w * h) as f64 > FULL_FRAME_AREA_FRACTION * (width * height) as f64 {
                None
            } else {
                Some((l, t, w, h))
            }
        }
    }
}

/// Copies the `(left, top, width, height)` rect out of a full-width buffer
/// of `bpp`-byte pixels.
fn crop(buf: &[u8], full_width: usize, bpp: usize, rect: (usize, usize, usize, usize)) -> Vec<u8> {
    let (left, top, w, h) = rect;
    let mut out = Vec::with_capacity(w * h * bpp);
    for y in top..top + h {
        let start = (y * full_width + left) * bpp;
        out.extend_from_slice(&buf[start..start + w * bpp]);
    }
    out
}

impl GifEncoder {
    /// Creates `path` (and any missing parent directories). The GIF header
    /// is written lazily — see module docs on the global-palette strategy —
    /// but the file itself is created (and truncated) immediately.
    ///
    /// # Errors
    /// Returns any I/O error from creating the parent directories or the
    /// file.
    pub fn create(path: &Path, opts: GifOptions) -> io::Result<Self> {
        ensure_parent(path)?;

        let writer = BufWriter::new(File::create(path)?);

        Ok(Self {
            sink: Sink::Buffered {
                writer,
                palette: Palette::default(),
                frames: Vec::new(),
            },
            opts,
            pending: None,
            scratch: Vec::new(),
            spare: Vec::new(),
            delay_error: 0.0,
            frames_written: 0,
            frames_coalesced: 0,
            total_delay_cs: 0,
            loop_offset_percent: 0.0,
        })
    }

    /// Rotates playback so the loop starts `percent`% into the timeline
    /// (VHS `Set LoopOffset` semantics): the frames before the offset point
    /// are appended at the end, delays travel with their frames, and total
    /// duration is unchanged. Clamped to 0..=100; 0 disables rotation.
    ///
    /// Requires the global-palette (buffered) path: if encoding falls back
    /// to per-frame palettes (>256 unique colors or the memory cap), frames
    /// have already streamed to disk in original order and
    /// [`GifEncoder::finish`] returns an error.
    pub fn set_loop_offset(&mut self, percent: f64) {
        self.loop_offset_percent = percent.clamp(0.0, 100.0);
    }

    /// Pushes a frame captured at `timestamp` (relative to session start).
    /// `rgba` must be exactly `width * height * 4` bytes.
    ///
    /// # Errors
    /// Returns `InvalidInput` if `rgba` has the wrong length, or any I/O
    /// error from writing the displaced previous frame to the file.
    pub fn push_frame(&mut self, timestamp: Duration, rgba: &[u8]) -> io::Result<()> {
        let expected = self.opts.width as usize * self.opts.height as usize * 4;
        if rgba.len() != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("frame buffer is {} bytes, expected {expected}", rgba.len()),
            ));
        }

        let Some(pending) = self.pending.as_mut() else {
            let mut buf = std::mem::take(&mut self.spare);
            buf.clear();
            buf.extend_from_slice(rgba);
            self.pending = Some(PendingFrame {
                timestamp,
                rgba: buf,
            });
            return Ok(());
        };

        // Identical consecutive frame: drop entirely. Its time folds into the
        // pending delay because the pending timestamp is unchanged and the
        // next distinct frame defines the delay.
        if pending.rgba == rgba {
            self.frames_coalesced += 1;
            return Ok(());
        }

        // Faster than the fps cap: replace pending pixels, keep timestamp.
        let min_interval = Duration::from_secs_f64(1.0 / self.opts.max_fps);
        if timestamp < pending.timestamp + min_interval {
            pending.rgba.clear();
            pending.rgba.extend_from_slice(rgba);
            self.frames_coalesced += 1;
            return Ok(());
        }

        // Successor arrived: write the pending frame with the real delay.
        // write_pending recycles the written buffer into self.spare, which
        // the new pending frame reuses.
        let dt = timestamp - pending.timestamp;
        self.write_pending(dt)?;
        let mut buf = std::mem::take(&mut self.spare);
        buf.clear();
        buf.extend_from_slice(rgba);
        self.pending = Some(PendingFrame {
            timestamp,
            rgba: buf,
        });

        Ok(())
    }

    /// Flushes the pending frame with `last_frame_hold`, applies any loop
    /// offset, and finalizes the file, returning encoding statistics.
    ///
    /// # Errors
    /// Returns any I/O error from writing the remaining frames or
    /// finalizing the file.
    pub fn finish(mut self) -> io::Result<GifStats> {
        let hold = self.opts.last_frame_hold;
        if self.pending.is_some() {
            self.write_pending(hold)?;
        }

        let width = self.opts.width as usize;
        let height = self.opts.height as usize;

        let (mut writer, palette_mode) = match self.sink {
            Sink::Buffered {
                writer,
                palette,
                mut frames,
            } => {
                // Loop offset: rotate frames (with their delays) so playback
                // starts `percent`% into the frame sequence, pre-offset
                // frames appended at the end. Dirty rects are computed below
                // on the *rotated* order, so deltas stay valid; the new
                // first frame is written full.
                if self.loop_offset_percent > 0.0 && frames.len() > 1 {
                    let n = frames.len();
                    let k = ((self.loop_offset_percent / 100.0) * n as f64).round() as usize % n;
                    frames.rotate_left(k);
                }

                let mut encoder =
                    Encoder::new(writer, self.opts.width, self.opts.height, &palette.colors)
                        .map_err(to_io)?;
                encoder.set_repeat(Repeat::Infinite).map_err(to_io)?;
                Self::write_indexed_frames(&mut encoder, &frames, None, width, height)?;
                (encoder.into_inner()?, PaletteMode::Global(palette.len()))
            }
            Sink::Streaming { encoder, .. } => {
                if self.loop_offset_percent > 0.0 {
                    return Err(io::Error::other(
                        "LoopOffset requires the global-palette path (at most 256 unique \
                         colors and a recording small enough to buffer); this recording \
                         fell back to per-frame palettes",
                    ));
                }
                (encoder.into_inner()?, PaletteMode::PerFrame)
            }
            Sink::Poisoned => return Err(poisoned_err()),
        };
        writer.flush()?;

        Ok(GifStats {
            frames_written: self.frames_written,
            frames_coalesced: self.frames_coalesced,
            duration: Duration::from_millis(self.total_delay_cs * 10),
            palette: palette_mode,
        })
    }

    /// Writes the pending frame with the given source-time delay, applying
    /// playback speed and error-carried centisecond quantization.
    fn write_pending(&mut self, dt: Duration) -> io::Result<()> {
        let pending = self
            .pending
            .take()
            .expect("write_pending called without a pending frame");

        let exact_cs = dt.as_secs_f64() / self.opts.playback_speed * 100.0 + self.delay_error;
        let rounded = exact_cs.round();
        self.delay_error = exact_cs - rounded;
        // Browser-safe floor: delays below 2cs are treated as "default speed"
        // by most players.
        let delay_cs = (rounded as i64).clamp(2, u16::MAX as i64) as u16;

        self.encode_frame(&pending.rgba, delay_cs)?;
        // Recycle the flushed buffer for the next pending frame.
        self.spare = pending.rgba;

        self.frames_written += 1;
        self.total_delay_cs += delay_cs as u64;

        Ok(())
    }

    /// Routes one frame into the current sink, handling the fallback
    /// transitions out of buffered mode.
    fn encode_frame(&mut self, rgba: &[u8], delay_cs: u16) -> io::Result<()> {
        if let Sink::Buffered {
            palette, frames, ..
        } = &mut self.sink
        {
            let px = rgba.len() / 4;
            let within_budget = (frames.len() + 1).saturating_mul(px) <= MAX_BUFFERED_BYTES;
            if within_budget && let Some(indices) = palette.index_frame(rgba) {
                frames.push(IndexedFrame { indices, delay_cs });
                return Ok(());
            }
            // Palette overflow or memory cap: flush what we have and stream
            // from here on.
            self.fall_back_to_streaming()?;
        }
        self.stream_frame(rgba, delay_cs)
    }

    /// Abandons buffering: writes the header (no global table) and flushes
    /// all buffered frames with the accumulated palette as a shared *local*
    /// table — their indices reference it unchanged, so they stay exact.
    fn fall_back_to_streaming(&mut self) -> io::Result<()> {
        let Sink::Buffered {
            writer,
            palette,
            frames,
        } = std::mem::replace(&mut self.sink, Sink::Poisoned)
        else {
            return Err(poisoned_err());
        };

        let mut encoder =
            Encoder::new(writer, self.opts.width, self.opts.height, &[]).map_err(to_io)?;
        encoder.set_repeat(Repeat::Infinite).map_err(to_io)?;
        Self::write_indexed_frames(
            &mut encoder,
            &frames,
            Some(&palette.colors),
            self.opts.width as usize,
            self.opts.height as usize,
        )?;

        self.sink = Sink::Streaming {
            encoder,
            // No RGBA predecessor across the transition: the next streamed
            // frame is written full, which is always safe.
            prev_rgba: None,
        };
        Ok(())
    }

    /// Writes full-size indexed frames, delta-cropping each against its
    /// predecessor. `local_palette: Some` attaches it to every frame as its
    /// local table (fallback path); `None` relies on the global table.
    fn write_indexed_frames(
        encoder: &mut Encoder<BufWriter<File>>,
        frames: &[IndexedFrame],
        local_palette: Option<&[u8]>,
        width: usize,
        height: usize,
    ) -> io::Result<()> {
        let mut prev: Option<&[u8]> = None;
        for f in frames {
            let region = dirty_region(prev, &f.indices, width, height, 1);
            let (buffer, left, top, w, h) = match region {
                None => (Cow::Borrowed(f.indices.as_slice()), 0, 0, width, height),
                Some(r) => (
                    Cow::Owned(crop(&f.indices, width, 1, r)),
                    r.0,
                    r.1,
                    r.2,
                    r.3,
                ),
            };

            let frame = Frame {
                delay: f.delay_cs,
                dispose: DisposalMethod::Keep,
                left: left as u16,
                top: top as u16,
                width: w as u16,
                height: h as u16,
                palette: local_palette.map(<[u8]>::to_vec),
                buffer,
                ..Frame::default()
            };
            encoder.write_frame(&frame).map_err(to_io)?;

            prev = Some(&f.indices);
        }
        Ok(())
    }

    /// Streams one frame immediately (fallback mode): dirty-rect cropped,
    /// indexed exactly against a frame-local palette when the region fits in
    /// 256 colors, NeuQuant-quantized otherwise.
    fn stream_frame(&mut self, rgba: &[u8], delay_cs: u16) -> io::Result<()> {
        let width = self.opts.width as usize;
        let height = self.opts.height as usize;
        let Sink::Streaming {
            encoder, prev_rgba, ..
        } = &mut self.sink
        else {
            return Err(poisoned_err());
        };

        let region = dirty_region(prev_rgba.as_deref(), rgba, width, height, 4);
        let (region_buf, left, top, w, h): (Cow<'_, [u8]>, _, _, _, _) = match region {
            None => (Cow::Borrowed(rgba), 0, 0, width, height),
            Some(r) => (Cow::Owned(crop(rgba, width, 4, r)), r.0, r.1, r.2, r.3),
        };

        let mut palette = Palette::default();
        let mut frame = match palette.index_frame(&region_buf) {
            Some(indices) => Frame {
                palette: Some(palette.colors),
                buffer: Cow::Owned(indices),
                ..Frame::default()
            },
            None => {
                self.scratch.clear();
                self.scratch.extend_from_slice(&region_buf);
                Frame::from_rgba_speed(w as u16, h as u16, &mut self.scratch, 10)
            }
        };
        frame.left = left as u16;
        frame.top = top as u16;
        frame.width = w as u16;
        frame.height = h as u16;
        frame.delay = delay_cs;
        frame.dispose = DisposalMethod::Keep;
        encoder.write_frame(&frame).map_err(to_io)?;

        match prev_rgba {
            Some(p) => {
                p.clear();
                p.extend_from_slice(rgba);
            }
            None => *prev_rgba = Some(rgba.to_vec()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(rgb: [u8; 3]) -> Vec<u8> {
        solid_sized(rgb, 8, 8)
    }

    fn solid_sized(rgb: [u8; 3], w: usize, h: usize) -> Vec<u8> {
        let mut buf = Vec::with_capacity(w * h * 4);
        for _ in 0..w * h {
            buf.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        buf
    }

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("vhs_rs-gif-test-{}-{name}", std::process::id()));
        p
    }

    struct DecodedFrame {
        left: u16,
        top: u16,
        width: u16,
        height: u16,
        dispose: DisposalMethod,
        delay: u16,
        /// RGBA of the frame's own rect only.
        buffer: Vec<u8>,
    }

    struct DecodedGif {
        width: u16,
        height: u16,
        repeat: Repeat,
        has_global_palette: bool,
        frames: Vec<DecodedFrame>,
    }

    impl DecodedGif {
        fn delays(&self) -> Vec<u16> {
            self.frames.iter().map(|f| f.delay).collect()
        }

        fn first_pixels(&self) -> Vec<[u8; 4]> {
            self.frames
                .iter()
                .map(|f| [f.buffer[0], f.buffer[1], f.buffer[2], f.buffer[3]])
                .collect()
        }

        /// Composites frames 0..=`upto` (Keep disposal) into a full canvas.
        fn composite(&self, upto: usize) -> Vec<u8> {
            let (cw, ch) = (self.width as usize, self.height as usize);
            let mut canvas = vec![0u8; cw * ch * 4];
            for f in &self.frames[..=upto] {
                let (fw, fh) = (f.width as usize, f.height as usize);
                for y in 0..fh {
                    let dst = ((f.top as usize + y) * cw + f.left as usize) * 4;
                    let src = y * fw * 4;
                    canvas[dst..dst + fw * 4].copy_from_slice(&f.buffer[src..src + fw * 4]);
                }
            }
            canvas
        }
    }

    fn decode(path: &Path) -> DecodedGif {
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = options.read_info(File::open(path).unwrap()).unwrap();
        let (width, height, repeat) = (decoder.width(), decoder.height(), decoder.repeat());
        let has_global_palette = decoder.global_palette().is_some_and(|p| !p.is_empty());

        let mut frames = Vec::new();
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            frames.push(DecodedFrame {
                left: frame.left,
                top: frame.top,
                width: frame.width,
                height: frame.height,
                dispose: frame.dispose,
                delay: frame.delay,
                buffer: frame.buffer.to_vec(),
            });
        }

        DecodedGif {
            width,
            height,
            repeat,
            has_global_palette,
            frames,
        }
    }

    #[test]
    fn coalescing_variable_delays_and_hold() {
        let path = tmp_path("coalesce.gif");
        let mut enc = GifEncoder::create(&path, GifOptions::new(8, 8)).unwrap();

        // 35ms and 40ms arrive <20ms (1/50fps) after the pending frame's
        // timestamp (30ms) and coalesce into it.
        enc.push_frame(ms(0), &solid([255, 0, 0])).unwrap();
        enc.push_frame(ms(30), &solid([0, 255, 0])).unwrap();
        enc.push_frame(ms(35), &solid([0, 0, 255])).unwrap();
        enc.push_frame(ms(40), &solid([255, 255, 0])).unwrap();
        enc.push_frame(ms(3000), &solid([0, 255, 255])).unwrap();

        let stats = enc.finish().unwrap();
        assert_eq!(stats.frames_written, 3);
        assert_eq!(stats.frames_coalesced, 2);
        assert_eq!(stats.palette, PaletteMode::Global(3));

        // Total playback = 3s of source time + 1s hold; drift < 10ms.
        let expected = Duration::from_secs(4);
        let drift = stats.duration.abs_diff(expected);
        assert!(
            drift < ms(10),
            "duration {:?} drifted from 4s",
            stats.duration
        );

        let gif = decode(&path);
        assert_eq!(gif.width, 8);
        assert_eq!(gif.height, 8);
        assert_eq!(gif.repeat, Repeat::Infinite);
        assert_eq!(gif.frames.len(), 3);
        let delays = gif.delays();
        // 0→30ms = 3cs; 30ms→3s = 297cs (within the 296..=300 window);
        // final hold = 100cs.
        assert_eq!(delays[0], 3);
        assert!(
            (296..=300).contains(&delays[1]),
            "long-gap delay was {}cs",
            delays[1]
        );
        assert_eq!(delays[2], 100);

        // The coalesced chain leaves the last replacement's pixels (yellow)
        // as the second written frame. Solid frames change 100% of pixels,
        // so all are full frames and first_pixels sample the real colors.
        let px = gif.first_pixels();
        assert_eq!(px[0], [255, 0, 0, 255]);
        assert_eq!(px[1], [255, 255, 0, 255]);
        assert_eq!(px[2], [0, 255, 255, 255]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn playback_speed_halves_delays() {
        let path = tmp_path("speed.gif");
        let mut opts = GifOptions::new(8, 8);
        opts.playback_speed = 2.0;
        let mut enc = GifEncoder::create(&path, opts).unwrap();

        enc.push_frame(ms(0), &solid([255, 0, 0])).unwrap();
        enc.push_frame(ms(1000), &solid([0, 255, 0])).unwrap();
        enc.push_frame(ms(3000), &solid([0, 0, 255])).unwrap();

        let stats = enc.finish().unwrap();
        assert_eq!(stats.frames_written, 3);
        // (3s source + 1s hold) / 2.0 = 2s.
        assert_eq!(stats.duration, Duration::from_secs(2));

        let gif = decode(&path);
        assert_eq!(gif.delays(), vec![50, 100, 50]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn identical_consecutive_frames_dropped() {
        let path = tmp_path("identical.gif");
        let mut enc = GifEncoder::create(&path, GifOptions::new(8, 8)).unwrap();

        let red = solid([255, 0, 0]);
        enc.push_frame(ms(0), &red).unwrap();
        // Identical frame well past the fps window: dropped entirely, its
        // time folds into the pending delay.
        enc.push_frame(ms(100), &red).unwrap();
        enc.push_frame(ms(3000), &solid([0, 0, 255])).unwrap();

        let stats = enc.finish().unwrap();
        assert_eq!(stats.frames_written, 2);
        assert_eq!(stats.frames_coalesced, 1);

        let gif = decode(&path);
        // Red frame spans the full 0→3s gap.
        assert_eq!(gif.delays(), vec![300, 100]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn error_carry_keeps_total_duration_stable() {
        let path = tmp_path("carry.gif");
        let mut opts = GifOptions::new(8, 8);
        opts.max_fps = 1000.0; // don't coalesce; exercise quantization only
        let mut enc = GifEncoder::create(&path, opts).unwrap();

        // 200 frames, 25ms apart: each delay is exactly 2.5cs. Naive
        // rounding would drift by 0.5cs per frame (~1 full second total);
        // error carry must keep the sum exact.
        for i in 0..200u64 {
            let color = if i % 2 == 0 { [255, 0, 0] } else { [0, 255, 0] };
            enc.push_frame(ms(i * 25), &solid(color)).unwrap();
        }

        let stats = enc.finish().unwrap();
        assert_eq!(stats.frames_written, 200);
        // 199 gaps × 25ms = 4975ms source + 1000ms hold = 5975ms.
        let expected = ms(5975);
        let drift = stats.duration.abs_diff(expected);
        assert!(
            drift < ms(10),
            "duration {:?} drifted from 5975ms",
            stats.duration
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn wrong_buffer_size_is_rejected() {
        let path = tmp_path("badsize.gif");
        let mut enc = GifEncoder::create(&path, GifOptions::new(8, 8)).unwrap();
        let err = enc.push_frame(ms(0), &[0u8; 16]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        drop(enc);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn empty_stream_produces_valid_zero_frame_gif() {
        let path = tmp_path("empty.gif");
        let enc = GifEncoder::create(&path, GifOptions::new(8, 8)).unwrap();
        let stats = enc.finish().unwrap();
        assert_eq!(stats.frames_written, 0);
        assert_eq!(stats.frames_coalesced, 0);
        assert_eq!(stats.duration, Duration::ZERO);
        assert_eq!(stats.palette, PaletteMode::Global(0));
        let gif = decode(&path);
        assert!(gif.frames.is_empty());
        std::fs::remove_file(&path).ok();
    }

    /// A 16x16 patterned frame using 4 of `n` colors, chosen by `phase`.
    fn patterned(phase: usize, n: usize) -> Vec<u8> {
        let colors: Vec<[u8; 3]> = (0..n as u8).map(|i| [i * 20, 255 - i * 20, i]).collect();
        let mut buf = Vec::with_capacity(16 * 16 * 4);
        for y in 0..16 {
            for x in 0..16 {
                let c = colors[(x / 8 + 2 * (y / 8) + phase) % n];
                buf.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        buf
    }

    #[test]
    fn global_palette_exact_roundtrip() {
        let path = tmp_path("global.gif");
        let mut enc = GifEncoder::create(&path, GifOptions::new(16, 16)).unwrap();

        // Frames drawing from ≤10 distinct colors must roundtrip pixel-exact
        // through a single global palette (no quantization, no dithering).
        let frames: Vec<Vec<u8>> = (0..4).map(|p| patterned(p, 10)).collect();
        for (i, f) in frames.iter().enumerate() {
            enc.push_frame(ms(i as u64 * 100), f).unwrap();
        }

        let stats = enc.finish().unwrap();
        assert_eq!(stats.frames_written, 4);
        match stats.palette {
            PaletteMode::Global(n) => assert!(n <= 10, "palette grew to {n} colors"),
            PaletteMode::PerFrame => panic!("expected global palette mode"),
        }

        let gif = decode(&path);
        assert!(gif.has_global_palette, "global color table missing");
        assert_eq!(gif.frames.len(), 4);
        // First frame is always full-size.
        assert_eq!(
            (gif.frames[0].width, gif.frames[0].height),
            (gif.width, gif.height)
        );
        for (i, original) in frames.iter().enumerate() {
            assert_eq!(
                &gif.composite(i),
                original,
                "frame {i} not pixel-exact after roundtrip"
            );
        }

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn dirty_rect_encodes_changed_region_only() {
        let path = tmp_path("dirty.gif");
        let mut enc = GifEncoder::create(&path, GifOptions::new(32, 32)).unwrap();

        // Second frame differs from the first only in an 8x8 block at (8, 16).
        let f0 = solid_sized([10, 20, 30], 32, 32);
        let mut f1 = f0.clone();
        for y in 16..24 {
            for x in 8..16 {
                let o = (y * 32 + x) * 4;
                f1[o..o + 4].copy_from_slice(&[200, 100, 50, 255]);
            }
        }
        enc.push_frame(ms(0), &f0).unwrap();
        enc.push_frame(ms(100), &f1).unwrap();

        let stats = enc.finish().unwrap();
        assert_eq!(stats.frames_written, 2);

        let gif = decode(&path);
        assert_eq!(gif.frames.len(), 2);
        let d1 = &gif.frames[1];
        assert_eq!(
            (d1.left, d1.top, d1.width, d1.height),
            (8, 16, 8, 8),
            "second frame should cover exactly the changed 8x8 block"
        );
        assert_eq!(d1.dispose, DisposalMethod::Keep);
        // Compositing the sub-frame over the first must reproduce f1 exactly.
        assert_eq!(gif.composite(0), f0);
        assert_eq!(gif.composite(1), f1);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn palette_overflow_falls_back_to_per_frame() {
        let path = tmp_path("overflow.gif");
        let mut enc = GifEncoder::create(&path, GifOptions::new(20, 20)).unwrap();

        // First frame: 2 colors — buffered exactly.
        let f0 = patterned_two_colors();
        enc.push_frame(ms(0), &f0).unwrap();
        // Second frame: 400 unique colors — overflows the 256-color cap and
        // forces the per-frame fallback for the whole file.
        let mut f1 = Vec::with_capacity(20 * 20 * 4);
        for i in 0..400u16 {
            f1.extend_from_slice(&[(i % 256) as u8, (i / 256) as u8 * 90 + 10, 77, 255]);
        }
        enc.push_frame(ms(100), &f1).unwrap();
        enc.push_frame(ms(200), &f0).unwrap();

        let stats = enc.finish().unwrap();
        assert_eq!(stats.frames_written, 3);
        assert_eq!(stats.palette, PaletteMode::PerFrame);

        // The file must still decode cleanly with the right geometry, and
        // frames written before the overflow stay pixel-exact (their indices
        // reference the flushed palette as a local table).
        // Note: the gif crate always sets the global-color-table flag (an
        // empty palette is written as zero padding), so `has_global_palette`
        // is not meaningful here; `stats.palette` is the source of truth.
        let gif = decode(&path);
        assert_eq!(gif.frames.len(), 3);
        assert_eq!(gif.composite(0), f0);
        // Third frame (2 colors, after fallback) is exact via its own local
        // palette; the >256-color frame in between is quantized, so composite
        // only the final full state indirectly: frame 2 repaints everything
        // that differs from the quantized frame 1, but pixels equal in f0 and
        // the *quantized* f1 may be left as quantized values. Just verify the
        // decode is structurally sound.
        for f in &gif.frames {
            assert!(f.width as usize * f.height as usize * 4 == f.buffer.len());
        }

        std::fs::remove_file(&path).ok();
    }

    fn patterned_two_colors() -> Vec<u8> {
        let mut buf = Vec::with_capacity(20 * 20 * 4);
        for i in 0..400 {
            let c: [u8; 4] = if i % 2 == 0 {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            };
            buf.extend_from_slice(&c);
        }
        buf
    }

    #[test]
    fn loop_offset_rotates_frames_and_preserves_duration() {
        let path = tmp_path("loopoffset.gif");
        let mut enc = GifEncoder::create(&path, GifOptions::new(8, 8)).unwrap();
        enc.set_loop_offset(50.0);

        let colors = [[255u8, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]];
        for (i, c) in colors.iter().enumerate() {
            enc.push_frame(ms(i as u64 * 100), &solid(*c)).unwrap();
        }

        let stats = enc.finish().unwrap();
        assert_eq!(stats.frames_written, 4);
        // 3 gaps × 100ms + 1s hold = 1.3s, unchanged by rotation.
        assert_eq!(stats.duration, ms(1300));

        let gif = decode(&path);
        // 50% of 4 frames → starts at frame 2: blue, yellow, red, green.
        let px = gif.first_pixels();
        assert_eq!(px[0], [0, 0, 255, 255]);
        assert_eq!(px[1], [255, 255, 0, 255]);
        assert_eq!(px[2], [255, 0, 0, 255]);
        assert_eq!(px[3], [0, 255, 0, 255]);
        // Delays travel with their frames: [10, 100, 10, 10] after rotating
        // [10, 10, 10, 100] by two.
        assert_eq!(gif.delays(), vec![10, 100, 10, 10]);
        assert_eq!(gif.delays().iter().map(|&d| d as u64).sum::<u64>(), 130);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn loop_offset_errors_after_palette_fallback() {
        let path = tmp_path("loopoffset-fallback.gif");
        let mut enc = GifEncoder::create(&path, GifOptions::new(20, 20)).unwrap();
        enc.set_loop_offset(25.0);

        // >256 unique colors forces streaming fallback; frames are already
        // on disk in original order, so LoopOffset cannot be honored.
        let mut f0 = Vec::with_capacity(20 * 20 * 4);
        for i in 0..400u16 {
            f0.extend_from_slice(&[(i % 256) as u8, (i / 256) as u8 * 90 + 10, 77, 255]);
        }
        enc.push_frame(ms(0), &f0).unwrap();
        enc.push_frame(ms(100), &patterned_two_colors()).unwrap();

        let err = enc.finish().unwrap_err();
        assert!(
            err.to_string().contains("LoopOffset"),
            "error should mention LoopOffset: {err}"
        );

        std::fs::remove_file(&path).ok();
    }

    /// Synthetic "terminal typing" frames: dark background, one glyph-sized
    /// colored block added per frame from a small color set.
    fn synth_typing_frames(n: usize, w: usize, h: usize) -> Vec<Vec<u8>> {
        let mut cur = vec![0u8; w * h * 4];
        for px in cur.chunks_mut(4) {
            px.copy_from_slice(&[30, 30, 46, 255]);
        }
        let colors: [[u8; 3]; 4] = [
            [205, 214, 244],
            [243, 139, 168],
            [166, 227, 161],
            [137, 180, 250],
        ];
        let mut frames = Vec::with_capacity(n);
        for i in 0..n {
            let cx = 10 + (i * 9) % (w - 20);
            let cy = 20 + ((i * 9) / (w - 20)) * 20;
            let c = colors[i % colors.len()];
            for y in cy..(cy + 16).min(h) {
                for x in cx..(cx + 8).min(w) {
                    let o = (y * w + x) * 4;
                    cur[o..o + 3].copy_from_slice(&c);
                }
            }
            frames.push(cur.clone());
        }
        frames
    }

    #[test]
    fn synthetic_typing_uses_global_palette_and_roundtrips() {
        let path = tmp_path("typing.gif");
        let frames = synth_typing_frames(10, 120, 80);
        let mut enc = GifEncoder::create(&path, GifOptions::new(120, 80)).unwrap();
        for (i, f) in frames.iter().enumerate() {
            enc.push_frame(ms(i as u64 * 50), f).unwrap();
        }
        let stats = enc.finish().unwrap();
        assert_eq!(stats.palette, PaletteMode::Global(5)); // bg + 4 glyph colors

        let gif = decode(&path);
        assert_eq!(gif.frames.len(), 10);
        // Every frame after the first only repaints a small glyph-sized rect.
        for f in &gif.frames[1..] {
            assert!(
                (f.width as usize) * (f.height as usize) <= 8 * 16,
                "delta frame too large: {}x{}",
                f.width,
                f.height
            );
        }
        for (i, original) in frames.iter().enumerate() {
            assert_eq!(&gif.composite(i), original, "frame {i} mismatch");
        }

        std::fs::remove_file(&path).ok();
    }

    #[test]
    #[ignore = "benchmark; run with --ignored --nocapture"]
    fn bench_encode_30_synthetic_frames() {
        let path = tmp_path("bench.gif");
        let frames = synth_typing_frames(30, 800, 400);
        let start = std::time::Instant::now();
        let mut enc = GifEncoder::create(&path, GifOptions::new(800, 400)).unwrap();
        for (i, f) in frames.iter().enumerate() {
            enc.push_frame(ms(i as u64 * 50), f).unwrap();
        }
        let stats = enc.finish().unwrap();
        let elapsed = start.elapsed();
        let size = std::fs::metadata(&path).unwrap().len();
        eprintln!(
            "bench: {} frames written, {size} bytes, {elapsed:?}, palette {:?}",
            stats.frames_written, stats.palette
        );
        std::fs::remove_file(&path).ok();
    }
}
