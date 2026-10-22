use crate::ui::style::CellStyle;
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

    /// Re-styles any span that currently uses `from_style` to `to_style`, or applies segment overrides.
    pub fn apply_base_style(&mut self, style: CellStyle) {
        for span in &mut self.spans {
            span.style.bg = style.bg;
            if span.style.fg == crate::ui::style::Color::Default {
                span.style.fg = style.fg;
            }
        }
    }
}

/// Computes the Unicode column width of `text` (ignoring `\r` and `\n`).
pub fn display_width(text: &str) -> u16 {
    text.chars()
        .filter(|&c| c != '\n' && c != '\r')
        .filter_map(UnicodeWidthChar::width)
        .sum::<usize>() as u16
}
