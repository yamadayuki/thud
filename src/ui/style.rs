use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Terminal dimensions in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

/// Terminal color representation supporting default terminal color, 256-color ANSI index,
/// and 24-bit TrueColor RGB.
///
/// In TOML/JSON configuration, `Color` can be written as:
/// - An ANSI 256-color integer (`0..=255`, e.g. `fg = 231`)
/// - A 24-bit hex RGB string (`fg = "#89b4fa"`)
/// - A semantic color name (`fg = "cyan"`, `"blue"`, `"green"`, `"magenta"`, `"yellow"`,
///   `"red"`, `"white"`, `"black"`, `"gray"`, `"dark_gray"`, `"orange"`, `"purple"`,
///   `"pink"`, `"teal"`, `"lavender"`, `"peach"`, `"sky"`, `"mauve"`, `"emerald"`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Ansi(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    /// Parses a string specification (`"#rrggbb"`, `"cyan"`, or `"231"`) into a `Color`.
    pub fn parse_str(s: &str) -> Result<Self, String> {
        let trimmed = s.trim();
        if let Some(hex) = trimmed.strip_prefix('#') {
            if hex.len() == 6 {
                let r = u8::from_str_radix(&hex[0..2], 16)
                    .map_err(|_| format!("invalid hex color '{s}'"))?;
                let g = u8::from_str_radix(&hex[2..4], 16)
                    .map_err(|_| format!("invalid hex color '{s}'"))?;
                let b = u8::from_str_radix(&hex[4..6], 16)
                    .map_err(|_| format!("invalid hex color '{s}'"))?;
                return Ok(Color::Rgb(r, g, b));
            }
            return Err(format!("hex color '{s}' must be 6 hex digits (#rrggbb)"));
        }

        if let Ok(idx) = trimmed.parse::<u8>() {
            return Ok(Color::Ansi(idx));
        }

        match trimmed.to_ascii_lowercase().as_str() {
            "default" | "none" | "reset" => Ok(Color::Default),
            "black" => Ok(Color::Ansi(16)),
            "white" => Ok(Color::Ansi(231)),
            "gray" | "grey" => Ok(Color::Ansi(245)),
            "dark_gray" | "dark_grey" | "charcoal" => Ok(Color::Ansi(236)),
            "red" => Ok(Color::Rgb(243, 139, 168)),
            "green" => Ok(Color::Rgb(166, 227, 161)),
            "yellow" => Ok(Color::Rgb(249, 226, 175)),
            "blue" => Ok(Color::Rgb(137, 180, 250)),
            "magenta" | "purple" | "mauve" => Ok(Color::Rgb(203, 166, 247)),
            "cyan" | "sky" => Ok(Color::Rgb(137, 220, 235)),
            "teal" => Ok(Color::Rgb(148, 226, 213)),
            "orange" | "peach" => Ok(Color::Rgb(250, 179, 135)),
            "pink" => Ok(Color::Rgb(245, 194, 231)),
            "lavender" => Ok(Color::Rgb(180, 190, 254)),
            "emerald" => Ok(Color::Rgb(16, 185, 129)),
            other => Err(format!(
                "unknown color '{other}' (expected color name, '#rrggbb', or 0..255)"
            )),
        }
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match *self {
            Color::Default => serializer.serialize_str("default"),
            Color::Ansi(idx) => serializer.serialize_u8(idx),
            Color::Rgb(r, g, b) => serializer.serialize_str(&format!("#{r:02x}{g:02x}{b:02x}")),
        }
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ColorVisitor;

        impl Visitor<'_> for ColorVisitor {
            type Value = Color;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(
                    "a color name (\"cyan\"), hex RGB (\"#rrggbb\"), or ANSI 256 index (0..=255)",
                )
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                if value <= 255 {
                    Ok(Color::Ansi(value as u8))
                } else {
                    Err(E::custom(format!(
                        "ANSI color index {value} out of range 0..=255"
                    )))
                }
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                if (0..=255).contains(&value) {
                    Ok(Color::Ansi(value as u8))
                } else {
                    Err(E::custom(format!(
                        "ANSI color index {value} out of range 0..=255"
                    )))
                }
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Color::parse_str(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_any(ColorVisitor)
    }
}

/// Built-in color theme presets for `thud`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreset {
    #[default]
    Default,
    TokyoNight,
    Catppuccin,
    Cyberpunk,
    Nord,
    Gruvbox,
}

/// Resolved palette for a `ThemePreset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    pub bar_fg: Color,
    pub bar_bg: Color,
    pub overlay_fg: Color,
    pub overlay_bg: Color,
    pub border_fg: Color,
    pub title_fg: Color,
    pub track_fg: Color,
    pub cwd_accent: (Color, Color),
    pub git_accent: (Color, Color),
    pub cmd_accent: (Color, Color),
    pub metric_accent: (Color, Color),
    pub battery_accent: (Color, Color),
}

