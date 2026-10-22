use crate::ui::{Buffer, CellStyle, Color, CursorState, TermSize};

/// Internal adapter wrapping `vt100::Parser` (dual Primary/Alternate screen state machine),
/// terminal capability query responder (`DA1`, `DA2`, `DSR`, `XTVERSION`, Kitty keyboard, `OSC 10/11`),
/// and cell projection into a 2D `Buffer`.
pub(crate) struct VtScreenAdapter {
    vt: vt100::Parser,
}

impl VtScreenAdapter {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            vt: vt100::Parser::new(rows, cols, 0),
        }
    }

    pub fn set_size(&mut self, cols: u16, rows: u16) {
        self.vt.set_size(rows, cols);
    }

    pub fn is_alternate_screen(&self) -> bool {
        self.vt.screen().alternate_screen()
    }

    /// Processes raw child PTY output bytes through the VT100 parser and returns any synthetic
    /// terminal query replies that must be written back to the child PTY.
    pub fn process_and_respond(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.vt.process(bytes);
        self.respond_to_terminal_queries(bytes)
    }

    /// Projects the current virtual terminal screen cells and cursor state into the top
    /// `child_size` area of `buf`.
    pub fn paint_into_buffer(&self, child_size: TermSize, buf: &mut Buffer) {
        let screen = self.vt.screen();
        for r in 0..child_size.rows {
            let mut c = 0;
            while c < child_size.cols {
                if let Some(cell) = screen.cell(r, c) {
                    let style = Self::convert_vt_style(cell);
                    if cell.is_wide_continuation() {
                        c += 1;
                        continue;
                    }
                    let contents = cell.contents();
                    let ch = contents.chars().next().unwrap_or(' ');
                    let width = if cell.is_wide() { 2 } else { 1 };
                    buf.set_char(r, c, ch, width, style);
                    c += u16::from(width);
                } else {
                    c += 1;
                }
            }
        }

        let (cur_row, cur_col) = screen.cursor_position();
        buf.cursor = CursorState {
            row: cur_row.min(child_size.rows.saturating_sub(1)),
            col: cur_col.min(child_size.cols.saturating_sub(1)),
            hidden: screen.hide_cursor(),
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
