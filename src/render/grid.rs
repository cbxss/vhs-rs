//! Cell grid drawing: backgrounds, glyphs, text decorations, and the cursor.

use crate::render::font::{FontSet, Metrics};
use crate::render::renderer::Canvas;
use crate::snapshot::{Cell, Color, GridSnapshot};
use crate::theme::{Rgb, Theme};

/// Linear blend of `a` toward `b` by `t` in [0, 1].
fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Rgb(ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
}

/// Resolves a cell's effective foreground/background colors: defaults from
/// the theme, xterm-style bold brightening of indexed 0-7, inverse swap, and
/// faint blending halfway toward the background.
fn cell_colors(cell: &Cell, theme: &Theme) -> (Rgb, Rgb) {
    let mut fg_src = cell.fg;
    if cell.attrs.bold
        && let Some(Color::Indexed(i @ 0..=7)) = fg_src
    {
        fg_src = Some(Color::Indexed(i + 8));
    }
    let mut fg = fg_src.map_or(theme.foreground, |c| theme.resolve(c));
    let mut bg = cell.bg.map_or(theme.background, |c| theme.resolve(c));
    if cell.attrs.inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.attrs.faint {
        fg = mix(fg, bg, 0.5);
    }
    (fg, bg)
}

/// Blends one glyph's coverage bitmap in `color` over the canvas.
///
/// `clip_x` bounds the drawn columns; wide glyphs are clipped to their
/// two-cell box, normal glyphs only to the canvas (so italic overhang
/// survives).
#[allow(clippy::too_many_arguments)]
fn draw_glyph(
    canvas: &mut Canvas,
    fonts: &mut FontSet,
    ch: char,
    bold: bool,
    italic: bool,
    cell_x: i32,
    baseline_y: i32,
    color: Rgb,
    clip_x: Option<(i32, i32)>,
) {
    let glyph = fonts.glyph(ch, bold, italic);
    let gw = glyph.metrics.width as i32;
    let gh = glyph.metrics.height as i32;
    let gx = cell_x + glyph.metrics.xmin;
    // fontdue's ymin is the bottom edge's offset from the baseline, positive
    // up; the bitmap's top row sits (ymin + height) above the baseline.
    let gy = baseline_y - (glyph.metrics.ymin + gh);
    for row in 0..gh {
        let y = gy + row;
        for col in 0..gw {
            let x = gx + col;
            if let Some((lo, hi)) = clip_x
                && (x < lo || x >= hi)
            {
                continue;
            }
            let cov = glyph.bitmap[(row * gw + col) as usize];
            if cov > 0 {
                canvas.blend_px(x, y, color, cov as f32 / 255.0);
            }
        }
    }
}