impl ThemePreset {
    pub const fn palette(self) -> ThemePalette {
        match self {
            ThemePreset::Default => ThemePalette {
                bar_fg: Color::Ansi(255),
                bar_bg: Color::Ansi(236),
                overlay_fg: Color::Ansi(231),
                overlay_bg: Color::Ansi(238),
                border_fg: Color::Ansi(245),
                title_fg: Color::Ansi(231),
                track_fg: Color::Ansi(242),
                cwd_accent: (Color::Ansi(255), Color::Ansi(236)),
                git_accent: (Color::Ansi(255), Color::Ansi(236)),
                cmd_accent: (Color::Ansi(255), Color::Ansi(236)),
                metric_accent: (Color::Ansi(255), Color::Ansi(236)),
                battery_accent: (Color::Ansi(255), Color::Ansi(236)),
            },
            ThemePreset::TokyoNight => ThemePalette {
                bar_fg: Color::Rgb(192, 202, 245),
                bar_bg: Color::Rgb(26, 27, 38),
                overlay_fg: Color::Rgb(192, 202, 245),
                overlay_bg: Color::Rgb(36, 40, 59),
                border_fg: Color::Rgb(122, 162, 247),
                title_fg: Color::Rgb(125, 207, 255),
                track_fg: Color::Rgb(65, 72, 104),
                cwd_accent: (Color::Rgb(26, 27, 38), Color::Rgb(122, 162, 247)),
                git_accent: (Color::Rgb(26, 27, 38), Color::Rgb(187, 154, 247)),
                cmd_accent: (Color::Rgb(26, 27, 38), Color::Rgb(255, 158, 100)),
                metric_accent: (Color::Rgb(125, 207, 255), Color::Rgb(36, 40, 59)),
                battery_accent: (Color::Rgb(26, 27, 38), Color::Rgb(158, 206, 106)),
            },
            ThemePreset::Catppuccin => ThemePalette {
                bar_fg: Color::Rgb(205, 214, 244),
                bar_bg: Color::Rgb(24, 24, 37),
                overlay_fg: Color::Rgb(205, 214, 244),
                overlay_bg: Color::Rgb(30, 30, 46),
                border_fg: Color::Rgb(180, 190, 254),
                title_fg: Color::Rgb(203, 166, 247),
                track_fg: Color::Rgb(88, 91, 112),
                cwd_accent: (Color::Rgb(17, 17, 27), Color::Rgb(137, 180, 250)),
                git_accent: (Color::Rgb(17, 17, 27), Color::Rgb(166, 227, 161)),
                cmd_accent: (Color::Rgb(17, 17, 27), Color::Rgb(250, 179, 135)),
                metric_accent: (Color::Rgb(148, 226, 213), Color::Rgb(49, 50, 68)),
                battery_accent: (Color::Rgb(17, 17, 27), Color::Rgb(249, 226, 175)),
            },
            ThemePreset::Cyberpunk => ThemePalette {
                bar_fg: Color::Rgb(0, 240, 255),
                bar_bg: Color::Rgb(13, 2, 33),
                overlay_fg: Color::Rgb(252, 238, 10),
                overlay_bg: Color::Rgb(22, 6, 54),
                border_fg: Color::Rgb(255, 0, 127),
                title_fg: Color::Rgb(0, 240, 255),
                track_fg: Color::Rgb(74, 21, 112),
                cwd_accent: (Color::Rgb(13, 2, 33), Color::Rgb(0, 240, 255)),
                git_accent: (Color::Rgb(255, 255, 255), Color::Rgb(255, 0, 127)),
                cmd_accent: (Color::Rgb(13, 2, 33), Color::Rgb(252, 238, 10)),
                metric_accent: (Color::Rgb(0, 255, 102), Color::Rgb(22, 6, 54)),
                battery_accent: (Color::Rgb(13, 2, 33), Color::Rgb(0, 255, 102)),
            },
            ThemePreset::Nord => ThemePalette {
                bar_fg: Color::Rgb(216, 222, 233),
                bar_bg: Color::Rgb(46, 52, 64),
                overlay_fg: Color::Rgb(236, 239, 244),
                overlay_bg: Color::Rgb(59, 66, 82),
                border_fg: Color::Rgb(129, 161, 193),
                title_fg: Color::Rgb(136, 192, 208),
                track_fg: Color::Rgb(76, 86, 106),
                cwd_accent: (Color::Rgb(46, 52, 64), Color::Rgb(136, 192, 208)),
                git_accent: (Color::Rgb(46, 52, 64), Color::Rgb(163, 190, 140)),
                cmd_accent: (Color::Rgb(46, 52, 64), Color::Rgb(235, 203, 139)),
                metric_accent: (Color::Rgb(136, 192, 208), Color::Rgb(67, 76, 94)),
                battery_accent: (Color::Rgb(46, 52, 64), Color::Rgb(180, 142, 173)),
            },
            ThemePreset::Gruvbox => ThemePalette {
                bar_fg: Color::Rgb(235, 219, 178),
                bar_bg: Color::Rgb(40, 40, 40),
                overlay_fg: Color::Rgb(235, 219, 178),
                overlay_bg: Color::Rgb(60, 56, 54),
                border_fg: Color::Rgb(214, 93, 14),
                title_fg: Color::Rgb(250, 189, 47),
                track_fg: Color::Rgb(102, 92, 84),
                cwd_accent: (Color::Rgb(40, 40, 40), Color::Rgb(131, 165, 152)),
                git_accent: (Color::Rgb(40, 40, 40), Color::Rgb(184, 187, 38)),
                cmd_accent: (Color::Rgb(40, 40, 40), Color::Rgb(254, 128, 25)),
                metric_accent: (Color::Rgb(250, 189, 47), Color::Rgb(60, 56, 54)),
                battery_accent: (Color::Rgb(40, 40, 40), Color::Rgb(211, 134, 155)),
            },
        }
    }
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
    pub const fn new(fg: Color, bg: Color, bold: bool) -> Self {
        Self {
            fg,
            bg,
            bold,
            italic: false,
            underline: false,
            inverse: false,
        }
    }

    /// Constructs a style from 256-color ANSI foreground/background indices and bold weight.
    pub const fn ansi(fg_ansi: u8, bg_ansi: u8, bold: bool) -> Self {
        Self::new(Color::Ansi(fg_ansi), Color::Ansi(bg_ansi), bold)
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
