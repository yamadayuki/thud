use crate::ui::rect::Rect;
use crate::ui::style::{CellStyle, Color};
use crate::ui::text::{Line, Span};
use std::fmt::Write as _;
use unicode_width::UnicodeWidthChar;

/// A single character cell in the 2D `Buffer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// Text content of this cell (empty string if this cell is the 2nd column of a wide char).
    pub ch: String,
    /// Display width of this cell: `1` for normal, `2` for wide head cell, `0` for wide continuation cell.
    pub width: u8,
    pub style: CellStyle,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: " ".to_string(),
            width: 1,
            style: CellStyle::default(),
        }
    }
}

/// Terminal cursor position and visibility state associated with a `Buffer` frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorState {
    pub row: u16,
    pub col: u16,
    pub hidden: bool,
}

/// A 2D grid of styled terminal cells (`Cell`) that automatically heals split CJK 2-cell
/// wide characters on overwrite and generates minimal ANSI SGR/CUP diffs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buffer {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Cell>,
    pub cursor: CursorState,
}

impl Buffer {
    pub fn new(cols: u16, rows: u16) -> Self {
        let len = usize::from(cols) * usize::from(rows);
        Self {
            cols,
            rows,
            cells: vec![Cell::default(); len],
            cursor: CursorState::default(),
        }
    }

    pub const fn area(&self) -> Rect {
        Rect::new(0, 0, self.cols, self.rows)
    }

    fn idx(&self, row: u16, col: u16) -> Option<usize> {
        if row < self.rows && col < self.cols {
            Some(usize::from(row) * usize::from(self.cols) + usize::from(col))
        } else {
            None
        }
    }

    pub fn get(&self, row: u16, col: u16) -> Option<&Cell> {
        self.idx(row, col).map(|i| &self.cells[i])
    }

    /// Fills a rectangular region with spaces of `style`.
    pub fn fill_rect(&mut self, rect: Rect, style: CellStyle) {
        let clipped = rect.intersection(self.area());
        for r in clipped.y..clipped.bottom() {
            for c in clipped.x..clipped.right() {
                self.set_char(r, c, ' ', 1, style);
            }
        }
    }

    /// Writes a single character at `(row, col)` while healing any split 2-cell wide characters
    /// at the left or right boundary of the overwrite.
    pub fn set_char(&mut self, row: u16, col: u16, c: char, char_width: u8, style: CellStyle) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        if char_width == 2 && col + 1 >= self.cols {
            self.set_char(row, col, ' ', 1, style);
            return;
        }

        // 1. Heal left neighbor if overwriting the continuation cell (2nd half) of a wide char.
        if let Some(cur_idx) = self.idx(row, col)
            && self.cells[cur_idx].width == 0
            && col > 0
            && let Some(left_idx) = self.idx(row, col - 1)
        {
            let bg_style = self.cells[left_idx].style;
            self.cells[left_idx] = Cell {
                ch: " ".to_string(),
                width: 1,
                style: bg_style,
            };
        }

        // 2. Heal right neighbor if overwriting the head cell (1st half) of a wide char with a 1-col char.
        if char_width == 1
            && let Some(cur_idx) = self.idx(row, col)
            && self.cells[cur_idx].width == 2
            && let Some(right_idx) = self.idx(row, col + 1)
        {
            let bg_style = self.cells[right_idx].style;
            self.cells[right_idx] = Cell {
                ch: " ".to_string(),
                width: 1,
                style: bg_style,
            };
        }

        // 3. If writing a 2-column character, also check what resides at `col + 1`.
        if char_width == 2
            && let Some(next_idx) = self.idx(row, col + 1)
            && self.cells[next_idx].width == 2
            && let Some(after_idx) = self.idx(row, col + 2)
        {
            let bg_style = self.cells[after_idx].style;
            self.cells[after_idx] = Cell {
                ch: " ".to_string(),
                width: 1,
                style: bg_style,
            };
        }

        let mut resolved_style = style;
        if let Some(idx) = self.idx(row, col) {
            let prev_style = self.cells[idx].style;
            let blended_bg = style.bg.blend_over(prev_style.bg, (26, 27, 38));
            let blended_fg = style.fg.blend_over(blended_bg, (26, 27, 38));
            resolved_style.bg = blended_bg;
            resolved_style.fg = blended_fg;
            self.cells[idx] = Cell {
                ch: c.to_string(),
                width: char_width,
                style: resolved_style,
            };
        }

