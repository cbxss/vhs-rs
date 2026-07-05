//! Streaming GIF encoder with variable per-frame delays.
//!
//! Frames are pushed with absolute timestamps as plain RGBA buffers; the
//! encoder holds a single pending frame (one-frame lookahead) so each frame's
//! delay is derived from its successor's timestamp. Frames arriving faster
//! than `max_fps` coalesce into the pending slot; identical consecutive
//! frames are dropped, folding their time into the pending delay. Delay
//! quantization to GIF centiseconds uses error carry so total duration drift
//! stays below 10ms regardless of frame count.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use gif::{Encoder, Frame, Repeat};

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
        GifOptions {
            width,
            height,
            max_fps: 50.0,
            playback_speed: 1.0,
            last_frame_hold: Duration::from_secs(1),
        }
    }
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
}

struct PendingFrame {
    timestamp: Duration,
    rgba: Vec<u8>,
}

/// Streaming GIF encoder. See module docs for timing semantics.
pub struct GifEncoder {
    encoder: Encoder<BufWriter<File>>,
    opts: GifOptions,
    pending: Option<PendingFrame>,
    /// `Frame::from_rgba_speed` consumes/overwrites its input buffer, so the
    /// pending pixels are copied here before quantization.
    scratch: Vec<u8>,
    /// Fractional centiseconds owed to the next written delay.
    delay_error: f64,
    frames_written: usize,
    frames_coalesced: usize,
    /// Sum of written delays, in centiseconds.
    total_delay_cs: u64,
}

fn to_io(e: gif::EncodingError) -> io::Error {
    match e {
        gif::EncodingError::Io(e) => e,
        other => io::Error::other(other),
    }
}

impl GifEncoder {
    /// Creates `path` (and any missing parent directories) and writes the GIF
    /// header with infinite looping.
    pub fn create(path: &Path, opts: GifOptions) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let file = File::create(path)?;
        let mut encoder =
            Encoder::new(BufWriter::new(file), opts.width, opts.height, &[]).map_err(to_io)?;
        encoder.set_repeat(Repeat::Infinite).map_err(to_io)?;

        Ok(GifEncoder {
            encoder,
            opts,
            pending: None,
            scratch: Vec::new(),
            delay_error: 0.0,
            frames_written: 0,
            frames_coalesced: 0,
            total_delay_cs: 0,
        })
    }

    /// Pushes a frame captured at `timestamp` (relative to session start).
    /// `rgba` must be exactly `width * height * 4` bytes.
    pub fn push_frame(&mut self, timestamp: Duration, rgba: &[u8]) -> io::Result<()> {
        let expected = self.opts.width as usize * self.opts.height as usize * 4;
        if rgba.len() != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("frame buffer is {} bytes, expected {expected}", rgba.len()),
            ));
        }

        let Some(pending) = self.pending.as_mut() else {
            self.pending = Some(PendingFrame {
                timestamp,
                rgba: rgba.to_vec(),
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
        let dt = timestamp - pending.timestamp;
        self.write_pending(dt)?;
        self.pending = Some(PendingFrame {
            timestamp,
            rgba: rgba.to_vec(),
        });

        Ok(())
    }

    /// Flushes the pending frame with `last_frame_hold` and finalizes the
    /// file, returning encoding statistics.
    pub fn finish(mut self) -> io::Result<GifStats> {
        let hold = self.opts.last_frame_hold;
        if self.pending.is_some() {
            self.write_pending(hold)?;
        }

        let mut writer = self.encoder.into_inner()?;
        writer.flush()?;

        Ok(GifStats {
            frames_written: self.frames_written,
            frames_coalesced: self.frames_coalesced,
            duration: Duration::from_millis(self.total_delay_cs * 10),
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

        self.scratch.clear();
        self.scratch.extend_from_slice(&pending.rgba);
        let mut frame =
            Frame::from_rgba_speed(self.opts.width, self.opts.height, &mut self.scratch, 10);
        frame.delay = delay_cs;
        self.encoder.write_frame(&frame).map_err(to_io)?;

        self.frames_written += 1;
        self.total_delay_cs += delay_cs as u64;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(rgb: [u8; 3]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8 * 8 * 4);
        for _ in 0..8 * 8 {
            buf.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        buf
    }

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("vterm-gif-test-{}-{name}", std::process::id()));
        p
    }

    struct DecodedGif {
        width: u16,
        height: u16,
        repeat: Repeat,
        delays: Vec<u16>,
        first_pixels: Vec<[u8; 4]>,
    }

    fn decode(path: &Path) -> DecodedGif {
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = options.read_info(File::open(path).unwrap()).unwrap();
        let (width, height, repeat) = (decoder.width(), decoder.height(), decoder.repeat());

        let mut delays = Vec::new();
        let mut first_pixels = Vec::new();
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            delays.push(frame.delay);
            first_pixels.push([
                frame.buffer[0],
                frame.buffer[1],
                frame.buffer[2],
                frame.buffer[3],
            ]);
        }

        DecodedGif {
            width,
            height,
            repeat,
            delays,
            first_pixels,
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
        assert_eq!(gif.delays.len(), 3);
        // 0→30ms = 3cs; 30ms→3s = 297cs (within the 296..=300 window);
        // final hold = 100cs.
        assert_eq!(gif.delays[0], 3);
        assert!(
            (296..=300).contains(&gif.delays[1]),
            "long-gap delay was {}cs",
            gif.delays[1]
        );
        assert_eq!(gif.delays[2], 100);

        // The coalesced chain leaves the last replacement's pixels (yellow)
        // as the second written frame.
        assert_eq!(gif.first_pixels[0], [255, 0, 0, 255]);
        assert_eq!(gif.first_pixels[1], [255, 255, 0, 255]);
        assert_eq!(gif.first_pixels[2], [0, 255, 255, 255]);

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
        assert_eq!(gif.delays, vec![50, 100, 50]);

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
        assert_eq!(gif.delays.len(), 2);
        // Red frame spans the full 0→3s gap.
        assert_eq!(gif.delays[0], 300);
        assert_eq!(gif.delays[1], 100);

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
        std::fs::remove_file(&path).ok();
    }
}
