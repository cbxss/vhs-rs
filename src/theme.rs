//! Terminal color themes: VHS/xterm.js-compatible theme parsing, the builtin
//! theme catalog (vendored VHS themes.json), and indexed-color resolution.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;

use crate::snapshot::Color;

/// A 24-bit sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parses `#rrggbb`, `rrggbb`, `#rgb`, or `rgb` (VHS's parseHexColor set).
    pub fn from_hex(s: &str) -> Option<Self> {
        let hex = s.strip_prefix('#').unwrap_or(s);
        match hex.len() {
            6 => {
                let v = u32::from_str_radix(hex, 16).ok()?;
                Some(Self((v >> 16) as u8, (v >> 8) as u8, v as u8))
            }
            3 => {
                let v = u32::from_str_radix(hex, 16).ok()?;
                // Double each hex digit: 0xf -> 0xff.
                let d = |n: u32| ((n & 0xf) * 17) as u8;
                Some(Self(d(v >> 8), d(v >> 4), d(v)))
            }
            _ => None,
        }
    }

    /// Linear per-channel blend from `self` toward `other` by `t` in [0, 1].
    pub fn lerp(self, other: Self, t: f32) -> Self {
        let ch = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Self(
            ch(self.0, other.0),
            ch(self.1, other.1),
            ch(self.2, other.2),
        )
    }
}

/// Errors produced while parsing themes.
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("invalid theme JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid color value {0:?}")]
    Color(String),
}

/// A terminal theme: the 16 ANSI colors plus screen colors.
///
/// Field names follow xterm.js's ITheme (which VHS themes.json uses); serde
/// aliases accept the Windows Terminal spellings (`purple`, `cursorColor`,
/// `selectionBackground`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub name: String,
    pub black: Rgb,
    pub red: Rgb,
    pub green: Rgb,
    pub yellow: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
    pub cyan: Rgb,
    pub white: Rgb,
    pub bright_black: Rgb,
    pub bright_red: Rgb,
    pub bright_green: Rgb,
    pub bright_yellow: Rgb,
    pub bright_blue: Rgb,
    pub bright_magenta: Rgb,
    pub bright_cyan: Rgb,
    pub bright_white: Rgb,
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
}

/// Raw JSON form of a theme: every field optional so partial inline themes
/// (`Set Theme {"background": "#171717"}`) overlay the default theme, exactly
/// like VHS unmarshalling into its current Theme struct.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTheme {
    name: Option<String>,
    black: Option<String>,
    red: Option<String>,
    green: Option<String>,
    yellow: Option<String>,
    blue: Option<String>,
    #[serde(alias = "purple")]
    magenta: Option<String>,
    cyan: Option<String>,
    white: Option<String>,
    bright_black: Option<String>,
    bright_red: Option<String>,
    bright_green: Option<String>,
    bright_yellow: Option<String>,
    bright_blue: Option<String>,
    #[serde(alias = "brightPurple")]
    bright_magenta: Option<String>,
    bright_cyan: Option<String>,
    bright_white: Option<String>,
    background: Option<String>,
    foreground: Option<String>,
    #[serde(alias = "cursorColor")]
    cursor: Option<String>,
    #[serde(alias = "selectionBackground")]
    selection: Option<String>,
}

fn parse_color(s: &str) -> Result<Rgb, ThemeError> {
    Rgb::from_hex(s).ok_or_else(|| ThemeError::Color(s.to_string()))
}

impl RawTheme {
    /// Overlays this raw theme onto `base`; missing cursor/selection derive
    /// from the (post-overlay) foreground/background.
    fn apply(self, mut base: Theme) -> Result<Theme, ThemeError> {
        macro_rules! set {
            ($($field:ident),+ $(,)?) => {
                $(if let Some(s) = &self.$field {
                    base.$field = parse_color(s)?;
                })+
            };
        }
        if let Some(name) = self.name {
            base.name = name;
        }
        set!(
            black,
            red,
            green,
            yellow,
            blue,
            magenta,
            cyan,
            white,
            bright_black,
            bright_red,
            bright_green,
            bright_yellow,
            bright_blue,
            bright_magenta,
            bright_cyan,
            bright_white,
            background,
            foreground,
        );
        base.cursor = match &self.cursor {
            Some(s) => parse_color(s)?,
            None => base.foreground,
        };
        base.selection = match &self.selection {
            Some(s) => parse_color(s)?,
            None => base.background,
        };
        Ok(base)
    }
}

