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
    pub fn new(w: usize, h: usize) -> Self {
        Self {
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
        let old = Rgb(self.buf[i], self.buf[i + 1], self.buf[i + 2]);
        let new = old.lerp(c, a);
        self.buf[i..i + 4].copy_from_slice(&[new.0, new.1, new.2, 0xff]);
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
            "Colorful" => Ok(Self::Colorful),
            "ColorfulRight" => Ok(Self::ColorfulRight),
            "Rings" => Ok(Self::Rings),
            "RingsRight" => Ok(Self::RingsRight),
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
        Self {
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

/// The previous frame's inputs, kept by the renderer for damage diffing.
struct PrevFrame {
    snap: GridSnapshot,
    /// The `cursor_visible` gate the frame was rendered with.
    cursor_gate: bool,
}

/// Renders [`GridSnapshot`]s into a reusable [`Canvas`].
///
/// Consecutive frames of a replay usually differ in one or two cells, so the
/// renderer diffs each snapshot against the previous one and repaints only
/// the damaged rows; anything that invalidates that comparison (first frame,
/// theme change, grid resize, chrome overlapping the grid) falls back to a
/// full redraw. Both paths produce byte-identical canvases.
pub struct Renderer {
    opts: RenderOptions,
    theme: Theme,
    fonts: FontSet,
    metrics: Metrics,
    canvas: Canvas,
    /// Previous frame state; `None` forces the next frame to redraw fully.
    prev: Option<PrevFrame>,
    /// How many frames took the damage path (tests assert it engages).
    #[cfg(test)]
    damage_repaints: usize,
}

// Not derivable (the font set holds non-Debug `fontdue::Font`s); the canvas
// geometry identifies the renderer well enough.
impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("width", &self.opts.width)
            .field("height", &self.opts.height)
            .finish_non_exhaustive()
    }
}

impl Renderer {
    pub fn new(opts: RenderOptions, theme: Theme) -> Self {
        let fonts = FontSet::new(opts.font_size);
        let metrics = fonts.metrics(opts.line_height, opts.letter_spacing);
        let canvas = Canvas::new(opts.width, opts.height);
        Self {
            opts,
            theme,
            fonts,
            metrics,
            canvas,
            prev: None,
            #[cfg(test)]
            damage_repaints: 0,
        }
    }

    pub fn options(&self) -> &RenderOptions {
        &self.opts
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Swaps the active theme (mid-tape `Set Theme`). Invalidates the damage
    /// state: the next frame redraws fully under the new theme.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.prev = None;
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
            self.prev = None;
        }

        // Damage repaint needs a comparable previous frame; take it out of
        // self so its buffers can be reused for the new prev state below.
        let prev = self.prev.take();
        let repainted = match &prev {
            Some(p) if p.snap.cols == snap.cols && p.snap.rows == snap.rows => {
                self.render_damage(p, snap, cursor_visible)
            }
            _ => false,
        };
        if !repainted {
            self.render_full(snap, cursor_visible);
        }
        #[cfg(test)]
        if repainted {
            self.damage_repaints += 1;
        }

        // Remember this frame, reusing the old cell buffer when possible.
        let mut p = prev.unwrap_or(PrevFrame {
            snap: GridSnapshot {
                cols: 0,
                rows: 0,
                cells: Vec::new(),
                cursor: snap.cursor,
            },
            cursor_gate: cursor_visible,
        });
        p.snap.cols = snap.cols;
        p.snap.rows = snap.rows;
        p.snap.cells.clear();
        p.snap.cells.extend_from_slice(&snap.cells);
        p.snap.cursor = snap.cursor;
        p.cursor_gate = cursor_visible;
        self.prev = Some(p);

        &self.canvas
    }

    /// The classic full-frame path: margin fill → rounded window → bar →
    /// every grid cell.
    fn render_full(&mut self, snap: &GridSnapshot, cursor_visible: bool) {
        let o = &self.opts;

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
    }

    /// Damage repaint: diffs `snap` against `prev` (same grid dimensions,
    /// same theme — the caller guarantees both) and repaints only the rows
    /// that changed, byte-identically to what [`Renderer::render_full`]
    /// would produce. Returns `false` without touching the canvas when the
    /// damage cannot be repainted exactly (it would overlap the window bar,
    /// rounded corners, or the margin) — the caller then redraws fully.
    ///
    /// Exactness argument: outside the grid the frame is chrome, constant
    /// while options and theme are; inside, a pixel's final value is the
    /// window background plus the cell fills and glyph blends that touch it,
    /// applied in row-major cell order. For every repainted rect this
    /// re-applies exactly that sequence: refill with the window background,
    /// then redraw (clipped to the rect) every row whose ink can reach it.
    /// Glyphs are never clipped vertically, so ascenders/descenders can
    /// cross row strips ([`grid::row_draw_extent`] measures the real
    /// overhang per row, [`FontSet::glyph_reach`] bounds it globally):
    /// changed rows therefore dilate the damage to every strip their old or
    /// new ink touches, and neighbor rows are redrawn (clipped) so their
    /// spill into the refilled rect is restored.
    fn render_damage(
        &mut self,
        prev: &PrevFrame,
        snap: &GridSnapshot,
        cursor_visible: bool,
    ) -> bool {
        let o = &self.opts;
        let (rows, cols) = (snap.rows, snap.cols);
        let cell_w = self.metrics.cell_w as i32;
        let cell_h = self.metrics.cell_h as i32;
        let bar = self.bar_height() as i32;
        let origin_x = (o.margin + o.padding) as i32;
        let origin_y = o.margin as i32 + bar + o.padding as i32;
        let origin = (origin_x as f32, origin_y as f32);
        let strip_top = |r: usize| origin_y + r as i32 * cell_h;

        // Rows whose cells changed, plus the rows the effective cursor left
        // and entered (covers moves, hide/show, and blink-gate toggles).
        let mut dirty = vec![false; rows];
        let mut any = false;
        for (r, d) in dirty.iter_mut().enumerate() {
            if prev.snap.cells[r * cols..(r + 1) * cols] != snap.cells[r * cols..(r + 1) * cols] {
                *d = true;
                any = true;
            }
        }
        let effective_cursor = |s: &GridSnapshot, gate: bool| {
            (gate && s.cursor.visible && s.cursor.row < rows && s.cursor.col < cols)
                .then_some((s.cursor.row, s.cursor.col))
        };
        let cur_prev = effective_cursor(&prev.snap, prev.cursor_gate);
        let cur_new = effective_cursor(snap, cursor_visible);
        if cur_prev != cur_new {
            for cur in [cur_prev, cur_new].into_iter().flatten() {
                dirty[cur.0] = true;
                any = true;
            }
        }
        if !any {
            return true; // nothing changed; the canvas is already exact
        }

        // Measure the changed rows' old and new ink extents: they define the
        // horizontal fill range (italic overhang may poke into the padding),
        // dilate the dirty set across row strips, and, at the grid's top and
        // bottom edge, extend the fill into the padding.
        let changed: Vec<usize> = (0..rows).filter(|&r| dirty[r]).collect();
        let mut x_lo = origin_x;
        let mut x_hi = origin_x + cols as i32 * cell_w;
        let mut above = origin_y;
        let mut below = origin_y + rows as i32 * cell_h;
        for &r in &changed {
            for s in [&prev.snap, snap] {
                let (ex0, ex1, ey0, ey1) =
                    grid::row_draw_extent(s, &mut self.fonts, &self.metrics, origin, r);
                x_lo = x_lo.min(ex0);
                x_hi = x_hi.max(ex1);
                above = above.min(ey0);
                below = below.max(ey1);
                // Dilate: every strip this row's ink reaches must repaint.
                let first = (ey0 - origin_y).div_euclid(cell_h).max(0) as usize;
                let last = (ey1 - 1 - origin_y).div_euclid(cell_h).min(rows as i32 - 1);
                for d in dirty.iter_mut().take(last.max(0) as usize + 1).skip(first) {
                    *d = true;
                }
            }
        }

        // How many rows above/below a repainted band can spill ink into it.
        // glyph_reach covers every glyph on the canvas: old ones were cached
        // by earlier frames, new ones by the extent scan above.
        let (max_rise, min_ymin) = self.fonts.glyph_reach();
        let baseline = self.metrics.baseline as i32;
        let strike_end = self.metrics.strikeout_y as i32 + self.metrics.line_thickness as i32;
        let reach_up = (max_rise - baseline)
            .max(-(self.metrics.strikeout_y as i32))
            .max(0);
        let reach_down = ((baseline - min_ymin).max(strike_end) - cell_h).max(0);
        let k_up = (reach_down as usize).div_ceil(cell_h.max(1) as usize);
        let k_down = (reach_up as usize).div_ceil(cell_h.max(1) as usize);

        // Contiguous dirty bands with their repaint rects. Interior bands
        // fill exactly their strips; the grid-edge bands extend to the
        // measured overhang so stale ink in the padding is erased too.
        let mut bands: Vec<(usize, usize, i32, i32)> = Vec::new();
        let mut r = 0;
        while r < rows {
            if !dirty[r] {
                r += 1;
                continue;
            }
            let a = r;
            while r < rows && dirty[r] {
                r += 1;
            }
            let b = r - 1;
            let y0 = if a == 0 {
                above.min(strip_top(0))
            } else {
                strip_top(a)
            };
            let y1 = if b == rows - 1 {
                below.max(strip_top(b + 1))
            } else {
                strip_top(b + 1)
            };
            bands.push((a, b, y0, y1));
        }

        // Every repaint rect must lie in the window's plain-background zone:
        // clear of the margin, the window bar, and the rounded corners
        // (corner blending only happens within `radius` of a vertical edge).
        // Otherwise the row repaint can't reproduce the chrome — redraw
        // fully. In practice this triggers only when BorderRadius exceeds
        // Padding or the grid overflows the window.
        let radius = o.border_radius as i32;
        let safe_x0 = o.margin as i32 + radius;
        let safe_x1 = o.width as i32 - o.margin as i32 - radius;
        let safe_y0 = o.margin as i32 + bar;
        let safe_y1 = o.height as i32 - o.margin as i32;
        if x_lo < safe_x0 || x_hi > safe_x1 {
            return false;
        }
        if bands
            .iter()
            .any(|&(_, _, y0, y1)| y0 < safe_y0 || y1 > safe_y1)
        {
            return false;
        }

        // Repaint: refill each band rect with the window background, then
        // redraw the band's rows plus the neighbors that can spill into it,
        // top to bottom, all clipped to the rect.
        for &(a, b, y0, y1) in &bands {
            self.canvas
                .fill_rect(x_lo, y0, x_hi, y1, self.theme.background);
            let clip = grid::Clip {
                x0: x_lo,
                y0,
                x1: x_hi,
                y1,
            };
            for row in a.saturating_sub(k_up)..=(b + k_down).min(rows - 1) {
                grid::draw_row(
                    &mut self.canvas,
                    snap,
                    &self.theme,
                    &mut self.fonts,
                    &self.metrics,
                    origin,
                    cursor_visible,
                    row,
                    Some(&clip),
                );
            }
        }
        true
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
        let (_, rows_plain) = Renderer::new(small_options(), theme).term_size();
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

    /// Scripted ~11-frame sequence exercising the damage-diff hazards:
    /// typing (with italic overhang and descenders that bleed across row
    /// strips), blink-gate toggles, a no-op frame, color changes, a wide
    /// char, cursor row moves, decorations, a row clear (stale-ink erase),
    /// and cursor hide.
    fn scripted_sequence() -> Vec<(GridSnapshot, bool)> {
        let cell = |ch: char, fg: Option<Color>, bg: Option<Color>, attrs: CellAttrs| Cell {
            ch,
            fg,
            bg,
            attrs,
            width: 1,
        };
        let italic = CellAttrs {
            italic: true,
            ..CellAttrs::default()
        };
        let deco = CellAttrs {
            underline: true,
            strikethrough: true,
            ..CellAttrs::default()
        };
        let mut snap = GridSnapshot {
            cols: 12,
            rows: 4,
            cells: vec![Cell::default(); 48],
            cursor: Cursor {
                col: 0,
                row: 0,
                visible: true,
            },
        };
        let mut frames = Vec::new();

        // 1. Blank grid, cursor at origin.
        frames.push((snap.clone(), true));
        // 2. Type "Hig(" — 'g' and '(' descend below the cell box, the
        //    italic 'g' also overhangs sideways.
        snap.cells[0] = cell('H', Some(Color::Indexed(1)), None, CellAttrs::default());
        snap.cells[1] = cell('i', None, None, CellAttrs::default());
        snap.cells[2] = cell('g', None, None, italic);
        snap.cells[3] = cell('(', None, None, CellAttrs::default());
        snap.cursor.col = 4;
        frames.push((snap.clone(), true));
        // 3./4. Cursor blink off, then on again.
        frames.push((snap.clone(), false));
        frames.push((snap.clone(), true));
        // 5. Identical frame: zero damage.
        frames.push((snap.clone(), true));
        // 6. A cell changes colors only.
        snap.cells[1].fg = Some(Color::Rgb(10, 200, 30));
        snap.cells[1].bg = Some(Color::Indexed(4));
        frames.push((snap.clone(), true));
        // 7. Wide char: leading cell width 2, continuation width 0.
        snap.cells[5] = Cell {
            ch: '\u{6F22}',
            width: 2,
            ..Cell::default()
        };
        snap.cells[6].width = 0;
        frames.push((snap.clone(), true));
        // 8. Cursor moves to another row.
        snap.cursor = Cursor {
            col: 2,
            row: 2,
            visible: true,
        };
        frames.push((snap.clone(), true));
        // 9. Decorated cells on row 1 ('y' descends, '|' spans the cell).
        snap.cells[12] = cell('y', Some(Color::Indexed(2)), None, deco);
        snap.cells[13] = cell('|', None, None, CellAttrs::default());
        frames.push((snap.clone(), true));
        // 10. Row 0 cleared: every trace (including descender/overhang ink
        //     outside row 0's strip) must vanish.
        for c in &mut snap.cells[0..12] {
            *c = Cell::default();
        }
        frames.push((snap.clone(), true));
        // 11. Cursor becomes invisible.
        snap.cursor.visible = false;
        frames.push((snap.clone(), true));

        frames
    }

    /// The damage path must be pixel-identical to a from-scratch full redraw
    /// of every frame, across chrome variants (rounded corners inside and
    /// beyond the padding, window bar, tight line height forcing vertical
    /// glyph bleed).
    #[test]
    fn damage_path_matches_full_redraw() {
        let variants: Vec<(&str, RenderOptions, bool)> = vec![
            ("plain", small_options(), true),
            (
                "radius<padding",
                RenderOptions {
                    border_radius: 8,
                    ..small_options()
                },
                true,
            ),
            (
                // Corners reach into the grid: the conservative guard must
                // fall back to full redraws (and still match, trivially).
                "radius>padding",
                RenderOptions {
                    border_radius: 20,
                    ..small_options()
                },
                false,
            ),
            (
                "window-bar",
                RenderOptions {
                    window_bar: Some(BarStyle::Colorful),
                    window_bar_size: 16,
                    ..small_options()
                },
                true,
            ),
            (
                "tight-line-height",
                RenderOptions {
                    line_height: 0.75,
                    ..small_options()
                },
                true,
            ),
            (
                "margin",
                RenderOptions {
                    margin: 6,
                    margin_fill: MarginFill::Color(Rgb(1, 2, 3)),
                    border_radius: 4,
                    ..small_options()
                },
                true,
            ),
        ];

        let theme = default_theme();
        let frames = scripted_sequence();
        for (name, opts, expect_damage) in variants {
            let mut incremental = Renderer::new(opts.clone(), theme.clone());
            for (i, (snap, gate)) in frames.iter().enumerate() {
                let got = incremental.render_frame(snap, *gate).buf.clone();
                // Reference: a brand-new renderer can only redraw fully.
                let want = Renderer::new(opts.clone(), theme.clone())
                    .render_frame(snap, *gate)
                    .buf
                    .clone();
                assert_eq!(got, want, "variant {name:?}: frame {i} diverged");
            }
            if expect_damage {
                assert_eq!(
                    incremental.damage_repaints,
                    frames.len() - 1,
                    "variant {name:?}: damage path did not engage"
                );
            } else {
                // Only the fully identical frame short-circuits (before the
                // guard, which is fine: an unchanged canvas is exact); every
                // real change must fall back to a full redraw.
                assert_eq!(
                    incremental.damage_repaints, 1,
                    "variant {name:?}: guard should force full redraws"
                );
            }
        }
    }

    /// 300-frame synthetic replay: an 80x24 terminal where each frame types
    /// one character (one cell changes plus the cursor advance), snapshotted,
    /// rendered, and pushed to a GIF encoder — the full per-frame hot path.
    #[test]
    #[ignore = "benchmark; run with --ignored --nocapture"]
    fn bench_300_frame_replay() {
        use crate::encode::gif::{GifEncoder, GifOptions};
        use crate::term::Term;
        use std::time::{Duration, Instant};

        // Canvas sized so an 80x24 grid fits (cell 14x22 at defaults).
        let opts = RenderOptions {
            width: 1240,
            height: 650,
            ..RenderOptions::default()
        };
        let mut r = Renderer::new(opts, default_theme());
        let (cols, rows) = r.term_size();
        assert!(cols >= 80 && rows >= 24, "grid {cols}x{rows} too small");

        let mut term = Term::new(80, 24);
        let path =
            std::env::temp_dir().join(format!("vhs_rs-bench-replay-{}.gif", std::process::id()));
        let mut enc = GifEncoder::create(&path, GifOptions::new(1240, 650)).unwrap();

        let start = Instant::now();
        let mut render_time = Duration::ZERO;
        let mut snap = term.snapshot();
        for i in 0..300u64 {
            let ch = (b'a' + (i % 26) as u8) as char;
            term.feed(&ch.to_string());
            let t = Instant::now();
            term.snapshot_into(&mut snap);
            let canvas = r.render_frame(&snap, i % 7 != 0);
            render_time += t.elapsed();
            enc.push_frame(Duration::from_millis(i * 40), &canvas.buf)
                .unwrap();
        }
        let loop_time = start.elapsed();
        let stats = enc.finish().unwrap();
        let total = start.elapsed();
        eprintln!(
            "bench_300_frame_replay: snapshot+render {render_time:?}, render+push \
             {loop_time:?}, total (with finish) {total:?}, frames written {}",
            stats.frames_written
        );
        std::fs::remove_file(&path).ok();
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
