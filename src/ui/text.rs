use crate::ui::style::{CellStyle, Color};
use unicode_width::UnicodeWidthChar;

/// A contiguous inline run of characters sharing a single `CellStyle`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub content: String,
    pub style: CellStyle,
}

impl Span {
    pub fn styled(content: impl Into<String>, style: CellStyle) -> Self {
        Self {
            content: content.into(),
            style,
        }
    }

    /// Display width in terminal character columns.
    pub fn width(&self) -> u16 {
        display_width(&self.content)
    }

    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }
}

/// A horizontal sequence of styled `Span`s representing a single row of rich text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    pub fn new(spans: Vec<Span>) -> Self {
        Self { spans }
    }

    pub fn styled(content: impl Into<String>, style: CellStyle) -> Self {
        let s = content.into();
        if s.is_empty() {
            Self { spans: Vec::new() }
        } else {
            Self {
                spans: vec![Span::styled(s, style)],
            }
        }
    }

    pub fn push_span(&mut self, span: Span) {
        if !span.is_empty() {
            self.spans.push(span);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty() || self.spans.iter().all(Span::is_empty)
    }

    /// Total display width across all spans in terminal columns.
    pub fn width(&self) -> u16 {
        self.spans.iter().map(Span::width).sum()
    }

    /// Wraps this line with optional `prefix` and `suffix` spans using `affix_style`.
    pub fn with_affixes(
        mut self,
        prefix: Option<&str>,
        suffix: Option<&str>,
        affix_style: CellStyle,
    ) -> Self {
        if self.is_empty() {
            return self;
        }
        if let Some(pre) = prefix
            && !pre.is_empty()
        {
            self.spans.insert(0, Span::styled(pre, affix_style));
        }
        if let Some(suf) = suffix
            && !suf.is_empty()
        {
            self.spans.push(Span::styled(suf, affix_style));
        }
        self
    }

    /// Decorates a segment line with an optional leading bold `icon`, inner `padding` (`" "`),
    /// explicit `prefix` / `suffix`, and outer Nerd Font rounded `pill` caps (`` / ``).
    #[allow(clippy::too_many_arguments)]
    pub fn decorate_segment(
        mut self,
        icon: Option<&str>,
        padding: bool,
        pill: bool,
        prefix: Option<&str>,
        suffix: Option<&str>,
        seg_style: CellStyle,
        outer_bg: Color,
    ) -> Self {
        if self.is_empty() {
            return self;
        }

        if let Some(ic) = icon
            && !ic.is_empty()
        {
            self.spans
                .insert(0, Span::styled(format!("{ic} "), seg_style.with_bold(true)));
        }

        self = self.with_affixes(prefix, suffix, seg_style);

        if padding {
            self.spans.insert(0, Span::styled(" ", seg_style));
            self.spans.push(Span::styled(" ", seg_style));
        }

        if pill {
            let cap_style = CellStyle::new(seg_style.bg, outer_bg, false);
            self.spans.insert(0, Span::styled("\u{e0b6}", cap_style));
            self.spans.push(Span::styled("\u{e0b4}", cap_style));
        }

        self
    }
}

/// Computes the Unicode column width of `text` (ignoring `\r` and `\n`).
pub fn display_width(text: &str) -> u16 {
    text.chars()
        .filter(|&c| c != '\n' && c != '\r')
        .filter_map(UnicodeWidthChar::width)
        .sum::<usize>() as u16
}
