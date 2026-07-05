//! Shared boundary types between the session engine and the renderer/encoders.
//!
//! The session side (term.rs) converts avt state into these types; the render
//! side (render/, encode/) consumes them. Keeping this seam avt-free means the
//! renderer never depends on the emulator crate.

use std::time::Duration;

/// A resolved or indexed terminal color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    /// ANSI palette index: 0-15 themed, 16-231 the 6×6×6 cube, 232-255 grayscale.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Cell text attributes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CellAttrs {
    pub bold: bool,
    pub faint: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub inverse: bool,
    pub blink: bool,
}

/// One terminal cell. Wide characters occupy their leading cell with
/// `width == 2`; the following continuation cell has `width == 0` and must not
/// be drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub attrs: CellAttrs,
    pub width: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: None,
            bg: None,
            attrs: CellAttrs::default(),
            width: 1,
        }
    }
}

/// Cursor position and visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub col: usize,
    pub row: usize,
    pub visible: bool,
}

/// An immutable snapshot of the visible screen: `rows × cols` cells in
/// row-major order, plus the cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct GridSnapshot {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<Cell>,
    pub cursor: Cursor,
}

impl GridSnapshot {
    pub fn cell(&self, col: usize, row: usize) -> &Cell {
        &self.cells[row * self.cols + col]
    }

    /// The screen as plain text: one line per row, trailing whitespace trimmed
    /// per line (matching VHS's buffer semantics for Wait/Assert/goldens).
    pub fn text(&self) -> String {
        let mut lines = Vec::with_capacity(self.rows);
        for row in 0..self.rows {
            let mut line = String::with_capacity(self.cols);
            for col in 0..self.cols {
                let cell = self.cell(col, row);
                if cell.width == 0 {
                    continue; // wide-char continuation
                }
                line.push(cell.ch);
            }
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
    }
}

/// One recorded session event, timestamped relative to session start.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEventKind {
    /// Decoded UTF-8 output from the child (already boundary-safe).
    Output(String),
    /// Terminal resize to (cols, rows).
    Resize(usize, usize),
    /// Child exited.
    Exit,
    /// Frame capture toggled by Hide/Show: false = hidden section begins.
    Visibility(bool),
}

/// Timestamped event in the session log. The log replays through a fresh
/// emulator at encode time to produce GIF frames and .cast output.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEvent {
    pub time: Duration,
    pub kind: SessionEventKind,
}
