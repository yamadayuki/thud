pub mod buffer;
pub mod rect;
pub mod style;
pub mod text;
pub mod widgets;

pub use buffer::{Buffer, Cell, CursorState};
pub use rect::Rect;
pub use style::{
    Anchor, BorderStyle, CellStyle, Color, TermSize, ThemeConfig, ThemePalette, ThemePreset,
};
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
        let gauge = Gauge::new(5, 4, Some("B"), CellStyle::ansi(231, 236, true));
        let line = gauge.to_line();
        (&line).render(Rect::new(0, 1, 16, 1), &mut buf);
        assert!(
            buf.to_plain_text()
                .lines()
                .nth(1)
                .unwrap()
                .starts_with("B ────   5%")
        );
    }

    #[test]
    fn color_parses_named_colors_hex_rgb_and_ansi_numbers() {
        assert_eq!(Color::parse_str("cyan").unwrap(), Color::Rgb(137, 220, 235));
        assert_eq!(Color::parse_str("#1e1e2e").unwrap(), Color::Rgb(30, 30, 46));
        assert_eq!(Color::parse_str("231").unwrap(), Color::Ansi(231));
        assert_eq!(Color::parse_str("transparent").unwrap(), Color::Default);
        assert_eq!(
            Color::parse_str("#1e1e2e80").unwrap(),
            Color::Rgba(30, 30, 46, 128)
        );
        assert_eq!(
            Color::parse_str("#1e1e2e@0.5").unwrap(),
            Color::Rgba(30, 30, 46, 128)
        );
        assert_eq!(Color::parse_str("cyan@0%").unwrap(), Color::Default);

        // Verify in-buffer alpha blending over an underlying cell background
        let mut buf = Buffer::new(4, 1);
        buf.set_char(
            0,
            0,
            'A',
            1,
            CellStyle::new(Color::Rgb(255, 255, 255), Color::Rgb(0, 0, 0), false),
        );
        buf.set_char(
            0,
            0,
            'B',
            1,
            CellStyle::new(
                Color::Rgb(255, 255, 255),
                Color::Rgba(200, 100, 50, 128),
                false,
            ),
        );
        assert_eq!(buf.get(0, 0).unwrap().style.bg, Color::Rgb(100, 50, 25));
    }

    #[test]
    fn buffer_diff_tracks_cursor_run_length_and_omits_cup() {
        let prev = Buffer::new(10, 2);
        let mut buf = Buffer::new(10, 2);
        buf.set_str(0, 0, "hello", CellStyle::default());
        buf.set_str(1, 5, "world", CellStyle::default());

        let diff_bytes = buf.diff(Some(&prev));
        let diff_str = String::from_utf8(diff_bytes).unwrap();

        // Must position at (1, 1) for "hello", but omit CUP between 'h', 'e', 'l', 'l', 'o'
        assert!(diff_str.contains("\x1b[1;1H\x1b[0mhello"));
        // Must position at (2, 6) for "world", but omit CUP between 'w', 'o', 'r', 'l', 'd'
        assert!(diff_str.contains("\x1b[2;6Hworld"));
        // Should NOT contain individual cell CUPs like \x1b[1;2H
        assert!(!diff_str.contains("\x1b[1;2H"));
        assert!(!diff_str.contains("\x1b[2;7H"));
    }
}
