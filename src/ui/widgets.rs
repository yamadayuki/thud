use crate::ui::buffer::Buffer;
use crate::ui::rect::Rect;
use crate::ui::style::{BorderStyle, CellStyle, Color};
use crate::ui::text::{Line, Span, display_width};
use std::collections::VecDeque;

const SPARK_BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Ratatui-inspired 2D widget interface that renders into a bounded `Rect` of a `Buffer`.
pub trait Widget {
    fn render(self, area: Rect, buf: &mut Buffer);
}

impl Widget for &Line {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        buf.set_line(area.y, area.x, area.right(), self);
    }
}

/// A bordered container widget (`Single`, `Rounded`, `Double`, or `None`) with an optional styled title,
/// horizontal inner padding (`padding_x`), and multi-widget inner content lines.
#[derive(Debug, Clone)]
pub struct Block<'a> {
    pub border: BorderStyle,
    pub title: Option<&'a str>,
    pub base_style: CellStyle,
    pub border_style: CellStyle,
    pub title_style: CellStyle,
    pub padding_x: u16,
    pub lines: Vec<Line>,
}

impl<'a> Block<'a> {
    pub fn new(
        border: BorderStyle,
        title: Option<&'a str>,
        base_style: CellStyle,
        lines: Vec<Line>,
    ) -> Self {
        let border_style = base_style.with_bold(false);
        let title_style = base_style.with_bold(true);
        Self {
            border,
            title,
            base_style,
            border_style,
            title_style,
            padding_x: 0,
            lines,
        }
    }

    pub fn with_border_and_title_fg(
        mut self,
        border_fg: Option<Color>,
        title_fg: Option<Color>,
    ) -> Self {
        if let Some(bfg) = border_fg {
            self.border_style.fg = bfg;
        }
        if let Some(tfg) = title_fg {
            self.title_style.fg = tfg;
        }
        self
    }

    pub fn with_padding_x(mut self, padding_x: u16) -> Self {
        self.padding_x = padding_x;
        self
    }

    /// Computes the `(width, height)` required to fit this block, its horizontal padding, and its inner lines.
    pub fn measure(&self) -> (u16, u16) {
        if self.lines.is_empty() {
            return (0, 0);
        }
        let pad_both = self.padding_x.saturating_mul(2);
        let max_content_width = self
            .lines
            .iter()
            .map(Line::width)
            .max()
            .unwrap_or(0)
            .saturating_add(pad_both);

        if self.border == BorderStyle::None {
            return (max_content_width, self.lines.len() as u16);
        }
        let title_width = self.title.map_or(0, display_width);
        let inner_width = max_content_width.max(title_width);
        (inner_width + 2, self.lines.len() as u16 + 2)
    }
}

impl Widget for Block<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() || self.lines.is_empty() {
            return;
        }

        let Some((tl, tr, bl, br, hz, vt)) = self.border.chars() else {
            if self.padding_x > 0 {
                buf.fill_rect(area, self.base_style);
            }
            for (idx, line) in self.lines.iter().enumerate() {
                let row = area.y + idx as u16;
                if row >= area.bottom() {
                    break;
                }
                let start_col = (area.x + self.padding_x).min(area.right());
                buf.set_line(
                    row,
                    start_col,
                    area.right().saturating_sub(self.padding_x),
                    line,
                );
            }
            return;
        };

        if area.width < 2 || area.height < 2 {
            return;
        }

        // Fill background inside the block area
        buf.fill_rect(area, self.base_style);

        let top_row = area.y;
        let bot_row = area.bottom() - 1;
        let left_col = area.x;
        let right_col = area.right() - 1;
        let inner_width = area.width.saturating_sub(2);

        // Top border + optional title
        buf.set_char(top_row, left_col, tl, 1, self.border_style);
        let mut col = left_col + 1;
        if let Some(title_str) = self.title {
            let tw = display_width(title_str);
            if tw > 0 && tw <= inner_width {
                col += buf.set_str_clipped(top_row, col, right_col, title_str, self.title_style);
            }
        }
        while col < right_col {
            buf.set_char(top_row, col, hz, 1, self.border_style);
            col += 1;
        }
        buf.set_char(top_row, right_col, tr, 1, self.border_style);

        // Content rows with vertical borders and horizontal padding
        let inner_area = area.inner(1, 1);
        let content_start_col = (inner_area.x + self.padding_x).min(inner_area.right());
        let content_max_col = inner_area.right().saturating_sub(self.padding_x);
        for (idx, line) in self.lines.iter().enumerate() {
            let r = inner_area.y + idx as u16;
            if r >= bot_row {
                break;
            }
            buf.set_char(r, left_col, vt, 1, self.border_style);
            buf.set_line(r, content_start_col, content_max_col, line);
            buf.set_char(r, right_col, vt, 1, self.border_style);
        }

        // Bottom border
        buf.set_char(bot_row, left_col, bl, 1, self.border_style);
        for c in (left_col + 1)..right_col {
            buf.set_char(bot_row, c, hz, 1, self.border_style);
        }
        buf.set_char(bot_row, right_col, br, 1, self.border_style);
    }
}

