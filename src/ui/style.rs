use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Terminal dimensions in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

/// Terminal color representation supporting transparent/default terminal color,
/// 256-color ANSI index, 24-bit TrueColor RGB, and 32-bit RGBA with alpha blending.
///
/// In TOML/JSON configuration, `Color` can be written as:
/// - `"transparent"` / `"default"` / `"none"` (emits no background SGR so terminal emulator
///   window opacity such as Ghostty `background-opacity` shines through)
/// - An ANSI 256-color integer (`0..=255`, e.g. `fg = 231`)
/// - A 24-bit hex RGB string (`fg = "#89b4fa"`)
/// - A 32-bit hex RGBA string (`bg = "#24283b80"`) or `@<opacity>` suffix (`bg = "#24283b@0.75"`, `bg = "blue@50%"`)
/// - A semantic color name (`fg = "cyan"`, `"blue"`, `"green"`, `"magenta"`, `"yellow"`,
///   `"red"`, `"white"`, `"black"`, `"gray"`, `"dark_gray"`, `"orange"`, `"purple"`,
///   `"pink"`, `"teal"`, `"lavender"`, `"peach"`, `"sky"`, `"mauve"`, `"emerald"`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Ansi(u8),
    Rgb(u8, u8, u8),
    Rgba(u8, u8, u8, u8),
}

impl Color {
    /// Parses a string specification (`"transparent"`, `"#rrggbb"`, `"#rrggbbaa"`, `"#rrggbb@0.7"`, `"cyan"`, or `"231"`) into a `Color`.
    pub fn parse_str(s: &str) -> Result<Self, String> {
        let trimmed = s.trim();
        if let Some((base_str, alpha_str)) = trimmed.rsplit_once('@') {
            let base = Self::parse_base_str(base_str.trim())?;
            let alpha = Self::parse_alpha_spec(alpha_str.trim(), s)?;
            return Ok(base.with_alpha(alpha));
        }
        Self::parse_base_str(trimmed)
    }

    fn parse_alpha_spec(alpha_str: &str, full_input: &str) -> Result<u8, String> {
        if let Some(pct_str) = alpha_str.strip_suffix('%') {
            let pct: f64 = pct_str
                .trim()
                .parse()
                .map_err(|_| format!("invalid opacity percentage in '{full_input}'"))?;
            if !(0.0..=100.0).contains(&pct) {
                return Err(format!(
                    "opacity percentage in '{full_input}' must be between 0% and 100%"
                ));
            }
            return Ok(((pct / 100.0) * 255.0).round() as u8);
        }
        let factor: f64 = alpha_str.parse().map_err(|_| {
            format!("invalid opacity factor in '{full_input}' (expected 0.0..=1.0)")
        })?;
        if !(0.0..=1.0).contains(&factor) {
            return Err(format!(
                "opacity factor in '{full_input}' must be between 0.0 and 1.0"
            ));
        }
        Ok((factor * 255.0).round() as u8)
    }