        if char_width == 2
            && let Some(next_idx) = self.idx(row, col + 1)
        {
            self.cells[next_idx] = Cell {
                ch: String::new(),
                width: 0,
                style: resolved_style,
            };
        }
    }

    /// Paints a string starting at `(row, start_col)`, clipped to `max_col` (and `self.cols`).
    /// Returns the number of columns written.
    pub fn set_str_clipped(
        &mut self,
        row: u16,
        start_col: u16,
        max_col: u16,
        text: &str,
        style: CellStyle,
    ) -> u16 {
        let limit = max_col.min(self.cols);
        let mut col = start_col;
        for c in text.chars() {
            if c == '\n' || c == '\r' {
                continue;
            }
            let w = UnicodeWidthChar::width(c).unwrap_or(0);
            if w == 0 {
                continue;
            }
            let w_u8 = w as u8;
            if col + u16::from(w_u8) > limit {
                break;
            }
            self.set_char(row, col, c, w_u8, style);
            col += u16::from(w_u8);
        }
        col.saturating_sub(start_col)
    }

    /// Paints a string starting at `(row, start_col)`, clipped to `self.cols`.
    pub fn set_str(&mut self, row: u16, start_col: u16, text: &str, style: CellStyle) -> u16 {
        self.set_str_clipped(row, start_col, self.cols, text, style)
    }

    /// Paints a single `Span` starting at `(row, start_col)`, clipped to `max_col`.
    pub fn set_span(&mut self, row: u16, start_col: u16, max_col: u16, span: &Span) -> u16 {
        self.set_str_clipped(row, start_col, max_col, &span.content, span.style)
    }

    /// Paints a multi-span `Line` starting at `(row, start_col)`, clipped to `max_col`.
    pub fn set_line(&mut self, row: u16, start_col: u16, max_col: u16, line: &Line) -> u16 {
        let mut col = start_col;
        for span in &line.spans {
            if col >= max_col {
                break;
            }
            col += self.set_span(row, col, max_col, span);
        }
        col.saturating_sub(start_col)
    }

    /// Returns the buffer contents as plain text (`\n`-separated rows).
    pub fn to_plain_text(&self) -> String {
        let mut out = String::new();
        for r in 0..self.rows {
            if r > 0 {
                out.push('\n');
            }
            for c in 0..self.cols {
                if let Some(cell) = self.get(r, c)
                    && cell.width > 0
                {
                    out.push_str(&cell.ch);
                }
            }
        }
        out
    }

    /// Computes the minimal ANSI escape sequence diff between `prev` and `self`,
    /// restoring the terminal cursor position and visibility at the end of the frame.
    pub fn diff(&self, prev: Option<&Buffer>) -> Vec<u8> {
        let mut ansi = String::new();
        let mut any_cell_changed = false;
        let mut current_style: Option<CellStyle> = None;

        for r in 0..self.rows {
            let mut c = 0;
            while c < self.cols {
                let next_cell =
                    &self.cells[usize::from(r) * usize::from(self.cols) + usize::from(c)];
                if next_cell.width == 0 {
                    c += 1;
                    continue;
                }

                let is_same = if let Some(prev_buf) = prev {
                    if prev_buf.cols == self.cols && prev_buf.rows == self.rows {
                        let idx = usize::from(r) * usize::from(self.cols) + usize::from(c);
                        let same_head = prev_buf.cells[idx] == *next_cell;
                        let same_cont = if next_cell.width == 2 && c + 1 < self.cols {
                            prev_buf.cells[idx + 1] == self.cells[idx + 1]
                        } else {
                            true
                        };
                        same_head && same_cont
                    } else {
                        false
                    }
                } else {
                    false
                };

                if !is_same {
                    if !any_cell_changed {
                        ansi.push_str("\x1b[?25l");
                        any_cell_changed = true;
                    }
                    let _ = write!(ansi, "\x1b[{};{}H", r + 1, c + 1);
                    if current_style != Some(next_cell.style) {
                        Self::write_sgr(&mut ansi, next_cell.style);
                        current_style = Some(next_cell.style);
                    }
                    ansi.push_str(&next_cell.ch);
                }

                c += u16::from(next_cell.width.max(1));
            }
        }

        let cursor_changed = prev.is_none_or(|p| p.cursor != self.cursor);

        if any_cell_changed || cursor_changed {
            ansi.push_str("\x1b[0m");
            let _ = write!(
                ansi,
                "\x1b[{};{}H",
                self.cursor.row + 1,
                self.cursor.col + 1
            );
            if self.cursor.hidden {
                ansi.push_str("\x1b[?25l");
            } else {
                ansi.push_str("\x1b[?25h");
            }
        }

        ansi.into_bytes()
    }

    fn write_sgr(out: &mut String, style: CellStyle) {
        out.push_str("\x1b[0");
        if style.bold {
            out.push_str(";1");
        }
        if style.italic {
            out.push_str(";3");
        }
        if style.underline {
            out.push_str(";4");
        }
        if style.inverse {
            out.push_str(";7");
        }
        match style.fg {
            Color::Default | Color::Rgba(_, _, _, 0) => {}
            Color::Ansi(idx) => {
                let _ = write!(out, ";38;5;{idx}");
            }
            Color::Rgb(r, g, b) | Color::Rgba(r, g, b, _) => {
                let _ = write!(out, ";38;2;{r};{g};{b}");
            }
        }
        match style.bg {
            Color::Default | Color::Rgba(_, _, _, 0) => {}
            Color::Ansi(idx) => {
                let _ = write!(out, ";48;5;{idx}");
            }
            Color::Rgb(r, g, b) | Color::Rgba(r, g, b, _) => {
                let _ = write!(out, ";48;2;{r};{g};{b}");
            }
        }
        out.push('m');
    }
}