/// Multi-span horizontal progress bar component (`Label ━━━─── 60%`).
#[derive(Debug, Clone)]
pub struct Gauge<'a> {
    pub percent: u8,
    pub width: usize,
    pub label: Option<&'a str>,
    pub base_style: CellStyle,
    pub track_fg: Option<Color>,
}

impl<'a> Gauge<'a> {
    pub fn new(percent: u8, width: usize, label: Option<&'a str>, base_style: CellStyle) -> Self {
        Self {
            percent: percent.min(100),
            width,
            label,
            base_style,
            track_fg: None,
        }
    }

    pub fn with_track_fg(mut self, track_fg: Option<Color>) -> Self {
        self.track_fg = track_fg;
        self
    }

    /// Builds a rich multi-span `Line` where the label, filled bar (`━`), dimmed track (`─`),
    /// and percentage (`%`) have distinct visual styles while preserving exact plain-text layout.
    pub fn to_line(&self) -> Line {
        let clamped = self.percent.min(100);
        let filled = (usize::from(clamped) * self.width) / 100;
        let empty = self.width.saturating_sub(filled);

        let filled_str: String = std::iter::repeat_n('━', filled).collect();
        let empty_str: String = std::iter::repeat_n('─', empty).collect();

        let label_style = self.base_style.with_bold(true);
        let filled_style = self.base_style.with_bold(true);
        let resolved_track_fg = self.track_fg.unwrap_or(match self.base_style.bg {
            Color::Ansi(bg) if (232..=245).contains(&bg) => Color::Ansi(242),
            _ => self.base_style.fg,
        });
        let track_style = self.base_style.with_fg(resolved_track_fg).with_bold(false);

        let mut spans = Vec::with_capacity(4);
        if let Some(lbl) = self.label
            && !lbl.is_empty()
        {
            spans.push(Span::styled(format!("{lbl} "), label_style));
        }
        if self.width == 0 {
            spans.push(Span::styled(format!("{clamped:>3}%"), label_style));
            return Line::new(spans);
        }
        if !filled_str.is_empty() {
            spans.push(Span::styled(filled_str, filled_style));
        }
        if !empty_str.is_empty() {
            spans.push(Span::styled(empty_str, track_style));
        }
        spans.push(Span::styled(format!(" {clamped:>3}%"), label_style));
        Line::new(spans)
    }
}

/// Historical mini-graph component (`▁▂▃▄▅▆▇█`).
#[derive(Debug, Clone)]
pub struct Sparkline<'a> {
    pub history: &'a VecDeque<u8>,
    pub width: usize,
    pub label: Option<&'a str>,
    pub style: CellStyle,
}

impl<'a> Sparkline<'a> {
    pub fn new(
        history: &'a VecDeque<u8>,
        width: usize,
        label: Option<&'a str>,
        style: CellStyle,
    ) -> Self {
        Self {
            history,
            width,
            label,
            style,
        }
    }

    pub fn to_line(&self) -> Line {
        if self.history.is_empty() || self.width == 0 {
            return Line::default();
        }
        let mut out = String::new();
        let len = self.history.len();
        let pad = self.width.saturating_sub(len);
        for _ in 0..pad {
            out.push(SPARK_BLOCKS[0]);
        }
        for &val in self.history.iter().skip(len.saturating_sub(self.width)) {
            let idx = (usize::from(val.min(100)) * 7) / 100;
            out.push(SPARK_BLOCKS[idx]);
        }
        let label_style = self.style.with_bold(true);
        let mut spans = Vec::with_capacity(2);
        if let Some(lbl) = self.label
            && !lbl.is_empty()
        {
            spans.push(Span::styled(format!("{lbl} "), label_style));
        }
        spans.push(Span::styled(out, self.style));
        Line::new(spans)
    }
}

/// Docked status bar widget that aligns left, center, and right `Line` segments inside `area`.
#[derive(Debug, Clone)]
pub struct StatusBar {
    pub base_style: CellStyle,
    pub left: Line,
    pub center: Line,
    pub right: Line,
}

impl Widget for StatusBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        buf.fill_rect(area, self.base_style);

        let row = area.y;
        // 1. Left segments
        buf.set_line(row, area.x, area.right(), &self.left);

        // 2. Center segments
        let center_w = self.center.width();
        if center_w > 0 {
            let c_col = area.x + area.width.saturating_sub(center_w) / 2;
            buf.set_line(row, c_col, area.right(), &self.center);
        }

        // 3. Right segments
        let right_w = self.right.width();
        if right_w > 0 {
            let r_col = area.x + area.width.saturating_sub(right_w);
            buf.set_line(row, r_col, area.right(), &self.right);
        }
    }
}

/// A single toast badge widget with styled icon and message.
#[derive(Debug, Clone)]
pub struct Toast<'a> {
    pub icon: &'a str,
    pub message: &'a str,
    pub style: CellStyle,
}

impl<'a> Toast<'a> {
    pub fn to_line(&self) -> Line {
        Line::new(vec![
            Span::styled(format!(" {}", self.icon), self.style.with_bold(true)),
            Span::styled(format!("{} ", self.message), self.style),
        ])
    }
}
