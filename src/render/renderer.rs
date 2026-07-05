//! The frame renderer: owns the fonts, theme, layout, and a reusable RGBA
//! canvas, and composites margin -> window -> bar -> grid per frame.

use crate::render::{chrome, font::FontSet, font::Metrics, grid};
use crate::snapshot::GridSnapshot;
use crate::theme::{Rgb, Theme};

/// A shared RGBA8 frame buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    /// Row-major RGBA8, `w * h * 4` bytes.
    pub buf: Vec<u8>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        Canvas {
            w,
            h,
            buf: vec![0; w * h * 4],
        }
    }

    /// Fills the whole canvas with an opaque color.
    pub fn fill(&mut self, c: Rgb) {
        for px in self.buf.chunks_exact_mut(4) {
            px.copy_from_slice(&[c.0, c.1, c.2, 0xff]);
        }
    }

    /// The pixel at (x, y) as RGBA.
    pub fn px(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.w + x) * 4;
        [
            self.buf[i],
            self.buf[i + 1],
            self.buf[i + 2],
            self.buf[i + 3],
        ]
    }

    /// Sets a pixel to an opaque color; out-of-bounds coordinates are ignored.
    pub fn set_px(&mut self, x: i32, y: i32, c: Rgb) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 4;
        self.buf[i..i + 4].copy_from_slice(&[c.0, c.1, c.2, 0xff]);
    }

    /// Fills the half-open rect [x0, x1) x [y0, y1) with an opaque color,
    /// clamped to the canvas.
    pub fn fill_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: Rgb) {
        let x0 = x0.max(0) as usize;
        let y0 = y0.max(0) as usize;
        let x1 = (x1.max(0) as usize).min(self.w);
        let y1 = (y1.max(0) as usize).min(self.h);
        for y in y0..y1 {
            let row = (y * self.w + x0) * 4;
            for px in self.buf[row..row + (x1 - x0) * 4].chunks_exact_mut(4) {
                px.copy_from_slice(&[c.0, c.1, c.2, 0xff]);
            }
        }
    }

    /// Blends `c` over the existing pixel with coverage `a` in [0, 1];
    /// out-of-bounds coordinates are ignored. The result is opaque.
    pub fn blend_px(&mut self, x: i32, y: i32, c: Rgb, a: f32) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let a = a.clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 4;
        if a >= 1.0 {
            self.buf[i..i + 4].copy_from_slice(&[c.0, c.1, c.2, 0xff]);
            return;
        }
        let blend = |old: u8, new: u8| (old as f32 + (new as f32 - old as f32) * a).round() as u8;
        self.buf[i] = blend(self.buf[i], c.0);
        self.buf[i + 1] = blend(self.buf[i + 1], c.1);
        self.buf[i + 2] = blend(self.buf[i + 2], c.2);
        self.buf[i + 3] = 0xff;
    }
}

/// Window bar styles, matching VHS's `Set WindowBar` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarStyle {
    Colorful,
    ColorfulRight,
    Rings,
    RingsRight,
}

impl std::str::FromStr for BarStyle {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Colorful" => Ok(BarStyle::Colorful),
            "ColorfulRight" => Ok(BarStyle::ColorfulRight),
            "Rings" => Ok(BarStyle::Rings),
            "RingsRight" => Ok(BarStyle::RingsRight),
            _ => Err(format!("unknown window bar style {s:?}")),
        }
    }
}

/// What to paint outside the window: a fixed color or the theme background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarginFill {
    Color(Rgb),
    Theme,
}

/// Frame styling options, defaults ported from VHS (vhs/style.go
/// DefaultStyleOptions + vhs/vhs.go DefaultVHSOptions).
#[derive(Debug, Clone, PartialEq)]
pub struct RenderOptions {
    pub width: usize,
    pub height: usize,
    pub padding: usize,
    pub margin: usize,
    pub margin_fill: MarginFill,
    pub window_bar: Option<BarStyle>,
    pub window_bar_size: usize,
    pub border_radius: usize,
    pub font_size: f32,
    pub line_height: f32,
    pub letter_spacing: f32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            width: 1200,                    // VHS defaultWidth
            height: 600,                    // VHS defaultHeight
            padding: 60,                    // VHS defaultPadding
            margin: 0,                      // VHS default
            margin_fill: MarginFill::Theme, // VHS: DefaultTheme.Background
            window_bar: None,               // VHS: WindowBar ""
            window_bar_size: 30,            // VHS defaultWindowBarSize
            border_radius: 0,               // VHS default
            font_size: 22.0,                // VHS defaultFontSize
            line_height: 1.0,               // VHS defaultLineHeight
            letter_spacing: 1.0,            // VHS defaultLetterSpacing (px, xterm.js semantics)
        }
    }
}