/// VHS's DefaultTheme (vhs/style.go + themes.go), ported exactly.
pub fn default_theme() -> Theme {
    Theme {
        name: "default".to_string(),
        background: Rgb(0x17, 0x17, 0x17),
        foreground: Rgb(0xdd, 0xdd, 0xdd),
        cursor: Rgb(0xdd, 0xdd, 0xdd),       // VHS: Cursor = Foreground
        selection: Rgb(0x17, 0x17, 0x17),    // derived from background
        black: Rgb(0x28, 0x2a, 0x2e),        // ansi 0
        bright_black: Rgb(0x4d, 0x4d, 0x4d), // ansi 8
        red: Rgb(0xd7, 0x4e, 0x6f),          // ansi 1
        bright_red: Rgb(0xfe, 0x5f, 0x86),   // ansi 9
        green: Rgb(0x31, 0xbb, 0x71),        // ansi 2
        bright_green: Rgb(0x00, 0xd7, 0x87), // ansi 10
        yellow: Rgb(0xd3, 0xe5, 0x61),       // ansi 3
        bright_yellow: Rgb(0xeb, 0xff, 0x71), // ansi 11
        blue: Rgb(0x80, 0x56, 0xff),         // ansi 4
        bright_blue: Rgb(0x9b, 0x79, 0xff),  // ansi 12
        magenta: Rgb(0xed, 0x61, 0xd7),      // ansi 5
        bright_magenta: Rgb(0xff, 0x7a, 0xea), // ansi 13
        cyan: Rgb(0x04, 0xd7, 0xd7),         // ansi 6
        bright_cyan: Rgb(0x00, 0xfe, 0xfe),  // ansi 14
        white: Rgb(0xbf, 0xbf, 0xbf),        // ansi 7
        bright_white: Rgb(0xe6, 0xe6, 0xe6), // ansi 15
    }
}

/// The vendored VHS theme catalog.
const THEMES_JSON: &str = include_str!("../assets/themes.json");

/// The builtin catalog keyed by lowercase name, parsed once on first use
/// (348 themes; runs that never `Set Theme` pay nothing). The catalog has
/// duplicate names differing only in case; the first entry wins, matching
/// the previous linear-scan lookup.
static BUILTIN_THEMES: LazyLock<HashMap<String, Theme>> = LazyLock::new(|| {
    let raws: Vec<RawTheme> =
        serde_json::from_str(THEMES_JSON).expect("vendored themes.json must parse");
    let mut themes = HashMap::with_capacity(raws.len());
    for raw in raws {
        let Some(name) = raw.name.clone() else {
            continue;
        };
        if let Ok(theme) = raw.apply(default_theme()) {
            themes.entry(name.to_ascii_lowercase()).or_insert(theme);
        }
    }
    themes
});

/// Looks up a builtin theme by name, case-insensitively.
pub fn load_builtin(name: &str) -> Option<Theme> {
    BUILTIN_THEMES.get(&name.to_ascii_lowercase()).cloned()
}

/// Parses an inline theme (`Set Theme {json}`); unspecified fields keep the
/// default theme's values.
pub fn from_json(s: &str) -> Result<Theme, ThemeError> {
    let raw: RawTheme = serde_json::from_str(s)?;
    raw.apply(default_theme())
}

impl Theme {
    /// One of the 16 ANSI palette entries.
    pub fn ansi(&self, i: u8) -> Rgb {
        match i {
            0 => self.black,
            1 => self.red,
            2 => self.green,
            3 => self.yellow,
            4 => self.blue,
            5 => self.magenta,
            6 => self.cyan,
            7 => self.white,
            8 => self.bright_black,
            9 => self.bright_red,
            10 => self.bright_green,
            11 => self.bright_yellow,
            12 => self.bright_blue,
            13 => self.bright_magenta,
            14 => self.bright_cyan,
            _ => self.bright_white,
        }
    }

