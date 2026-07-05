//! Window decoration: margin fill, rounded window rect, and window bars.
//!
//! All geometry and colors are ported from VHS's draw.go: the antialiased
//! circle (half-pixel sampling, radius - 1 inner edge), the rounded-rect
//! corner treatment (corner circles of radius + 1), the bar-to-dot ratios,
//! and the traffic-light dot colors.

use crate::render::renderer::{BarStyle, Canvas};
use crate::theme::Rgb;

/// VHS draw.go: dot radius = WindowBarSize / barToDotRatio.
const BAR_TO_DOT_RATIO: i32 = 6;
/// VHS draw.go: ring radius = WindowBarSize / barToDotBorderRatio.
const BAR_TO_DOT_BORDER_RATIO: i32 = 5;

/// VHS colorful-bar dot colors (draw.go makeColorfulBar).
const DOT_RED: Rgb = Rgb(0xff, 0x4f, 0x4d);
const DOT_YELLOW: Rgb = Rgb(0xfe, 0xbb, 0x00);
const DOT_GREEN: Rgb = Rgb(0x00, 0xcc, 0x1d);
/// VHS ring-bar stroke color (draw.go makeRingBar).
const RING: Rgb = Rgb(0x33, 0x33, 0x33);

/// Antialiased circle coverage at pixel (x, y), ported from VHS
/// draw.go circle.At: sample at the pixel center (+0.5), leave one pixel of
/// the radius for the antialiased edge.
fn circle_coverage(x: i32, y: i32, cx: i32, cy: i32, r: i32) -> f32 {
    let xx = (x - cx) as f32 + 0.5;
    let yy = (y - cy) as f32 + 0.5;
    let rr = (r - 1) as f32;
    let dist = (xx * xx + yy * yy).sqrt() - rr;
    if dist < 0.0 {
        1.0
    } else if dist <= 1.0 {
        1.0 - dist
    } else {
        0.0
    }
}

/// Rounded-rect coverage at pixel (x, y) for the half-open rect
/// `(x0, y0, x1, y1)`, ported from VHS draw.go roundedrect.At. Corner circles
/// use radius + 1 so fully-opaque pixels line up with the straight edges.
fn rounded_rect_coverage(x: i32, y: i32, rect: (i32, i32, i32, i32), radius: i32) -> f32 {
    let (x0, y0, x1, y1) = rect;
    if x < x0 || x >= x1 || y < y0 || y >= y1 {
        return 0.0;
    }
    if radius <= 0 {
        return 1.0;
    }
    let r = radius;
    // Corner centers, in the same coordinate convention as VHS (which works
    // in a rect anchored at the origin).
    let corner = if x < x0 + r && y < y0 + r {
        Some((x0 + r, y0 + r))
    } else if x >= x1 - r && y < y0 + r {
        Some((x1 - r, y0 + r))
    } else if x < x0 + r && y >= y1 - r {
        Some((x0 + r, y1 - r))
    } else if x >= x1 - r && y >= y1 - r {
        Some((x1 - r, y1 - r))
    } else {
        None
    };
    match corner {
        Some((cx, cy)) => circle_coverage(x, y, cx, cy, r + 1),
        None => 1.0,
    }
}

/// Fills the entire canvas with the margin color. Painted first each frame.
pub fn fill_margin(canvas: &mut Canvas, margin_fill: Rgb) {
    canvas.fill(margin_fill);
}

/// Draws the terminal window as a rounded rect composited over the existing
/// pixels (the margin fill shows through the shaved corners).
pub fn draw_window(canvas: &mut Canvas, rect: (i32, i32, i32, i32), bg: Rgb, border_radius: f32) {
    let radius = border_radius.round() as i32;
    let (x0, y0, x1, y1) = rect;
    if radius <= 0 {
        canvas.fill_rect(x0, y0, x1, y1, bg);
        return;
    }
    for y in y0.max(0)..y1.min(canvas.h as i32) {
        for x in x0.max(0)..x1.min(canvas.w as i32) {
            let in_corner =
                (x < x0 + radius || x >= x1 - radius) && (y < y0 + radius || y >= y1 - radius);
            if in_corner {
                canvas.blend_px(x, y, bg, rounded_rect_coverage(x, y, rect, radius));
            } else {
                canvas.set_px(x, y, bg);
            }
        }
    }
}

/// Fills an antialiased circle.
fn fill_circle(canvas: &mut Canvas, cx: i32, cy: i32, r: i32, color: Rgb) {
    for y in (cy - r)..=(cy + r) {
        for x in (cx - r)..=(cx + r) {
            let cov = circle_coverage(x, y, cx, cy, r);
            if cov > 0.0 {
                canvas.blend_px(x, y, color, cov);
            }
        }
    }
}

