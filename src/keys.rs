//! Key and chord encoding: tape key commands → raw PTY byte sequences.
//!
//! Replaces VHS's browser key events: instead of synthesizing DOM keydowns
//! into xterm.js, vhs_rs writes the escape sequences a terminal would send
//! directly to the child's PTY.
//!
//! Arrow/Home/End encoding depends on DECCKM (application cursor keys mode,
//! `CSI ? 1 h/l`): normal mode sends CSI (`\x1b[A`), application mode sends
//! SS3 (`\x1bOA`). The session tracks the mode so vim/fzf/etc. receive what
//! they expect.

use crate::token::TokenType;

/// Bytes for a standalone keypress token (Enter, Tab, arrows, ...).
/// Returns `None` for tokens that are not standalone keys.
pub fn keypress_bytes(t: TokenType, application_cursor: bool) -> Option<&'static [u8]> {
    use TokenType::*;

    let bytes: &'static [u8] = match t {
        Enter => b"\r",
        Space => b" ",
        Tab => b"\t",
        Backspace => &[0x7f],
        Delete => b"\x1b[3~",
        Insert => b"\x1b[2~",
        Escape => &[0x1b],
        Up if application_cursor => b"\x1bOA",
        Up => b"\x1b[A",
        Down if application_cursor => b"\x1bOB",
        Down => b"\x1b[B",
        Right if application_cursor => b"\x1bOC",
        Right => b"\x1b[C",
        Left if application_cursor => b"\x1bOD",
        Left => b"\x1b[D",
        Home if application_cursor => b"\x1bOH",
        Home => b"\x1b[H",
        End if application_cursor => b"\x1bOF",
        End => b"\x1b[F",
        PageUp => b"\x1b[5~",
        PageDown => b"\x1b[6~",
        _ => return None,
    };

    Some(bytes)
}

/// xterm modifier parameter: 1 + shift·1 + alt·2 + ctrl·4.
fn xterm_modifier(shift: bool, alt: bool, ctrl: bool) -> u8 {
    1 + (shift as u8) + (alt as u8) * 2 + (ctrl as u8) * 4
}

/// Bytes for a `Ctrl+...` chord, given VHS's Ctrl args format: modifiers
/// then key, space-separated ("C", "Shift C", "Shift Alt C", "Right", ...).
/// Ctrl itself is implicit. Unknown keys yield an empty vector.
pub fn ctrl_bytes(args: &str) -> Vec<u8> {
    let words: Vec<&str> = args.split_whitespace().collect();

    let Some((&key, modifiers)) = words.split_last() else {
        return Vec::new();
    };

    let mut shift = false;
    let mut alt = false;

    for m in modifiers {
        match *m {
            "Shift" => shift = true,
            "Alt" => alt = true,
            "Ctrl" => {} // implicit
            _ => return Vec::new(),
        }
    }

    let modifier = xterm_modifier(shift, alt, true);

    // Arrows and Home/End: CSI 1;<mod> <final>.
    let csi_final = match key {
        "Up" => Some('A'),
        "Down" => Some('B'),
        "Right" => Some('C'),
        "Left" => Some('D'),
        "Home" => Some('H'),
        "End" => Some('F'),
        _ => None,
    };

    if let Some(f) = csi_final {
        return format!("\x1b[1;{modifier}{f}").into_bytes();
    }

    // Editing/navigation keys: CSI <n>;<mod> ~.
    let tilde = match key {
        "Insert" => Some(2),
        "Delete" => Some(3),
        "PageUp" => Some(5),
        "PageDown" => Some(6),
        _ => None,
    };

    if let Some(n) = tilde {
        return format!("\x1b[{n};{modifier}~").into_bytes();
    }

    // Control characters.
    let ctrl_char: Option<u8> = match key {
        "Space" | "@" => Some(0x00),
        "[" => Some(0x1b),
        "\\" => Some(0x1c),
        "]" => Some(0x1d),
        "^" => Some(0x1e),
        "-" | "_" => Some(0x1f),
        "?" => Some(0x7f),
        _ => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => {
                    Some((c.to_ascii_uppercase() as u8) & 0x1f)
                }
                _ => None,
            }
        }
    };

    match ctrl_char {
        // Alt is an ESC prefix on control characters.
        Some(b) if alt => vec![0x1b, b],
        Some(b) => vec![b],
        None => Vec::new(),
    }
}

/// Bytes for an `Alt+...` chord: ESC prefix + the key.
/// `Alt+Enter` → `\x1b\r`, `Alt+Tab` → `\x1b\t`, otherwise ESC + the char(s).
pub fn alt_bytes(ch: &str) -> Vec<u8> {
    let mut bytes = vec![0x1b];

    match ch {
        "Enter" => bytes.push(b'\r'),
        "Tab" => bytes.push(b'\t'),
        _ => bytes.extend_from_slice(ch.as_bytes()),
    }

    bytes
}

