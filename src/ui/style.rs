use serde::{Deserialize, Serialize};

/// Terminal dimensions in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

/// Terminal color representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    #[default]
    Default,
    Ansi(u8),
    Rgb(u8, u8, u8),
}

/// Visual cell styling attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CellStyle {
    #[serde(default)]
    pub fg: Color,
    #[serde(default)]
    pub bg: Color,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub inverse: bool,
}

impl CellStyle {
    /// Constructs a style from 256-color ANSI foreground/background indices and bold weight.
    pub const fn ansi(fg_ansi: u8, bg_ansi: u8, bold: bool) -> Self {
        Self {
            fg: Color::Ansi(fg_ansi),
            bg: Color::Ansi(bg_ansi),
            bold,
            italic: false,
            underline: false,
            inverse: false,
        }
    }

    pub const fn with_fg(mut self, fg: Color) -> Self {
        self.fg = fg;
        self
    }

    pub const fn with_bg(mut self, bg: Color) -> Self {
        self.bg = bg;
        self
    }

    pub const fn with_bold(mut self, bold: bool) -> Self {
        self.bold = bold;
        self
    }
}

/// Corner or edge anchor for floating overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    #[default]
    TopRight,
    TopLeft,
    TopCenter,
    BottomRight,
    BottomLeft,
    BottomCenter,
}

impl Anchor {
    /// Returns the horizontally opposite anchor used when dodging the cursor or text.
    pub fn opposite_horizontal(self) -> Self {
        match self {
            Anchor::TopRight => Anchor::TopLeft,
            Anchor::TopLeft => Anchor::TopRight,
            Anchor::TopCenter => Anchor::BottomCenter,
            Anchor::BottomRight => Anchor::BottomLeft,
            Anchor::BottomLeft => Anchor::BottomRight,
            Anchor::BottomCenter => Anchor::TopCenter,
        }
    }
}

/// Box border style for floating overlays and HUD panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BorderStyle {
    #[default]
    None,
    Single,
    Rounded,
    Double,
}

impl BorderStyle {
    /// Returns `(top_left, top_right, bottom_left, bottom_right, horizontal, vertical)` box chars.
    pub fn chars(self) -> Option<(char, char, char, char, char, char)> {
        match self {
            BorderStyle::None => None,
            BorderStyle::Single => Some(('┌', '┐', '└', '┘', '─', '│')),
            BorderStyle::Rounded => Some(('╭', '╮', '╰', '╯', '─', '│')),
            BorderStyle::Double => Some(('╔', '╗', '╚', '╝', '═', '║')),
        }
    }
}
