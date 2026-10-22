pub mod buffer;
pub mod rect;
pub mod style;
pub mod text;
pub mod widgets;

pub use buffer::{Buffer, Cell, CursorState};
pub use rect::Rect;
pub use style::{Anchor, BorderStyle, CellStyle, Color, TermSize};
pub use text::{Line, Span, display_width};
pub use widgets::{Block, Gauge, Sparkline, StatusBar, Toast, Widget};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_heals_split_cjk_wide_characters_and_renders_multi_span_widgets() {
        let mut buf = Buffer::new(16, 3);
        let base = CellStyle::default();
        buf.set_str(0, 0, "あいうえお", base);
        assert_eq!(
            buf.to_plain_text().lines().next().unwrap(),
            "あいうえお      "
        );

        // Overwrite column 1 (2nd half of 'あ') with 'X' -> column 0 ('あ' head) heals to space
        buf.set_char(0, 1, 'X', 1, base);
        assert_eq!(
            buf.to_plain_text().lines().next().unwrap(),
            " Xいうえお      "
        );

        // Render a Gauge into row 1
        let gauge = Gauge::new(50, 4, Some("B"), CellStyle::ansi(231, 236, true));
        let line = gauge.to_line();
        (&line).render(Rect::new(0, 1, 16, 1), &mut buf);
        assert!(
            buf.to_plain_text()
                .lines()
                .nth(1)
                .unwrap()
                .starts_with("B ━━── 50%")
        );
    }
}
