//! Offscreen terminal emulator wrapper around `avt::Vt`.
//!
//! This is the only module allowed to use avt types. It converts avt state
//! into the crate's shared boundary types (`crate::snapshot`), so the render
//! and encode sides never depend on the emulator crate.
//!
//! avt 0.18 API surface used here:
//! - `Vt::builder().size(cols, rows).scrollback_limit(0).build()`
//! - `Vt::feed_str(&mut self, &str) -> Changes` (changed-line info, unused)
//! - `Vt::view() -> impl Iterator<Item = &Line>` (visible rows, top to bottom)
//! - `Line::cells() -> &[Cell]`, `Line::text() -> String` (skips wide tails)
//! - `Cell::char()`, `Cell::width()` (1 single / 2 wide head / 0 wide tail),
//!   `Cell::pen() -> &Pen`
//! - `Pen::{foreground, background, is_bold, is_faint, is_italic,
//!   is_underline, is_strikethrough, is_blink, is_inverse}`
//! - `avt::Color::{Indexed(u8), RGB(rgb::RGB8)}`
//! - `Vt::cursor() -> Cursor { col, row, visible }`
//! - `Vt::cursor_key_app_mode() -> bool` (DECCKM, parsed by avt itself)
//! - `Vt::resize(cols, rows)`, `Vt::size() -> (cols, rows)`

use crate::snapshot::{Cell, CellAttrs, Color, Cursor, GridSnapshot};
use avt::Vt;

/// The offscreen screen model for one session.
pub struct Term {
    vt: Vt,
}

impl Term {
    /// Creates a `cols × rows` terminal with no scrollback (vhs_rs only ever
    /// inspects the visible screen; dropping scrollback keeps memory flat).
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            vt: Vt::builder().size(cols, rows).scrollback_limit(0).build(),
        }
    }

    /// Feeds decoded (boundary-safe UTF-8) child output into the emulator.
    pub fn feed(&mut self, s: &str) {
        self.vt.feed_str(s);
    }

    /// Current size as `(cols, rows)`.
    pub fn size(&self) -> (usize, usize) {
        self.vt.size()
    }

    /// Resizes the screen, reflowing content per avt's rules.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.vt.resize(cols, rows);
    }

    /// Whether the application has enabled application cursor keys (DECCKM,
    /// `CSI ? 1 h/l`). avt parses the mode itself, so this is boundary-safe
    /// even when escape sequences split across reads.
    pub fn application_cursor(&self) -> bool {
        self.vt.cursor_key_app_mode()
    }

    /// The cursor position and visibility, without building a grid snapshot.
    pub fn cursor(&self) -> Cursor {
        let cursor = self.vt.cursor();

        Cursor {
            col: cursor.col,
            row: cursor.row,
            visible: cursor.visible,
        }
    }

    /// An immutable snapshot of the visible screen in crate boundary types.
    ///
    /// Wide characters keep avt's occupancy convention: the leading cell has
    /// `width == 2` and the following continuation cell has `width == 0`.
    pub fn snapshot(&self) -> GridSnapshot {
        let mut out = GridSnapshot {
            cols: 0,
            rows: 0,
            cells: Vec::new(),
            cursor: self.cursor(),
        };
        self.snapshot_into(&mut out);
        out
    }

    /// Overwrites `out` with a snapshot of the visible screen, reusing its
    /// cell buffer — no allocation once the buffer has reached the screen
    /// size. Semantics are identical to [`Term::snapshot`].
    pub fn snapshot_into(&self, out: &mut GridSnapshot) {
        let (cols, rows) = self.vt.size();
        out.cols = cols;
        out.rows = rows;
        out.cells.clear();
        out.cells.resize(cols * rows, Cell::default());

        for (row, line) in self.vt.view().take(rows).enumerate() {
            for (col, cell) in line.cells().iter().take(cols).enumerate() {
                out.cells[row * cols + col] = convert_cell(cell);
            }
        }

        out.cursor = self.cursor();
    }

    /// The visible screen as plain text: one line per row, trailing
    /// whitespace trimmed per line, rows joined with `\n`.
    pub fn text(&self) -> String {
        let (_, rows) = self.vt.size();
        let lines: Vec<String> = self
            .vt
            .view()
            .take(rows)
            .map(|line| line.text().trim_end().to_string())
            .collect();

        lines.join("\n")
    }

    /// The line under the cursor, trailing-trimmed (VHS `CurrentLine`
    /// semantics, used by `Wait+Line` / `Assert+Line`).
    pub fn current_line(&self) -> String {
        let row = self.vt.cursor().row;

        self.vt
            .view()
            .nth(row)
            .map(|line| line.text().trim_end().to_string())
            .unwrap_or_default()
    }
}

