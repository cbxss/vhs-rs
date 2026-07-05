//! Embedded JetBrains Mono fonts, cell metrics, and a rasterized glyph cache.
//!
//! Characters the main font lacks fall back to a single symbols-only Nerd
//! Font (powerline/devicons/codicons/etc., regular weight — icons have no
//! bold/italic), which keeps the embedded payload small compared to shipping
//! four fully patched Nerd Font variants.

use std::collections::HashMap;

use fontdue::{Font, FontSettings};

const REGULAR: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Bold.ttf");
const ITALIC: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Italic.ttf");
const BOLD_ITALIC: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-BoldItalic.ttf");
/// Symbols Nerd Font Mono: single-cell icon glyphs, one weight for all four
/// styles (see assets/fonts/SYMBOLS-LICENSE.txt).
const SYMBOLS: &[u8] = include_bytes!("../../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

/// Glyph used when the font has no coverage for a character.
const REPLACEMENT: char = '\u{25A1}'; // WHITE SQUARE

/// A rasterized glyph: fontdue placement metrics plus an 8-bit coverage map.
pub struct CachedGlyph {
    pub metrics: fontdue::Metrics,
    pub bitmap: Vec<u8>,
}

/// Grid layout metrics for a font size + line height + letter spacing.
///
/// All vertical positions are relative to the top of a cell; `cell_w`/`cell_h`
/// are rounded to whole pixels so cells land on device-pixel boundaries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub cell_w: f32,
    pub cell_h: f32,
    /// Baseline y-offset from the top of the cell.
    pub baseline: f32,
    pub underline_y: f32,
    pub strikeout_y: f32,
    /// The pixel size the fonts are rasterized at.
    pub px: f32,
}

/// The four embedded JetBrains Mono variants, the symbols fallback font, and
/// a glyph cache, all at one fixed pixel size.
pub struct FontSet {
    fonts: [Font; 4],
    symbols: Font,
    px: f32,
    cache: HashMap<(char, bool, bool), CachedGlyph>,
}

fn variant_index(bold: bool, italic: bool) -> usize {
    (bold as usize) | ((italic as usize) << 1)
}

impl FontSet {
    /// Loads the embedded fonts for rasterization at `px` pixels.
    pub fn new(px: f32) -> FontSet {
        let load = |bytes: &[u8]| {
            Font::from_bytes(bytes, FontSettings::default()).expect("embedded font must parse")
        };
        FontSet {
            fonts: [load(REGULAR), load(BOLD), load(ITALIC), load(BOLD_ITALIC)],
            symbols: load(SYMBOLS),
            px,
            cache: HashMap::new(),
        }
    }

    pub fn px(&self) -> f32 {
        self.px
    }

    /// The font for an attribute combination.
    pub fn pick(&self, bold: bool, italic: bool) -> &Font {
        &self.fonts[variant_index(bold, italic)]
    }

    /// Computes grid metrics from the regular variant.
    pub fn metrics(&self, line_height: f32, letter_spacing: f32) -> Metrics {
        let font = self.pick(false, false);
        let lm = font
            .horizontal_line_metrics(self.px)
            .expect("horizontal font must have line metrics");
        let cell_w = (font.metrics('0', self.px).advance_width + letter_spacing).round();
        let cell_h = (self.px * line_height).round();
        // Center the font's ascent..descent span within the cell (descent is
        // negative in fontdue).
        let glyph_span = lm.ascent - lm.descent;
        let baseline = ((cell_h - glyph_span) / 2.0 + lm.ascent).round();
        // Keep the underline inside the cell even at tight line heights.
        let line_thickness = (self.px / 14.0).round().max(1.0);
        let underline_y = (baseline + (self.px / 10.0).max(1.0))
            .round()
            .min(cell_h - line_thickness);
        let strikeout_y = (baseline - self.px * 0.3).round();
        Metrics {
            cell_w,
            cell_h,
            baseline,
            underline_y,
            strikeout_y,
            px: self.px,
        }
    }

