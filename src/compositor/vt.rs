use crate::ui::{Buffer, CellStyle, Color, CursorState, Rect};
use std::num::NonZeroU16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseEventKind {
    WheelUp,
    WheelDown,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedMouseEvent {
    pub kind: MouseEventKind,
    pub btn: u16,
    pub col: u16,
    pub row: u16,
    pub release: bool,
    pub is_sgr: bool,
    pub len: usize,
}

/// Parses an SGR (`\x1b[<btn;col;rowM`/`m`) or X10 (`\x1b[M<b1><b2><b3>`) mouse sequence
/// from the start of `bytes`.
pub(crate) fn parse_mouse_event(bytes: &[u8]) -> Option<ParsedMouseEvent> {
    if bytes.starts_with(b"\x1b[<") {
        let mut end = 3;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b';') {
            end += 1;
        }
        if end < bytes.len() && (bytes[end] == b'M' || bytes[end] == b'm') {
            let release = bytes[end] == b'm';
            let body = std::str::from_utf8(&bytes[3..end]).ok()?;
            let mut parts = body.split(';');
            let btn: u16 = parts.next()?.parse().ok()?;
            let col: u16 = parts.next()?.parse().ok()?;
            let row: u16 = parts.next()?.parse().ok()?;
            if parts.next().is_some() {
                return None;
            }
            let kind = if !release && (btn & 64) != 0 {
                match btn & 3 {
                    0 => MouseEventKind::WheelUp,
                    1 => MouseEventKind::WheelDown,
                    _ => MouseEventKind::Other,
                }
            } else {
                MouseEventKind::Other
            };
            return Some(ParsedMouseEvent {
                kind,
                btn,
                col,
                row,
                release,
                is_sgr: true,
                len: end + 1,
            });
        }
    } else if bytes.starts_with(b"\x1b[M") && bytes.len() >= 6 {
        let btn = u16::from(bytes[3].saturating_sub(32));
        let col = u16::from(bytes[4].saturating_sub(32));
        let row = u16::from(bytes[5].saturating_sub(32));
        let kind = if (btn & 64) != 0 {
            match btn & 3 {
                0 => MouseEventKind::WheelUp,
                1 => MouseEventKind::WheelDown,
                _ => MouseEventKind::Other,
            }
        } else {
            MouseEventKind::Other
        };
        return Some(ParsedMouseEvent {
            kind,
            btn,
            col,
            row,
            release: false,
            is_sgr: false,
            len: 6,
        });
    }
    None
}

/// Internal adapter wrapping `vt100::Parser` (dual Primary/Alternate screen state machine),
/// terminal capability query responder (`DA1`, `DA2`, `DSR`, `XTVERSION`, Kitty keyboard, `OSC 10/11`),
/// and cell projection into a 2D `Buffer`.
pub(crate) struct VtScreenAdapter {
    vt: vt100::Parser,
}

impl VtScreenAdapter {
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Self {
        let nz_rows = NonZeroU16::new(rows.max(1)).unwrap();
        let nz_cols = NonZeroU16::new(cols.max(1)).unwrap();
        Self {
            vt: vt100::Parser::new(nz_rows, nz_cols, scrollback_lines),
        }
    }

    pub fn set_size(&mut self, cols: u16, rows: u16) {
        let nz_rows = NonZeroU16::new(rows.max(1)).unwrap();
        let nz_cols = NonZeroU16::new(cols.max(1)).unwrap();
        self.vt.set_size(nz_rows, nz_cols);
    }

    pub fn is_alternate_screen(&self) -> bool {
        self.vt.screen().alternate_screen()
    }

    /// Returns true if the child process inside the PTY has enabled mouse tracking.
    pub fn wants_mouse(&self) -> bool {
        !matches!(
            self.vt.screen().mouse_protocol_mode(),
            vt100::MouseProtocolMode::None
        )
    }

    /// Returns the current scrollback offset (`0` = at live bottom).
    #[allow(dead_code)]
    pub fn scrollback(&self) -> usize {
        self.vt.screen().scrollback()
    }

    /// Scrolls the primary screen viewport up into history by `lines` rows.
    pub fn scroll_up(&mut self, lines: usize) {
        let current = self.vt.screen().scrollback();
        self.vt
            .screen_mut()
            .set_scrollback(current.saturating_add(lines));
    }

    /// Scrolls the primary screen viewport down toward the live bottom by `lines` rows.
    pub fn scroll_down(&mut self, lines: usize) {
        let current = self.vt.screen().scrollback();
        self.vt
            .screen_mut()
            .set_scrollback(current.saturating_sub(lines));
    }