/// Draws the full snapshot grid with its top-left corner at `origin`.
///
/// `cursor_visible` gates cursor painting in addition to the snapshot's own
/// cursor visibility (used for blink phases).
pub fn draw_grid(
    canvas: &mut Canvas,
    snap: &GridSnapshot,
    theme: &Theme,
    fonts: &mut FontSet,
    metrics: &Metrics,
    origin: (f32, f32),
    cursor_visible: bool,
) {
    let line_thickness = ((metrics.px / 14.0).round() as i32).max(1);

    for row in 0..snap.rows {
        for col in 0..snap.cols {
            let cell = snap.cell(col, row);
            if cell.width == 0 {
                continue; // wide-char continuation cell
            }
            let span = cell.width.max(1) as i32;

            // Cell box in canvas pixels (cell_w/cell_h are whole pixels).
            let x0 = (origin.0 + col as f32 * metrics.cell_w) as i32;
            let y0 = (origin.1 + row as f32 * metrics.cell_h) as i32;
            let x1 = x0 + span * metrics.cell_w as i32;
            let y1 = y0 + metrics.cell_h as i32;

            let (fg, bg) = cell_colors(cell, theme);
            let is_cursor = cursor_visible
                && snap.cursor.visible
                && snap.cursor.row == row
                && snap.cursor.col == col;

            // Background rect; a visible block cursor paints the whole cell
            // in the cursor color and inverts the glyph to the terminal
            // background.
            let ink = if is_cursor {
                canvas.fill_rect(x0, y0, x1, y1, theme.cursor);
                theme.background
            } else {
                if bg != theme.background {
                    canvas.fill_rect(x0, y0, x1, y1, bg);
                }
                fg
            };

            // Glyph (skip blanks); wide glyphs may spill into their
            // continuation cell but no further.
            if cell.ch != ' ' && cell.ch != '\0' {
                let clip = if cell.width == 2 {
                    Some((x0, x1))
                } else {
                    None
                };
                draw_glyph(
                    canvas,
                    fonts,
                    cell.ch,
                    cell.attrs.bold,
                    cell.attrs.italic,
                    x0,
                    y0 + metrics.baseline as i32,
                    ink,
                    clip,
                );
            }

            // Decorations.
            if cell.attrs.underline {
                let uy = y0 + metrics.underline_y as i32;
                canvas.fill_rect(x0, uy, x1, (uy + line_thickness).min(y1), ink);
            }
            if cell.attrs.strikethrough {
                let sy = y0 + metrics.strikeout_y as i32;
                canvas.fill_rect(x0, sy, x1, sy + line_thickness, ink);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{CellAttrs, Cursor};
    use crate::theme::default_theme;

    fn blank_snap(cols: usize, rows: usize) -> GridSnapshot {
        GridSnapshot {
            cols,
            rows,
            cells: vec![Cell::default(); cols * rows],
            cursor: Cursor {
                col: 0,
                row: 0,
                visible: false,
            },
        }
    }

    fn setup() -> (Canvas, FontSet, Metrics, Theme) {
        let fonts = FontSet::new(16.0);
        let metrics = fonts.metrics(1.0, 0.0);
        (Canvas::new(120, 60), fonts, metrics, default_theme())
    }

    fn px_rgb(canvas: &Canvas, x: usize, y: usize) -> Rgb {
        let p = canvas.px(x, y);
        Rgb(p[0], p[1], p[2])
    }

    #[test]
    fn color_resolution_rules() {
        let theme = default_theme();
        // Defaults.
        let cell = Cell::default();
        assert_eq!(
            cell_colors(&cell, &theme),
            (theme.foreground, theme.background)
        );
        // Bold brightens indexed 0-7.
        let cell = Cell {
            fg: Some(Color::Indexed(1)),
            attrs: CellAttrs {
                bold: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(cell_colors(&cell, &theme).0, theme.bright_red);
        // Inverse swaps.
        let cell = Cell {
            fg: Some(Color::Indexed(2)),
            bg: Some(Color::Indexed(4)),
            attrs: CellAttrs {
                inverse: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(cell_colors(&cell, &theme), (theme.blue, theme.green));
        // Faint blends halfway toward the background.
        let cell = Cell {
            fg: Some(Color::Rgb(255, 255, 255)),
            bg: Some(Color::Rgb(0, 0, 0)),
            attrs: CellAttrs {
                faint: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(cell_colors(&cell, &theme).0, Rgb(128, 128, 128));
    }

    #[test]
    fn underline_uses_fg_and_spans_cell() {
        let (mut canvas, mut fonts, metrics, theme) = setup();
        canvas.fill(theme.background);
        let mut snap = blank_snap(2, 1);
        snap.cells[0] = Cell {
            ch: ' ',
            fg: Some(Color::Rgb(10, 200, 30)),
            attrs: CellAttrs {
                underline: true,
                ..Default::default()
            },
            ..Default::default()
        };
        draw_grid(
            &mut canvas,
            &snap,
            &theme,
            &mut fonts,
            &metrics,
            (0.0, 0.0),
            true,
        );
        let uy = metrics.underline_y as usize;
        assert_eq!(px_rgb(&canvas, 1, uy), Rgb(10, 200, 30));
        assert_eq!(
            px_rgb(&canvas, metrics.cell_w as usize - 1, uy),
            Rgb(10, 200, 30)
        );
        // Second cell has no underline.
        assert_eq!(
            px_rgb(&canvas, metrics.cell_w as usize + 1, uy),
            theme.background
        );
    }

    #[test]
    fn bg_rect_painted_for_non_default_background() {
        let (mut canvas, mut fonts, metrics, theme) = setup();
        canvas.fill(theme.background);
        let mut snap = blank_snap(2, 1);
        snap.cells[1] = Cell {
            bg: Some(Color::Indexed(4)),
            ..Default::default()
        };
        draw_grid(
            &mut canvas,
            &snap,
            &theme,
            &mut fonts,
            &metrics,
            (0.0, 0.0),
            true,
        );
        let cw = metrics.cell_w as usize;
        assert_eq!(px_rgb(&canvas, cw + cw / 2, 3), theme.blue);
        assert_eq!(px_rgb(&canvas, cw / 2, 3), theme.background);
    }
}