fn convert_cell(cell: &avt::Cell) -> Cell {
    let pen = cell.pen();

    Cell {
        ch: cell.char(),
        fg: pen.foreground().map(convert_color),
        bg: pen.background().map(convert_color),
        attrs: CellAttrs {
            bold: pen.is_bold(),
            faint: pen.is_faint(),
            italic: pen.is_italic(),
            underline: pen.is_underline(),
            strikethrough: pen.is_strikethrough(),
            inverse: pen.is_inverse(),
            blink: pen.is_blink(),
        },
        width: cell.width(),
    }
}

fn convert_color(color: avt::Color) -> Color {
    match color {
        avt::Color::Indexed(i) => Color::Indexed(i),
        avt::Color::RGB(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_current_line() {
        let mut term = Term::new(20, 5);
        term.feed("hello");

        assert_eq!(term.text(), "hello\n\n\n\n");
        assert_eq!(term.current_line(), "hello");

        term.feed("\r\n");
        assert_eq!(term.current_line(), "");
        assert!(term.text().starts_with("hello\n"));
    }

    #[test]
    fn snapshot_text_matches_term_text() {
        let mut term = Term::new(10, 3);
        term.feed("ab\r\ncd");

        assert_eq!(term.snapshot().text(), term.text());
        assert_eq!(term.text(), "ab\ncd\n");
    }

    #[test]
    fn sgr_indexed_color() {
        let mut term = Term::new(10, 2);
        term.feed("\x1b[31mred\x1b[0m!");

        let snap = term.snapshot();

        for col in 0..3 {
            let cell = snap.cell(col, 0);
            assert_eq!(cell.fg, Some(Color::Indexed(1)), "col {col}");
            assert_eq!(cell.bg, None);
        }

        // After the reset, the '!' has default colors.
        assert_eq!(snap.cell(3, 0).ch, '!');
        assert_eq!(snap.cell(3, 0).fg, None);
    }

    #[test]
    fn sgr_rgb_color() {
        let mut term = Term::new(10, 2);
        term.feed("\x1b[38;2;10;20;30m\x1b[48;2;1;2;3mX");

        let cell = *term.snapshot().cell(0, 0);
        assert_eq!(cell.ch, 'X');
        assert_eq!(cell.fg, Some(Color::Rgb(10, 20, 30)));
        assert_eq!(cell.bg, Some(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn sgr_attrs() {
        let mut term = Term::new(10, 2);
        term.feed("\x1b[1;3;4;5;7;9mX\x1b[0m\x1b[2my");

        let snap = term.snapshot();
        let x = snap.cell(0, 0);
        assert!(x.attrs.bold);
        assert!(x.attrs.italic);
        assert!(x.attrs.underline);
        assert!(x.attrs.blink);
        assert!(x.attrs.inverse);
        assert!(x.attrs.strikethrough);
        assert!(!x.attrs.faint);

        let y = snap.cell(1, 0);
        assert!(y.attrs.faint);
        assert!(!y.attrs.bold);
        assert!(!y.attrs.italic);
    }

    #[test]
    fn wide_char_occupancy() {
        let mut term = Term::new(10, 2);
        term.feed("漢a");

        let snap = term.snapshot();
        assert_eq!(snap.cell(0, 0).ch, '漢');
        assert_eq!(snap.cell(0, 0).width, 2);
        assert_eq!(snap.cell(1, 0).width, 0); // continuation cell
        assert_eq!(snap.cell(2, 0).ch, 'a');
        assert_eq!(snap.cell(2, 0).width, 1);

        // Continuation cells are skipped in the text projections.
        assert_eq!(term.current_line(), "漢a");
        assert_eq!(snap.text(), "漢a\n");
    }

    #[test]
    fn cursor_position_and_visibility() {
        let mut term = Term::new(10, 3);
        term.feed("ab");

        let cursor = term.snapshot().cursor;
        assert_eq!((cursor.col, cursor.row), (2, 0));
        assert!(cursor.visible);

        term.feed("\x1b[?25l");
        assert!(!term.snapshot().cursor.visible);

        term.feed("\x1b[?25h\r\nx");
        let cursor = term.snapshot().cursor;
        assert_eq!((cursor.col, cursor.row), (1, 1));
        assert!(cursor.visible);
    }

    #[test]
    fn resize_updates_size_and_snapshot_dims() {
        let mut term = Term::new(80, 24);
        term.feed("hi");
        term.resize(100, 30);

        assert_eq!(term.size(), (100, 30));

        let snap = term.snapshot();
        assert_eq!((snap.cols, snap.rows), (100, 30));
        assert_eq!(snap.cells.len(), 100 * 30);
        assert!(snap.text().contains("hi"));
    }

    #[test]
    fn application_cursor_mode_tracked() {
        let mut term = Term::new(10, 2);
        assert!(!term.application_cursor());

        term.feed("\x1b[?1h");
        assert!(term.application_cursor());

        term.feed("\x1b[?1l");
        assert!(!term.application_cursor());
    }
}