    fn parse_base_str(trimmed: &str) -> Result<Self, String> {
        if let Some(hex) = trimmed.strip_prefix('#') {
            if hex.len() == 6 {
                let r = u8::from_str_radix(&hex[0..2], 16)
                    .map_err(|_| format!("invalid hex color '{trimmed}'"))?;
                let g = u8::from_str_radix(&hex[2..4], 16)
                    .map_err(|_| format!("invalid hex color '{trimmed}'"))?;
                let b = u8::from_str_radix(&hex[4..6], 16)
                    .map_err(|_| format!("invalid hex color '{trimmed}'"))?;
                return Ok(Color::Rgb(r, g, b));
            }
            if hex.len() == 8 {
                let r = u8::from_str_radix(&hex[0..2], 16)
                    .map_err(|_| format!("invalid hex color '{trimmed}'"))?;
                let g = u8::from_str_radix(&hex[2..4], 16)
                    .map_err(|_| format!("invalid hex color '{trimmed}'"))?;
                let b = u8::from_str_radix(&hex[4..6], 16)
                    .map_err(|_| format!("invalid hex color '{trimmed}'"))?;
                let a = u8::from_str_radix(&hex[6..8], 16)
                    .map_err(|_| format!("invalid hex alpha in '{trimmed}'"))?;
                return Ok(Color::Rgb(r, g, b).with_alpha(a));
            }
            return Err(format!(
                "hex color '{trimmed}' must be 6 (#rrggbb) or 8 (#rrggbbaa) hex digits"
            ));
        }

        if let Ok(idx) = trimmed.parse::<u8>() {
            return Ok(Color::Ansi(idx));
        }

        match trimmed.to_ascii_lowercase().as_str() {
            "transparent" | "default" | "none" | "reset" => Ok(Color::Default),
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
                "unknown color '{other}' (expected color name, 'transparent', '#rrggbb', '#rrggbbaa', or 0..255)"
            )),
        }
    }

    /// Applies an 8-bit alpha channel (`0..=255`) to this color.
    /// `0` maps to `Color::Default` (transparent), and `255` maps to opaque `Color::Rgb`.
    pub const fn with_alpha(self, a: u8) -> Self {
        if a == 0 {
            return Color::Default;
        }
        let (r, g, b) = self.to_rgb((26, 27, 38));
        if a == 255 {
            Color::Rgb(r, g, b)
        } else {
            Color::Rgba(r, g, b, a)
        }
    }

    /// Converts this color to an `(r, g, b)` tuple, using `fallback` when `self == Color::Default`.
    pub const fn to_rgb(self, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
        match self {
            Color::Default => fallback,
            Color::Rgb(r, g, b) | Color::Rgba(r, g, b, _) => (r, g, b),
            Color::Ansi(idx) => ansi_to_rgb(idx),
        }
    }

    /// Alpha-blends `self` over `underlying` when `self` is `Color::Rgba(r, g, b, a)`.
    /// If `underlying` is `Color::Default`, `base_bg` is used as the backdrop RGB for partial alpha.
    pub const fn blend_over(self, underlying: Color, base_bg: (u8, u8, u8)) -> Color {
        match self {
            Color::Rgba(sr, sg, sb, a) => {
                if a == 0 {
                    underlying
                } else if a == 255 {
                    Color::Rgb(sr, sg, sb)
                } else {
                    let (dr, dg, db) = underlying.to_rgb(base_bg);
                    let inv = (255 - a) as u16;
                    let alpha = a as u16;
                    let r = ((sr as u16 * alpha + dr as u16 * inv) / 255) as u8;
                    let g = ((sg as u16 * alpha + dg as u16 * inv) / 255) as u8;
                    let b = ((sb as u16 * alpha + db as u16 * inv) / 255) as u8;
                    Color::Rgb(r, g, b)
                }
            }
            other => other,
        }
    }
}