/// Bytes for a `Shift+...` chord: `Shift+Tab` → back-tab (`\x1b[Z`),
/// `Shift+Enter` → `\r`, otherwise the char as-is (the tape already carries
/// the uppercase form).
pub fn shift_bytes(ch: &str) -> Vec<u8> {
    match ch {
        "Tab" => b"\x1b[Z".to_vec(),
        "Enter" => b"\r".to_vec(),
        _ => ch.as_bytes().to_vec(),
    }
}

/// SGR-encoded mouse wheel event (`CSI < 64/65 ; col ; row M`) for
/// `ScrollUp`/`ScrollDown`. `col`/`row` are 0-based cell coordinates; the
/// wire format is 1-based. Button 64 = wheel up, 65 = wheel down.
pub fn wheel_bytes(up: bool, col: usize, row: usize) -> Vec<u8> {
    let button = if up { 64 } else { 65 };
    format!("\x1b[<{button};{};{}M", col + 1, row + 1).into_bytes()
}

/// Scans a raw output chunk for DECCKM set/reset (`\x1b[?1h` / `\x1b[?1l`)
/// and returns the updated application-cursor-keys mode. The last occurrence
/// wins; `current` is returned unchanged if neither appears.
///
/// Note: [`crate::term::Term::application_cursor`] (backed by avt's parser)
/// is boundary-safe and preferred when a `Term` has seen the bytes; this scan
/// is a raw-bytes fallback.
pub fn decckm_scan(bytes: &[u8], current: bool) -> bool {
    const SET: &[u8] = b"\x1b[?1h";
    const RESET: &[u8] = b"\x1b[?1l";

    let mut mode = current;
    let mut i = 0;

    while i + SET.len() <= bytes.len() {
        let window = &bytes[i..i + SET.len()];

        if window == SET {
            mode = true;
            i += SET.len();
        } else if window == RESET {
            mode = false;
            i += RESET.len();
        } else {
            i += 1;
        }
    }

    mode
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::TokenType::*;

    #[test]
    fn keypress_normal_mode() {
        let cases: &[(crate::token::TokenType, &[u8])] = &[
            (Enter, b"\r"),
            (Space, b" "),
            (Tab, b"\t"),
            (Backspace, &[0x7f]),
            (Delete, b"\x1b[3~"),
            (Insert, b"\x1b[2~"),
            (Escape, &[0x1b]),
            (Up, b"\x1b[A"),
            (Down, b"\x1b[B"),
            (Right, b"\x1b[C"),
            (Left, b"\x1b[D"),
            (Home, b"\x1b[H"),
            (End, b"\x1b[F"),
            (PageUp, b"\x1b[5~"),
            (PageDown, b"\x1b[6~"),
        ];

        for &(token, expected) in cases {
            assert_eq!(keypress_bytes(token, false), Some(expected), "{token:?}");
        }
    }

    #[test]
    fn keypress_application_cursor_mode() {
        let cases: &[(crate::token::TokenType, &[u8])] = &[
            (Up, b"\x1bOA"),
            (Down, b"\x1bOB"),
            (Right, b"\x1bOC"),
            (Left, b"\x1bOD"),
            (Home, b"\x1bOH"),
            (End, b"\x1bOF"),
            // Not affected by DECCKM:
            (Enter, b"\r"),
            (PageUp, b"\x1b[5~"),
            (Delete, b"\x1b[3~"),
        ];

        for &(token, expected) in cases {
            assert_eq!(keypress_bytes(token, true), Some(expected), "{token:?}");
        }
    }

    #[test]
    fn keypress_non_keys() {
        for token in [Type, Sleep, Output, Set, ScrollUp, ScrollDown] {
            assert_eq!(keypress_bytes(token, false), None, "{token:?}");
        }
    }

    #[test]
    fn ctrl_letters() {
        let cases: &[(&str, &[u8])] = &[
            ("C", &[0x03]),
            ("c", &[0x03]),
            ("A", &[0x01]),
            ("Z", &[0x1a]),
            ("D", &[0x04]),
            ("L", &[0x0c]),
        ];

        for &(args, expected) in cases {
            assert_eq!(ctrl_bytes(args), expected, "Ctrl+{args}");
        }
    }

    #[test]
    fn ctrl_special_chars() {
        let cases: &[(&str, &[u8])] = &[
            ("Space", &[0x00]),
            ("@", &[0x00]),
            ("[", &[0x1b]),
            ("\\", &[0x1c]),
            ("]", &[0x1d]),
            ("^", &[0x1e]),
            ("-", &[0x1f]),
            ("_", &[0x1f]),
            ("?", &[0x7f]),
        ];

        for &(args, expected) in cases {
            assert_eq!(ctrl_bytes(args), expected, "Ctrl+{args:?}");
        }
    }

    #[test]
    fn ctrl_navigation_with_modifiers() {
        let cases: &[(&str, &[u8])] = &[
            ("Right", b"\x1b[1;5C"),
            ("Left", b"\x1b[1;5D"),
            ("Up", b"\x1b[1;5A"),
            ("Down", b"\x1b[1;5B"),
            ("Home", b"\x1b[1;5H"),
            ("End", b"\x1b[1;5F"),
            ("Shift Right", b"\x1b[1;6C"),
            ("Alt Right", b"\x1b[1;7C"),
            ("Shift Alt Right", b"\x1b[1;8C"),
            ("Delete", b"\x1b[3;5~"),
            ("Insert", b"\x1b[2;5~"),
            ("PageUp", b"\x1b[5;5~"),
            ("PageDown", b"\x1b[6;5~"),
            ("Shift PageUp", b"\x1b[5;6~"),
        ];

        for &(args, expected) in cases {
            assert_eq!(ctrl_bytes(args), expected, "Ctrl+{args}");
        }
    }

    #[test]
    fn ctrl_modified_letters_and_unknowns() {
        // Shift on a letter doesn't change the control byte.
        assert_eq!(ctrl_bytes("Shift C"), vec![0x03]);
        // Alt prefixes ESC.
        assert_eq!(ctrl_bytes("Alt C"), vec![0x1b, 0x03]);
        assert_eq!(ctrl_bytes("Shift Alt C"), vec![0x1b, 0x03]);
        // Redundant explicit Ctrl modifier is tolerated.
        assert_eq!(ctrl_bytes("Ctrl C"), vec![0x03]);
        // Unknown keys/modifiers produce nothing.
        assert_eq!(ctrl_bytes(""), Vec::<u8>::new());
        assert_eq!(ctrl_bytes("Foo C"), Vec::<u8>::new());
        assert_eq!(ctrl_bytes("Widget"), Vec::<u8>::new());
    }

    #[test]
    fn alt_chords() {
        let cases: &[(&str, &[u8])] = &[
            ("Enter", b"\x1b\r"),
            ("Tab", b"\x1b\t"),
            ("f", b"\x1bf"),
            ("B", b"\x1bB"),
            (".", b"\x1b."),
        ];

        for &(ch, expected) in cases {
            assert_eq!(alt_bytes(ch), expected, "Alt+{ch}");
        }
    }

    #[test]
    fn shift_chords() {
        let cases: &[(&str, &[u8])] = &[
            ("Tab", b"\x1b[Z"),
            ("Enter", b"\r"),
            ("A", b"A"),
            ("g", b"g"),
        ];

        for &(ch, expected) in cases {
            assert_eq!(shift_bytes(ch), expected, "Shift+{ch}");
        }
    }

    #[test]
    fn wheel_events() {
        // (up, col, row, expected) — coordinates are 0-based in, 1-based out.
        let cases: &[(bool, usize, usize, &[u8])] = &[
            (true, 0, 0, b"\x1b[<64;1;1M"),
            (false, 0, 0, b"\x1b[<65;1;1M"),
            (true, 9, 4, b"\x1b[<64;10;5M"),
            (false, 79, 23, b"\x1b[<65;80;24M"),
        ];

        for &(up, col, row, expected) in cases {
            assert_eq!(
                wheel_bytes(up, col, row),
                expected,
                "up={up} col={col} row={row}"
            );
        }
    }

    #[test]
    fn decckm_scanning() {
        // (chunk, initial mode, expected mode)
        let cases: &[(&[u8], bool, bool)] = &[
            (b"", false, false),
            (b"", true, true),
            (b"plain output", false, false),
            (b"plain output", true, true),
            (b"\x1b[?1h", false, true),
            (b"\x1b[?1l", true, false),
            (b"before\x1b[?1hafter", false, true),
            (b"\x1b[?1h...\x1b[?1l", true, false),
            (b"\x1b[?1l...\x1b[?1h", false, true),
            // Similar-but-different sequences must not trigger.
            (b"\x1b[?1049h", false, false), // alt screen
            (b"\x1b[?12h", false, false),   // cursor blink
            (b"\x1b[?25h", true, true),     // show cursor
            (b"\x1b[1h", false, false),     // non-private mode
        ];

        for &(bytes, current, expected) in cases {
            assert_eq!(
                decckm_scan(bytes, current),
                expected,
                "bytes {} current {current}",
                bytes.escape_ascii()
            );
        }
    }
}