/// Draws the window bar across the top `bar_size` pixels of `rect`.
///
/// Geometry and colors ported from VHS draw.go: dots are `bar/6` in radius
/// (rings `bar/5`), inset half the leftover bar height from the edge, spaced
/// one diameter plus `bar/6` apart. Ring bars stroke by drawing the outer
/// disc in the ring color and re-filling the inner disc (radius
/// `4*outer/5`) with the bar background. The bar's top corners follow the
/// window's border radius.
pub fn draw_window_bar(
    canvas: &mut Canvas,
    rect: (i32, i32, i32, i32),
    style: BarStyle,
    bar_size: usize,
    bg: Rgb,
    border_radius: f32,
) {
    let (x0, y0, x1, y1) = rect;
    let bar = bar_size as i32;
    let radius = border_radius.round() as i32;
    let width = x1 - x0;

    // Bar background strip, clipped by the window's rounded top corners.
    let strip_y1 = (y0 + bar).min(y1);
    for y in y0.max(0)..strip_y1.min(canvas.h as i32) {
        for x in x0.max(0)..x1.min(canvas.w as i32) {
            let cov = rounded_rect_coverage(x, y, rect, radius);
            if cov > 0.0 {
                canvas.blend_px(x, y, bg, cov);
            }
        }
    }

    let is_right = matches!(style, BarStyle::ColorfulRight | BarStyle::RingsRight);
    match style {
        BarStyle::Colorful | BarStyle::ColorfulRight => {
            let dot_rad = bar / BAR_TO_DOT_RATIO;
            let dot_dia = 2 * dot_rad;
            let dot_gap = (bar - dot_dia) / 2;
            let dot_space = dot_dia + bar / BAR_TO_DOT_RATIO;
            let cy = y0 + dot_rad + dot_gap;
            for (i, color) in [DOT_RED, DOT_YELLOW, DOT_GREEN].into_iter().enumerate() {
                let offset = dot_gap + dot_rad + i as i32 * dot_space;
                let cx = if is_right {
                    x0 + width - offset
                } else {
                    x0 + offset
                };
                fill_circle(canvas, cx, cy, dot_rad, color);
            }
        }
        BarStyle::Rings | BarStyle::RingsRight => {
            let outer_rad = bar / BAR_TO_DOT_BORDER_RATIO;
            let outer_dia = 2 * outer_rad;
            let inner_rad = 2 * outer_dia / BAR_TO_DOT_BORDER_RATIO;
            let ring_gap = (bar - outer_dia) / 2;
            let ring_space = outer_dia + bar / BAR_TO_DOT_RATIO;
            let cy = y0 + outer_rad + ring_gap;
            for i in 0..3 {
                let offset = ring_gap + outer_rad + i * ring_space;
                let cx = if is_right {
                    x0 + width - offset
                } else {
                    x0 + offset
                };
                fill_circle(canvas, cx, cy, outer_rad, RING);
                fill_circle(canvas, cx, cy, inner_rad, bg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circle_coverage_profile() {
        // Center is fully covered, far outside is zero.
        assert_eq!(circle_coverage(10, 10, 10, 10, 5), 1.0);
        assert_eq!(circle_coverage(30, 10, 10, 10, 5), 0.0);
        // The rim is partially covered (antialiased).
        let rim = circle_coverage(14, 10, 10, 10, 5);
        assert!(rim > 0.0 && rim < 1.0, "rim coverage = {rim}");
    }

    #[test]
    fn rounded_rect_corners_and_body() {
        let rect = (0, 0, 100, 60);
        // Sharp corner pixel is outside the rounding.
        assert_eq!(rounded_rect_coverage(0, 0, rect, 10), 0.0);
        assert_eq!(rounded_rect_coverage(99, 59, rect, 10), 0.0);
        // Straight edges and body are fully covered.
        assert_eq!(rounded_rect_coverage(50, 0, rect, 10), 1.0);
        assert_eq!(rounded_rect_coverage(0, 30, rect, 10), 1.0);
        assert_eq!(rounded_rect_coverage(50, 30, rect, 10), 1.0);
        // Outside the rect entirely.
        assert_eq!(rounded_rect_coverage(100, 30, rect, 10), 0.0);
        // Radius 0 degenerates to a plain rect.
        assert_eq!(rounded_rect_coverage(0, 0, rect, 0), 1.0);
    }

    #[test]
    fn ring_bar_has_hollow_center() {
        let mut canvas = Canvas::new(120, 40);
        fill_margin(&mut canvas, Rgb(9, 9, 9));
        draw_window_bar(
            &mut canvas,
            (0, 0, 120, 40),
            BarStyle::Rings,
            30,
            Rgb(9, 9, 9),
            0.0,
        );
        // bar 30: outer_rad = 6, ring_gap = (30 - 12) / 2 = 9, center = (15, 15).
        // Center re-filled with bg.
        assert_eq!(canvas.px(15, 15), [9, 9, 9, 0xff]);
        // A point on the stroke (center +/- ~5px horizontally) is ring-colored.
        assert_eq!(canvas.px(10, 15), [0x33, 0x33, 0x33, 0xff]);
    }

    #[test]
    fn right_aligned_dots_mirror() {
        let mut canvas = Canvas::new(200, 30);
        fill_margin(&mut canvas, Rgb(0, 0, 0));
        draw_window_bar(
            &mut canvas,
            (0, 0, 200, 30),
            BarStyle::ColorfulRight,
            30,
            Rgb(0, 0, 0),
            0.0,
        );
        // Red dot is the rightmost: center x = 200 - (gap + rad) = 200 - 15.
        assert_eq!(canvas.px(185, 15), [0xff, 0x4f, 0x4d, 0xff]);
    }
}