const fn ansi_to_rgb(idx: u8) -> (u8, u8, u8) {
    match idx {
        0 => (0, 0, 0),
        1 => (128, 0, 0),
        2 => (0, 128, 0),
        3 => (128, 128, 0),
        4 => (0, 0, 128),
        5 => (128, 0, 128),
        6 => (0, 128, 128),
        7 => (192, 192, 192),
        8 => (128, 128, 128),
        9 => (255, 0, 0),
        10 => (0, 255, 0),
        11 => (255, 255, 0),
        12 => (0, 0, 255),
        13 => (255, 0, 255),
        14 => (0, 255, 255),
        15 => (255, 255, 255),
        16..=231 => {
            let n = idx - 16;
            let r_idx = n / 36;
            let g_idx = (n % 36) / 6;
            let b_idx = n % 6;
            let r = if r_idx == 0 { 0 } else { 55 + r_idx * 40 };
            let g = if g_idx == 0 { 0 } else { 55 + g_idx * 40 };
            let b = if b_idx == 0 { 0 } else { 55 + b_idx * 40 };
            (r, g, b)
        }
        232..=255 => {
            let gray = 8 + (idx - 232) * 10;
            (gray, gray, gray)
        }
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match *self {
            Color::Default => serializer.serialize_str("transparent"),
            Color::Ansi(idx) => serializer.serialize_u8(idx),
            Color::Rgb(r, g, b) => serializer.serialize_str(&format!("#{r:02x}{g:02x}{b:02x}")),
            Color::Rgba(r, g, b, a) => {
                serializer.serialize_str(&format!("#{r:02x}{g:02x}{b:02x}{a:02x}"))
            }
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
                    "a color name (\"cyan\", \"transparent\"), hex RGB/RGBA (\"#rrggbb\", \"#rrggbbaa\", \"#rrggbb@0.7\"), or ANSI 256 index (0..=255)",
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
    #[serde(rename = "github_dark")]
    GitHubDark,
}

/// Resolved palette for a `ThemePreset` or customized `ThemeConfig`.
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
}

impl ThemePreset {
    pub const fn palette(self) -> ThemePalette {
        match self {
            ThemePreset::Default => ThemePalette {
                bar_fg: Color::Ansi(255),
                bar_bg: Color::Default,
                overlay_fg: Color::Ansi(231),
                overlay_bg: Color::Ansi(238),
                border_fg: Color::Ansi(245),
                title_fg: Color::Ansi(231),
                track_fg: Color::Ansi(242),
                cwd_accent: (Color::Ansi(255), Color::Ansi(236)),
                git_accent: (Color::Ansi(255), Color::Ansi(236)),
                cmd_accent: (Color::Ansi(255), Color::Ansi(236)),
                metric_accent: (Color::Ansi(255), Color::Ansi(236)),
            },
            ThemePreset::TokyoNight => ThemePalette {
                bar_fg: Color::Rgb(192, 202, 245),
                bar_bg: Color::Default,
                overlay_fg: Color::Rgb(192, 202, 245),
                overlay_bg: Color::Rgb(36, 40, 59),
                border_fg: Color::Rgb(122, 162, 247),
                title_fg: Color::Rgb(125, 207, 255),
                track_fg: Color::Rgb(65, 72, 104),
                cwd_accent: (Color::Rgb(26, 27, 38), Color::Rgb(122, 162, 247)),
                git_accent: (Color::Rgb(26, 27, 38), Color::Rgb(187, 154, 247)),
                cmd_accent: (Color::Rgb(26, 27, 38), Color::Rgb(255, 158, 100)),
                metric_accent: (Color::Rgb(125, 207, 255), Color::Rgb(36, 40, 59)),
            },
            ThemePreset::Catppuccin => ThemePalette {
                bar_fg: Color::Rgb(205, 214, 244),
                bar_bg: Color::Default,
                overlay_fg: Color::Rgb(205, 214, 244),
                overlay_bg: Color::Rgb(30, 30, 46),
                border_fg: Color::Rgb(180, 190, 254),
                title_fg: Color::Rgb(203, 166, 247),
                track_fg: Color::Rgb(88, 91, 112),
                cwd_accent: (Color::Rgb(17, 17, 27), Color::Rgb(137, 180, 250)),
                git_accent: (Color::Rgb(17, 17, 27), Color::Rgb(166, 227, 161)),
                cmd_accent: (Color::Rgb(17, 17, 27), Color::Rgb(250, 179, 135)),
                metric_accent: (Color::Rgb(148, 226, 213), Color::Rgb(49, 50, 68)),
            },
            ThemePreset::Cyberpunk => ThemePalette {
                bar_fg: Color::Rgb(0, 240, 255),
                bar_bg: Color::Default,
                overlay_fg: Color::Rgb(252, 238, 10),
                overlay_bg: Color::Rgb(22, 6, 54),
                border_fg: Color::Rgb(255, 0, 127),
                title_fg: Color::Rgb(0, 240, 255),
                track_fg: Color::Rgb(74, 21, 112),
                cwd_accent: (Color::Rgb(13, 2, 33), Color::Rgb(0, 240, 255)),
                git_accent: (Color::Rgb(255, 255, 255), Color::Rgb(255, 0, 127)),
                cmd_accent: (Color::Rgb(13, 2, 33), Color::Rgb(252, 238, 10)),
                metric_accent: (Color::Rgb(0, 255, 102), Color::Rgb(22, 6, 54)),
            },
            ThemePreset::Nord => ThemePalette {
                bar_fg: Color::Rgb(216, 222, 233),
                bar_bg: Color::Default,
                overlay_fg: Color::Rgb(236, 239, 244),
                overlay_bg: Color::Rgb(59, 66, 82),
                border_fg: Color::Rgb(129, 161, 193),
                title_fg: Color::Rgb(136, 192, 208),
                track_fg: Color::Rgb(76, 86, 106),
                cwd_accent: (Color::Rgb(46, 52, 64), Color::Rgb(136, 192, 208)),
                git_accent: (Color::Rgb(46, 52, 64), Color::Rgb(163, 190, 140)),
                cmd_accent: (Color::Rgb(46, 52, 64), Color::Rgb(235, 203, 139)),
                metric_accent: (Color::Rgb(136, 192, 208), Color::Rgb(67, 76, 94)),
            },
            ThemePreset::Gruvbox => ThemePalette {
                bar_fg: Color::Rgb(235, 219, 178),
                bar_bg: Color::Default,
                overlay_fg: Color::Rgb(235, 219, 178),
                overlay_bg: Color::Rgb(60, 56, 54),
                border_fg: Color::Rgb(214, 93, 14),
                title_fg: Color::Rgb(250, 189, 47),
                track_fg: Color::Rgb(102, 92, 84),
                cwd_accent: (Color::Rgb(40, 40, 40), Color::Rgb(131, 165, 152)),
                git_accent: (Color::Rgb(40, 40, 40), Color::Rgb(184, 187, 38)),
                cmd_accent: (Color::Rgb(40, 40, 40), Color::Rgb(254, 128, 25)),
                metric_accent: (Color::Rgb(250, 189, 47), Color::Rgb(60, 56, 54)),
            },
            ThemePreset::GitHubDark => ThemePalette {
                bar_fg: Color::Rgb(201, 209, 217),
                bar_bg: Color::Default,
                overlay_fg: Color::Rgb(201, 209, 217),
                overlay_bg: Color::Rgb(22, 27, 34),
                border_fg: Color::Rgb(48, 54, 61),
                title_fg: Color::Rgb(88, 166, 255),
                track_fg: Color::Rgb(72, 79, 88),
                cwd_accent: (Color::Rgb(13, 17, 23), Color::Rgb(88, 166, 255)),
                git_accent: (Color::Rgb(13, 17, 23), Color::Rgb(63, 185, 80)),
                cmd_accent: (Color::Rgb(13, 17, 23), Color::Rgb(240, 136, 62)),
                metric_accent: (Color::Rgb(188, 140, 255), Color::Rgb(22, 27, 34)),
            },
        }
    }
}

/// Customizable theme configuration that accepts either a preset name (`theme = "tokyo_night"`)
/// or a `[theme]` table (`preset = "tokyo_night"` plus individual color overrides).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "ThemeConfigRepr", into = "ThemeConfigRepr")]
pub struct ThemeConfig {
    pub preset: ThemePreset,
    pub bar_fg: Option<Color>,
    pub bar_bg: Option<Color>,
    pub overlay_fg: Option<Color>,
    pub overlay_bg: Option<Color>,
    pub border_fg: Option<Color>,
    pub title_fg: Option<Color>,
    pub track_fg: Option<Color>,
    pub cwd_fg: Option<Color>,
    pub cwd_bg: Option<Color>,
    pub git_fg: Option<Color>,
    pub git_bg: Option<Color>,
    pub cmd_fg: Option<Color>,
    pub cmd_bg: Option<Color>,
    pub metric_fg: Option<Color>,
    pub metric_bg: Option<Color>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
struct ThemeTableRepr {
    preset: ThemePreset,
    #[serde(skip_serializing_if = "Option::is_none")]
    bar_fg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bar_bg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    overlay_fg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    overlay_bg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    border_fg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title_fg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    track_fg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd_fg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd_bg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_fg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_bg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cmd_fg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cmd_bg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metric_fg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metric_bg: Option<Color>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(untagged)]
enum ThemeConfigRepr {
    Preset(ThemePreset),
    Table(ThemeTableRepr),
}

impl From<ThemePreset> for ThemeConfig {
    fn from(preset: ThemePreset) -> Self {
        Self::from_preset(preset)
    }
}

impl From<ThemeConfigRepr> for ThemeConfig {
    fn from(repr: ThemeConfigRepr) -> Self {
        match repr {
            ThemeConfigRepr::Preset(preset) => Self::from_preset(preset),
            ThemeConfigRepr::Table(t) => Self {
                preset: t.preset,
                bar_fg: t.bar_fg,
                bar_bg: t.bar_bg,
                overlay_fg: t.overlay_fg,
                overlay_bg: t.overlay_bg,
                border_fg: t.border_fg,
                title_fg: t.title_fg,
                track_fg: t.track_fg,
                cwd_fg: t.cwd_fg,
                cwd_bg: t.cwd_bg,
                git_fg: t.git_fg,
                git_bg: t.git_bg,
                cmd_fg: t.cmd_fg,
                cmd_bg: t.cmd_bg,
                metric_fg: t.metric_fg,
                metric_bg: t.metric_bg,
            },
        }
    }
}

impl From<ThemeConfig> for ThemeConfigRepr {
    fn from(cfg: ThemeConfig) -> Self {
        if !cfg.has_overrides() {
            ThemeConfigRepr::Preset(cfg.preset)
        } else {
            ThemeConfigRepr::Table(ThemeTableRepr {
                preset: cfg.preset,
                bar_fg: cfg.bar_fg,
                bar_bg: cfg.bar_bg,
                overlay_fg: cfg.overlay_fg,
                overlay_bg: cfg.overlay_bg,
                border_fg: cfg.border_fg,
                title_fg: cfg.title_fg,
                track_fg: cfg.track_fg,
                cwd_fg: cfg.cwd_fg,
                cwd_bg: cfg.cwd_bg,
                git_fg: cfg.git_fg,
                git_bg: cfg.git_bg,
                cmd_fg: cfg.cmd_fg,
                cmd_bg: cfg.cmd_bg,
                metric_fg: cfg.metric_fg,
                metric_bg: cfg.metric_bg,
            })
        }
    }
}

impl ThemeConfig {
    pub const fn from_preset(preset: ThemePreset) -> Self {
        Self {
            preset,
            bar_fg: None,
            bar_bg: None,
            overlay_fg: None,
            overlay_bg: None,
            border_fg: None,
            title_fg: None,
            track_fg: None,
            cwd_fg: None,
            cwd_bg: None,
            git_fg: None,
            git_bg: None,
            cmd_fg: None,
            cmd_bg: None,
            metric_fg: None,
            metric_bg: None,
        }
    }

    pub const fn has_overrides(&self) -> bool {
        self.bar_fg.is_some()
            || self.bar_bg.is_some()
            || self.overlay_fg.is_some()
            || self.overlay_bg.is_some()
            || self.border_fg.is_some()
            || self.title_fg.is_some()
            || self.track_fg.is_some()
            || self.cwd_fg.is_some()
            || self.cwd_bg.is_some()
            || self.git_fg.is_some()
            || self.git_bg.is_some()
            || self.cmd_fg.is_some()
            || self.cmd_bg.is_some()
            || self.metric_fg.is_some()
            || self.metric_bg.is_some()
    }

    pub const fn is_unthemed_default(&self) -> bool {
        matches!(self.preset, ThemePreset::Default) && !self.has_overrides()
    }

    /// Resolves the final `ThemePalette` by applying any user overrides on top of `self.preset.palette()`.
    pub const fn palette(&self) -> ThemePalette {
        let mut p = self.preset.palette();
        if let Some(c) = self.bar_fg {
            p.bar_fg = c;
        }
        if let Some(c) = self.bar_bg {
            p.bar_bg = c;
        }
        if let Some(c) = self.overlay_fg {
            p.overlay_fg = c;
        }
        if let Some(c) = self.overlay_bg {
            p.overlay_bg = c;
        }
        if let Some(c) = self.border_fg {
            p.border_fg = c;
        }
        if let Some(c) = self.title_fg {
            p.title_fg = c;
        }
        if let Some(c) = self.track_fg {
            p.track_fg = c;
        }
        if let Some(c) = self.cwd_fg {
            p.cwd_accent.0 = c;
        }
        if let Some(c) = self.cwd_bg {
            p.cwd_accent.1 = c;
        }
        if let Some(c) = self.git_fg {
            p.git_accent.0 = c;
        }
        if let Some(c) = self.git_bg {
            p.git_accent.1 = c;
        }
        if let Some(c) = self.cmd_fg {
            p.cmd_accent.0 = c;
        }
        if let Some(c) = self.cmd_bg {
            p.cmd_accent.1 = c;
        }
        if let Some(c) = self.metric_fg {
            p.metric_accent.0 = c;
        }
        if let Some(c) = self.metric_bg {
            p.metric_accent.1 = c;
        }
        p
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