    /// Rasterizes (or returns the cached) glyph for a character + attributes.
    ///
    /// Resolution order: the style's JetBrains Mono variant, then the symbols
    /// fallback font (regular weight regardless of style — icons have no
    /// bold/italic), then U+25A1 WHITE SQUARE from the main font, then a
    /// blank glyph. The cache is keyed by (char, bold, italic); the cached
    /// entry simply holds whatever font rasterized it.
    pub fn glyph(&mut self, ch: char, bold: bool, italic: bool) -> &CachedGlyph {
        let key = (ch, bold, italic);
        if !self.cache.contains_key(&key) {
            let main = &self.fonts[variant_index(bold, italic)];
            let (font, drawn) = if main.lookup_glyph_index(ch) != 0 {
                (main, ch)
            } else if self.symbols.lookup_glyph_index(ch) != 0 {
                (&self.symbols, ch)
            } else if main.lookup_glyph_index(REPLACEMENT) != 0 {
                (main, REPLACEMENT)
            } else {
                (main, ' ')
            };
            let (metrics, bitmap) = font.rasterize(drawn, self.px);
            self.cache.insert(key, CachedGlyph { metrics, bitmap });
        }
        &self.cache[&key]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_are_sane() {
        let fonts = FontSet::new(22.0);
        let m = fonts.metrics(1.0, 1.0);
        assert_eq!(m.cell_h, 22.0);
        assert!(m.cell_w > 8.0 && m.cell_w < 22.0, "cell_w = {}", m.cell_w);
        assert_eq!(m.cell_w.fract(), 0.0, "cell_w must be whole pixels");
        assert!(m.baseline > 0.0 && m.baseline <= m.cell_h + 4.0);
        assert!(m.underline_y > m.strikeout_y);
        // Line height scales the cell.
        let m2 = fonts.metrics(1.5, 1.0);
        assert_eq!(m2.cell_h, 33.0);
    }

    #[test]
    fn glyph_cache_and_variants() {
        let mut fonts = FontSet::new(22.0);
        let g = fonts.glyph('A', false, false);
        assert!(g.metrics.width > 0 && g.metrics.height > 0);
        assert!(g.bitmap.iter().any(|&c| c > 0), "glyph has coverage");
        let regular_w = g.metrics.width;
        // Bold variant rasterizes from a different font.
        let gb = fonts.glyph('A', true, false);
        assert!(gb.bitmap.iter().any(|&c| c > 0));
        assert!(gb.metrics.width >= regular_w);
        // All four variants resolve to distinct fonts.
        let names: Vec<*const Font> = [(false, false), (true, false), (false, true), (true, true)]
            .iter()
            .map(|&(b, i)| fonts.pick(b, i) as *const Font)
            .collect();
        for i in 0..4 {
            for j in (i + 1)..4 {
                assert_ne!(names[i], names[j]);
            }
        }
    }

    #[test]
    fn nerd_glyph_coverage_via_symbols_fallback() {
        let mut fonts = FontSet::new(22.0);
        // The devicon U+E718 is absent from plain JetBrains Mono in every
        // variant, so rendering it must go through the symbols fallback.
        for &(b, i) in &[(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(
                fonts.pick(b, i).lookup_glyph_index('\u{E718}'),
                0,
                "U+E718 unexpectedly in the main font (bold={b} italic={i})"
            );
        }
        assert_ne!(
            fonts.symbols.lookup_glyph_index('\u{E718}'),
            0,
            "U+E718 missing from the symbols font"
        );

        // Both the powerline triangle and the devicon rasterize with ink in
        // all four styles (icons resolve through the single symbols weight).
        for ch in ['\u{E0B0}', '\u{E718}'] {
            for &(b, i) in &[(false, false), (true, false), (false, true), (true, true)] {
                let g = fonts.glyph(ch, b, i);
                assert!(
                    g.bitmap.iter().any(|&c| c > 0),
                    "U+{:04X} has no ink (bold={b} italic={i})",
                    ch as u32
                );
                assert!(g.metrics.width > 0 && g.metrics.height > 0);
            }
        }
    }

    #[test]
    fn cell_metrics_match_plain_jetbrains_mono() {
        // JetBrains Mono's '0' advance is 600/1000 em; the grid derives from
        // the main font only, never the symbols fallback. Guard within 1%.
        let px = 22.0;
        let fonts = FontSet::new(px);
        let advance = fonts.pick(false, false).metrics('0', px).advance_width;
        let expected = 0.6 * px;
        assert!(
            (advance - expected).abs() <= expected * 0.01,
            "'0' advance {advance} deviates >1% from {expected}"
        );
    }

    #[test]
    fn missing_glyph_falls_back() {
        let mut fonts = FontSet::new(22.0);
        // Neither JetBrains Mono nor the symbols font covers CJK; must not
        // panic and should draw the replacement square (which has ink).
        let g = fonts.glyph('\u{4E2D}', false, false);
        assert!(g.bitmap.iter().any(|&c| c > 0), "replacement glyph has ink");
    }
}