    /// Resets the scrollback offset to `0` (live bottom).
    pub fn reset_scrollback(&mut self) {
        if self.vt.screen().scrollback() > 0 {
            self.vt.screen_mut().set_scrollback(0);
        }
    }

    /// Processes raw child PTY output bytes through the VT100 parser and returns any synthetic
    /// terminal query replies that must be written back to the child PTY.
    pub fn process_and_respond(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.vt.process(bytes);
        self.respond_to_terminal_queries(bytes)
    }

    /// Projects the current virtual terminal screen cells and cursor state into `child_rect` of `buf`.
    pub fn paint_into_buffer(&self, child_rect: Rect, buf: &mut Buffer) {
        let screen = self.vt.screen();
        for r in 0..child_rect.height {
            let mut c = 0;
            while c < child_rect.width {
                if let Some(cell) = screen.cell(r, c) {
                    let style = Self::convert_vt_style(cell);
                    if cell.is_wide_continuation() {
                        c += 1;
                        continue;
                    }
                    let contents = cell.contents();
                    let ch = contents.chars().next().unwrap_or(' ');
                    let width = if cell.is_wide() { 2 } else { 1 };
                    buf.set_char(child_rect.y + r, child_rect.x + c, ch, width, style);
                    c += u16::from(width);
                } else {
                    c += 1;
                }
            }
        }

        let (cur_row, cur_col) = screen.cursor_position();
        let scrolled_back = screen.scrollback() > 0;
        buf.cursor = CursorState {
            row: child_rect.y + cur_row.min(child_rect.height.saturating_sub(1)),
            col: child_rect.x + cur_col.min(child_rect.width.saturating_sub(1)),
            hidden: screen.hide_cursor() || scrolled_back,
        };
    }

    fn respond_to_terminal_queries(&self, bytes: &[u8]) -> Vec<u8> {
        let mut reply = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != 0x1b || i + 1 >= bytes.len() {
                i += 1;
                continue;
            }
            match bytes[i + 1] {
                b'[' => {
                    let start = i + 2;
                    let mut end = start;
                    while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                        end += 1;
                    }
                    if end < bytes.len() {
                        let params = &bytes[start..end];
                        let final_byte = bytes[end];
                        match (params, final_byte) {
                            (b"" | b"0", b'c') => {
                                reply.extend_from_slice(b"\x1b[?62;22c");
                            }
                            (b">" | b">0", b'c') => {
                                reply.extend_from_slice(b"\x1b[>0;10;1c");
                            }
                            (b"5", b'n') => {
                                reply.extend_from_slice(b"\x1b[0n");
                            }
                            (b"6", b'n') => {
                                let (r, c) = self.vt.screen().cursor_position();
                                let _ = std::io::Write::write_fmt(
                                    &mut reply,
                                    format_args!("\x1b[{};{}R", r + 1, c + 1),
                                );
                            }
                            (b"?6", b'n') => {
                                let (r, c) = self.vt.screen().cursor_position();
                                let _ = std::io::Write::write_fmt(
                                    &mut reply,
                                    format_args!("\x1b[?{};{}R", r + 1, c + 1),
                                );
                            }
                            (b"?", b'u') => {
                                reply.extend_from_slice(b"\x1b[?0u");
                            }
                            (b">" | b">0", b'q') => {
                                reply.extend_from_slice(b"\x1bP>|thud(0.1.0)\x1b\\");
                            }
                            _ => {}
                        }
                        i = end + 1;
                        continue;
                    }
                }
                b']' => {
                    let start = i + 2;
                    let mut end = start;
                    while end < bytes.len() {
                        if bytes[end] == 0x07 {
                            break;
                        }
                        if bytes[end] == 0x1b && end + 1 < bytes.len() && bytes[end + 1] == b'\\' {
                            break;
                        }
                        end += 1;
                    }
                    if end < bytes.len() {
                        let osc_body = &bytes[start..end];
                        if osc_body == b"10;?" {
                            reply.extend_from_slice(b"\x1b]10;rgb:e5e5/e5e5/e5e5\x1b\\");
                        } else if osc_body == b"11;?" {
                            reply.extend_from_slice(b"\x1b]11;rgb:1c1c/1c1c/1c1c\x1b\\");
                        }
                        i = end + 1;
                        continue;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        reply
    }

    fn convert_vt_style(cell: &vt100::Cell) -> CellStyle {
        CellStyle {
            fg: Self::convert_vt_color(cell.fgcolor()),
            bg: Self::convert_vt_color(cell.bgcolor()),
            bold: cell.bold(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        }
    }

    fn convert_vt_color(c: vt100::Color) -> Color {
        match c {
            vt100::Color::Default => Color::Default,
            vt100::Color::Idx(i) => Color::Ansi(i),
            vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        }
    }
}