    /// Resolves a snapshot color to RGB: 0-15 themed, 16-231 the 6x6x6 cube,
    /// 232-255 the grayscale ramp, RGB passthrough.
    pub fn resolve(&self, c: Color) -> Rgb {
        match c {
            Color::Rgb(r, g, b) => Rgb(r, g, b),
            Color::Indexed(i @ 0..=15) => self.ansi(i),
            Color::Indexed(i @ 16..=231) => {
                let i = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
                Rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
            }
            Color::Indexed(i) => {
                let g = 8 + 10 * (i - 232);
                Rgb(g, g, g)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parsing() {
        assert_eq!(Rgb::from_hex("#29283b"), Some(Rgb(0x29, 0x28, 0x3b)));
        assert_eq!(Rgb::from_hex("ffffff"), Some(Rgb(255, 255, 255)));
        assert_eq!(Rgb::from_hex("#f0c"), Some(Rgb(0xff, 0x00, 0xcc)));
        assert_eq!(Rgb::from_hex("nope"), None);
        assert_eq!(Rgb::from_hex(""), None);
    }

    #[test]
    fn all_builtin_themes_deserialize() {
        let raws: Vec<RawTheme> = serde_json::from_str(THEMES_JSON).unwrap();
        assert!(raws.len() > 300, "expected the full VHS catalog");
        for raw in raws {
            let name = raw.name.clone().unwrap_or_default();
            assert!(!name.is_empty(), "every theme must be named");
            raw.apply(default_theme())
                .unwrap_or_else(|e| panic!("theme {name:?} failed: {e}"));
        }
    }

    #[test]
    fn dracula_lookup_case_insensitive() {
        let t = load_builtin("dracula").expect("Dracula exists");
        assert_eq!(t.name, "Dracula");
        assert_eq!(t, load_builtin("DRACULA").unwrap());
        assert!(load_builtin("no-such-theme").is_none());
    }

    #[test]
    fn missing_cursor_selection_derive_from_fg_bg() {
        let t = from_json(r##"{"foreground": "#123456", "background": "#654321"}"##).unwrap();
        assert_eq!(t.cursor, Rgb(0x12, 0x34, 0x56));
        assert_eq!(t.selection, Rgb(0x65, 0x43, 0x21));
        // Aliases from the Windows Terminal schema.
        let t = from_json(
            r##"{"purple": "#010203", "cursorColor": "#040506", "selectionBackground": "#070809"}"##,
        )
        .unwrap();
        assert_eq!(t.magenta, Rgb(1, 2, 3));
        assert_eq!(t.cursor, Rgb(4, 5, 6));
        assert_eq!(t.selection, Rgb(7, 8, 9));
    }

    #[test]
    fn default_theme_matches_vhs() {
        let t = default_theme();
        assert_eq!(t.background, Rgb(0x17, 0x17, 0x17));
        assert_eq!(t.foreground, Rgb(0xdd, 0xdd, 0xdd));
        assert_eq!(t.cursor, t.foreground);
        assert_eq!(t.red, Rgb(0xd7, 0x4e, 0x6f));
        assert_eq!(t.bright_white, Rgb(0xe6, 0xe6, 0xe6));
    }

    #[test]
    fn indexed_resolution() {
        let t = default_theme();
        assert_eq!(t.resolve(Color::Indexed(1)), t.red);
        assert_eq!(t.resolve(Color::Indexed(9)), t.bright_red);
        // Cube corners and a spot check: 196 = 16 + 36*5 = pure red.
        assert_eq!(t.resolve(Color::Indexed(16)), Rgb(0, 0, 0));
        assert_eq!(t.resolve(Color::Indexed(231)), Rgb(255, 255, 255));
        assert_eq!(t.resolve(Color::Indexed(196)), Rgb(255, 0, 0));
        assert_eq!(t.resolve(Color::Indexed(46)), Rgb(0, 255, 0));
        assert_eq!(t.resolve(Color::Indexed(21)), Rgb(0, 0, 255));
        // 60 = 16 + 36*1 + 6*1 + 2 -> (95, 95, 135)
        assert_eq!(t.resolve(Color::Indexed(60)), Rgb(95, 95, 135));
        // Grayscale ramp: 8 + 10*i.
        assert_eq!(t.resolve(Color::Indexed(232)), Rgb(8, 8, 8));
        assert_eq!(t.resolve(Color::Indexed(255)), Rgb(238, 238, 238));
        // RGB passthrough.
        assert_eq!(t.resolve(Color::Rgb(1, 2, 3)), Rgb(1, 2, 3));
    }
}