/// Renders [`GridSnapshot`]s into a reusable [`Canvas`].
pub struct Renderer {
    opts: RenderOptions,
    theme: Theme,
    fonts: FontSet,
    metrics: Metrics,
    canvas: Canvas,
}

impl Renderer {
    pub fn new(opts: RenderOptions, theme: Theme) -> Renderer {
        let fonts = FontSet::new(opts.font_size);
        let metrics = fonts.metrics(opts.line_height, opts.letter_spacing);
        let canvas = Canvas::new(opts.width, opts.height);
        Renderer {
            opts,
            theme,
            fonts,
            metrics,
            canvas,
        }
    }

    pub fn options(&self) -> &RenderOptions {
        &self.opts
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Swaps the active theme (mid-tape `Set Theme`).
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// Height reserved for the window bar, 0 when no bar is drawn.
    fn bar_height(&self) -> usize {
        if self.opts.window_bar.is_some() {
            self.opts.window_bar_size
        } else {
            0
        }
    }

    /// The terminal grid size implied by the layout — VHS's viewport
    /// derivation (vhs.go Setup): usable pixels are the frame minus padding,
    /// margin, and window bar on each applicable side.
    pub fn term_size(&self) -> (usize, usize) {
        let o = &self.opts;
        let avail_w = o.width.saturating_sub(2 * o.padding + 2 * o.margin);
        let avail_h = o
            .height
            .saturating_sub(2 * o.padding + 2 * o.margin + self.bar_height());
        let cols = (avail_w as f32 / self.metrics.cell_w).floor() as usize;
        let rows = (avail_h as f32 / self.metrics.cell_h).floor() as usize;
        (cols.max(1), rows.max(1))
    }

    /// Renders one frame with the cursor shown (when the snapshot's cursor is
    /// visible).
    pub fn render(&mut self, snap: &GridSnapshot) -> &Canvas {
        self.render_frame(snap, true)
    }

    /// Renders one frame; `cursor_visible` gates cursor painting on top of
    /// the snapshot's own visibility flag (for blink phases in GIFs).
    pub fn render_frame(&mut self, snap: &GridSnapshot, cursor_visible: bool) -> &Canvas {
        let o = &self.opts;
        if self.canvas.w != o.width || self.canvas.h != o.height {
            self.canvas = Canvas::new(o.width, o.height);
        }

        // 1. Margin fill over the whole frame.
        let fill = match o.margin_fill {
            MarginFill::Color(c) => c,
            MarginFill::Theme => self.theme.background,
        };
        chrome::fill_margin(&mut self.canvas, fill);

        // 2. Rounded window over the margin.
        let m = o.margin as i32;
        let rect = (m, m, o.width as i32 - m, o.height as i32 - m);
        let radius = o.border_radius as f32;
        chrome::draw_window(&mut self.canvas, rect, self.theme.background, radius);

        // 3. Window bar (its corners follow the same border radius).
        let bar = self.bar_height();
        if let Some(style) = o.window_bar {
            chrome::draw_window_bar(
                &mut self.canvas,
                rect,
                style,
                bar,
                self.theme.background,
                radius,
            );
        }

        // 4. The cell grid.
        let origin = (
            (o.margin + o.padding) as f32,
            (o.margin + bar + o.padding) as f32,
        );
        grid::draw_grid(
            &mut self.canvas,
            snap,
            &self.theme,
            &mut self.fonts,
            &self.metrics,
            origin,
            cursor_visible,
        );

        &self.canvas
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{Cell, CellAttrs, Color, Cursor, GridSnapshot};
    use crate::theme::default_theme;

    fn rgb(px: [u8; 4]) -> Rgb {
        Rgb(px[0], px[1], px[2])
    }

    /// 4x2 grid: "Hi" in bold red at row 0, cursor on (2, 0).
    fn sample_snapshot() -> GridSnapshot {
        let mut cells = vec![Cell::default(); 8];
        let attrs = CellAttrs {
            bold: true,
            ..CellAttrs::default()
        };
        cells[0] = Cell {
            ch: 'H',
            fg: Some(Color::Indexed(1)),
            bg: None,
            attrs,
            width: 1,
        };
        cells[1] = Cell {
            ch: 'i',
            fg: Some(Color::Indexed(1)),
            bg: None,
            attrs,
            width: 1,
        };
        GridSnapshot {
            cols: 4,
            rows: 2,
            cells,
            cursor: Cursor {
                col: 2,
                row: 0,
                visible: true,
            },
        }
    }

    fn small_options() -> RenderOptions {
        RenderOptions {
            width: 200,
            height: 100,
            padding: 10,
            margin: 0,
            border_radius: 0,
            font_size: 16.0,
            ..RenderOptions::default()
        }
    }

    #[test]
    fn renders_glyphs_background_and_cursor() {
        let theme = default_theme();
        let mut r = Renderer::new(small_options(), theme.clone());
        let m = *r.metrics();
        let canvas = r.render(&sample_snapshot()).clone();

        assert_eq!((canvas.w, canvas.h), (200, 100));
        // A padding pixel is exactly the theme background.
        assert_eq!(rgb(canvas.px(2, 2)), theme.background);

        // Some pixel inside cell (0,0) differs from the background ('H' ink).
        let (cw, ch) = (m.cell_w as usize, m.cell_h as usize);
        let mut inked = false;
        for y in 10..10 + ch {
            for x in 10..10 + cw {
                if rgb(canvas.px(x, y)) != theme.background {
                    inked = true;
                }
            }
        }
        assert!(inked, "glyph cell contains non-background pixels");

        // The cursor cell (2,0) holds a space, so its center is pure cursor
        // color.
        let cx = 10 + 2 * cw + cw / 2;
        let cy = 10 + ch / 2;
        assert_eq!(rgb(canvas.px(cx, cy)), theme.cursor);
    }

    #[test]
    fn cursor_hidden_when_gated_off() {
        let theme = default_theme();
        let mut r = Renderer::new(small_options(), theme.clone());
        let m = *r.metrics();
        let canvas = r.render_frame(&sample_snapshot(), false).clone();
        let cx = 10 + 2 * (m.cell_w as usize) + m.cell_w as usize / 2;
        let cy = 10 + m.cell_h as usize / 2;
        assert_eq!(rgb(canvas.px(cx, cy)), theme.background);
    }

    #[test]
    fn render_is_deterministic() {
        let mut r = Renderer::new(small_options(), default_theme());
        let snap = sample_snapshot();
        let a = r.render(&snap).buf.clone();
        let b = r.render(&snap).buf.clone();
        assert_eq!(a, b, "two renders of the same snapshot are byte-identical");
    }

    #[test]
    fn rounded_corner_shows_margin_fill() {
        let fill = Rgb(1, 2, 3);
        let opts = RenderOptions {
            border_radius: 20,
            margin_fill: MarginFill::Color(fill),
            ..small_options()
        };
        let theme = default_theme();
        let mut r = Renderer::new(opts, theme.clone());
        let canvas = r.render(&sample_snapshot()).clone();
        // The extreme corner lies outside the rounded window.
        assert_eq!(rgb(canvas.px(0, 0)), fill);
        assert_eq!(rgb(canvas.px(199, 99)), fill);
        // Deep inside the window it is the terminal background.
        assert_eq!(rgb(canvas.px(100, 50)), theme.background);
    }

    #[test]
    fn window_bar_shifts_grid_and_draws_dots() {
        let opts = RenderOptions {
            window_bar: Some(BarStyle::Colorful),
            window_bar_size: 30,
            ..small_options()
        };
        let theme = default_theme();
        let mut r = Renderer::new(opts, theme.clone());
        // Bar eats grid rows.
        let (_, rows_with_bar) = r.term_size();
        let (_, rows_plain) = Renderer::new(small_options(), theme.clone()).term_size();
        assert!(rows_with_bar < rows_plain);

        let canvas = r.render(&sample_snapshot()).clone();
        // First (red) dot center per VHS geometry: rad = 30/6 = 5,
        // gap = (30 - 10) / 2 = 10, center = (gap + rad, rad + gap) = (15, 15).
        assert_eq!(rgb(canvas.px(15, 15)), Rgb(0xff, 0x4f, 0x4d));
        // Second dot: spacing = dia + 30/6 = 15 -> x = 30.
        assert_eq!(rgb(canvas.px(30, 15)), Rgb(0xfe, 0xbb, 0x00));
        // Third dot at x = 45.
        assert_eq!(rgb(canvas.px(45, 15)), Rgb(0x00, 0xcc, 0x1d));
    }

    #[test]
    fn term_size_matches_vhs_derivation() {
        let theme = default_theme();
        let r = Renderer::new(RenderOptions::default(), theme);
        let m = r.metrics();
        let (cols, rows) = r.term_size();
        assert_eq!(cols, (1080.0 / m.cell_w).floor() as usize);
        assert_eq!(rows, (480.0 / m.cell_h).floor() as usize);
        assert!(cols >= 70, "default layout should fit a wide grid");
        assert!(rows >= 20);
    }
}
