pub mod ipc;
pub mod template;

pub use crate::ui::{Anchor, BorderStyle, CellStyle, Color, TermSize, ThemePalette, ThemePreset};
pub use ipc::{IpcMessage, ToastLevel};

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Policy when the child terminal's cursor or text collides with a floating overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum CollisionPolicy {
    /// Always render the overlay at its anchor regardless of cursor position or underlying text.
    Ignore,
    /// Temporarily hide the overlay while the child cursor is within `margin` cells
    /// (or while underlying cells contain non-space text if `avoid_text` is true).
    AutoHide {
        #[serde(default = "default_collision_margin")]
        margin: u16,
        #[serde(default = "default_true")]
        avoid_text: bool,
    },
    /// Move the overlay to an unoccupied position when the child cursor is within `margin` cells
    /// (or when underlying cells contain non-space text if `avoid_text` is true).
    DodgeToOppositeCorner {
        #[serde(default = "default_collision_margin")]
        margin: u16,
        #[serde(default = "default_true")]
        avoid_text: bool,
    },
}

fn default_collision_margin() -> u16 {
    1
}

impl Default for CollisionPolicy {
    fn default() -> Self {
        CollisionPolicy::DodgeToOppositeCorner {
            margin: 1,
            avoid_text: true,
        }
    }
}

/// Which terminal screen buffer mode an overlay should appear in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenCondition {
    #[default]
    Always,
    PrimaryOnly,
    AlternateOnly,
    /// Visible only when the user toggles the HUD Dashboard mode via hotkey.
    DashboardOnly,
}

/// Per-widget segment styling, icon, capsule (`pill` / `padding`), and affix options.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SegmentFormat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pill: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_fg: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
}

/// Declarative widget data source specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WidgetKind {
    /// Local wall-clock time formatted with `chrono` strftime syntax.
    Clock {
        #[serde(default = "default_clock_format")]
        format: String,
    },
    /// Current working directory of the child shell.
    Cwd,
    /// Current Git branch in the working directory.
    GitBranch,
    /// System battery percentage / status.
    Battery,
    /// Foreground process telemetry (e.g. `⚙ cargo (12s)` when running >= `min_secs`).
    ActiveCommand {
        #[serde(default)]
        min_secs: u64,
    },
    /// Static text label.
    Text { text: String },
    /// Periodic shell command executed via `sh -c`.
    Command {
        id: String,
        cmd: String,
        #[serde(default = "default_command_interval_ms")]
        interval_ms: u64,
    },
    /// Named slot populated dynamically via IPC socket or builtin state map.
    IpcSlot { key: String },
    /// Historical sparkline (`▁▂▃▄▅▆▇█`) for a numeric metric slot (`0..=100`).
    Sparkline {
        key: String,
        #[serde(default = "default_sparkline_width")]
        width: usize,
    },
    /// Horizontal progress/metric gauge (`━━━─── 60%`) for a numeric slot (`0..=100`).
    Gauge {
        key: String,
        #[serde(default = "default_gauge_width")]
        width: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

/// A widget specification combining a `WidgetKind` with optional per-segment styling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetSpec {
    #[serde(flatten)]
    pub kind: WidgetKind,
    #[serde(flatten)]
    pub style: SegmentFormat,
}

impl WidgetSpec {
    pub fn new(kind: WidgetKind) -> Self {
        Self {
            kind,
            style: SegmentFormat::default(),
        }
    }
}

impl From<WidgetKind> for WidgetSpec {
    fn from(kind: WidgetKind) -> Self {
        Self::new(kind)
    }
}

fn default_clock_format() -> String {
    "%H:%M:%S".to_string()
}

fn default_command_interval_ms() -> u64 {
    5000
}

fn default_sparkline_width() -> usize {
    8
}

fn default_gauge_width() -> usize {
    10
}

/// Docked bottom status line configuration (Vim-like status bar).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusBarConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_status_height")]
    pub height: u16,
    #[serde(default = "default_separator")]
    pub separator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<Color>,
    #[serde(default)]
    pub left: Vec<WidgetSpec>,
    #[serde(default)]
    pub center: Vec<WidgetSpec>,
    #[serde(default)]
    pub right: Vec<WidgetSpec>,
}

fn default_true() -> bool {
    true
}

fn default_status_height() -> u16 {
    1
}

fn default_separator() -> String {
    " │ ".to_string()
}

impl Default for StatusBarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            height: 1,
            separator: default_separator(),
            fg: None,
            bg: None,
            left: vec![
                WidgetSpec::new(WidgetKind::Cwd),
                WidgetSpec::new(WidgetKind::GitBranch),
                WidgetSpec::new(WidgetKind::ActiveCommand { min_secs: 2 }),
                WidgetSpec::new(WidgetKind::IpcSlot {
                    key: "status".to_string(),
                }),
            ],
            center: vec![],
            right: vec![WidgetSpec::new(WidgetKind::Battery)],
        }
    }
}

/// Floating non-destructive overlay configuration (e.g., top-right clock or multi-widget bordered HUD panel).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayConfig {
    pub id: String,
    #[serde(default)]
    pub anchor: Anchor,
    #[serde(default)]
    pub offset_x: i16,
    #[serde(default)]
    pub offset_y: i16,
    #[serde(default)]
    pub z_index: i32,
    #[serde(default)]
    pub screen_mode: ScreenCondition,
    #[serde(default)]
    pub collision: CollisionPolicy,
    #[serde(default)]
    pub border: BorderStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border_fg: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_fg: Option<Color>,
    #[serde(default)]
    pub padding_x: u16,
    #[serde(default = "default_true")]
    pub bold: bool,
    pub widgets: Vec<WidgetSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
}

/// Top-level declarative configuration for a `thud` session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub theme: ThemePreset,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub ipc_socket: Option<PathBuf>,
    #[serde(default = "default_zen_toggle_key")]
    pub zen_toggle_key: Option<String>,
    #[serde(default = "default_dashboard_toggle_key")]
    pub dashboard_toggle_key: Option<String>,
    #[serde(default)]
    pub status_bar: StatusBarConfig,
    #[serde(default = "default_overlays")]
    pub overlays: Vec<OverlayConfig>,
}

fn default_zen_toggle_key() -> Option<String> {
    Some("ctrl-\\".to_string())
}

fn default_dashboard_toggle_key() -> Option<String> {
    Some("ctrl-]".to_string())
}

fn default_overlays() -> Vec<OverlayConfig> {
    vec![OverlayConfig {
        id: "clock".to_string(),
        anchor: Anchor::TopRight,
        offset_x: 0,
        offset_y: 0,
        z_index: 10,
        screen_mode: ScreenCondition::Always,
        collision: CollisionPolicy::DodgeToOppositeCorner {
            margin: 1,
            avoid_text: true,
        },
        border: BorderStyle::None,
        title: None,
        fg: None,
        bg: None,
        border_fg: None,
        title_fg: None,
        padding_x: 0,
        bold: true,
        widgets: vec![WidgetSpec::new(WidgetKind::Clock {
            format: " %H:%M:%S ".to_string(),
        })],
        ttl_ms: None,
    }]
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: ThemePreset::Default,
            shell: None,
            ipc_socket: None,
            zen_toggle_key: default_zen_toggle_key(),
            dashboard_toggle_key: default_dashboard_toggle_key(),
            status_bar: StatusBarConfig::default(),
            overlays: default_overlays(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    Io(String),
    TomlParse(String),
    InvalidGeometry(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(msg) => write!(f, "configuration I/O error: {msg}"),
            ConfigError::TomlParse(msg) => write!(f, "invalid TOML configuration: {msg}"),
            ConfigError::InvalidGeometry(msg) => write!(f, "invalid geometry: {msg}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Parses configuration from a TOML string.
    pub fn from_toml(toml_str: &str) -> Result<Self, ConfigError> {
        toml::from_str(toml_str).map_err(|e| ConfigError::TomlParse(e.to_string()))
    }

    /// Loads configuration from an explicit file path.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let content =
            fs::read_to_string(path.as_ref()).map_err(|e| ConfigError::Io(e.to_string()))?;
        Self::from_toml(&content)
    }

    /// Returns the default configuration file path (`$THUD_CONFIG` if set, otherwise `~/.config/thud/config.toml`).
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(custom_path) = std::env::var("THUD_CONFIG")
            && !custom_path.is_empty()
        {
            return Some(PathBuf::from(custom_path));
        }
        std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join(".config/thud/config.toml"))
    }

    /// Loads configuration from `$THUD_CONFIG` (if set) or `~/.config/thud/config.toml` if present,
    /// falling back to `Config::default()`.
    pub fn load_or_default() -> Result<Self, ConfigError> {
        if let Ok(custom_path) = std::env::var("THUD_CONFIG")
            && !custom_path.is_empty()
        {
            let p = PathBuf::from(custom_path);
            if p.exists() {
                return Self::from_file(p);
            }
            return Ok(Self::default());
        }
        if let Some(path) = Self::default_path()
            && path.exists()
        {
            return Self::from_file(path);
        }
        Ok(Self::default())
    }

    /// Writes the annotated default configuration TOML to `target_path` (or `Self::default_path()`).
    /// Fails if the file already exists unless `force` is `true`.
    pub fn write_default_file(
        target_path: Option<PathBuf>,
        force: bool,
    ) -> Result<PathBuf, ConfigError> {
        let path = target_path.or_else(Self::default_path).ok_or_else(|| {
            ConfigError::Io(
                "could not determine default config path ($HOME is not set)".to_string(),
            )
        })?;

        if path.exists() && !force {
            return Err(ConfigError::Io(format!(
                "config file already exists at '{}' (use --force to overwrite)",
                path.display()
            )));
        }

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|e| ConfigError::Io(e.to_string()))?;
        }

        fs::write(&path, Self::annotated_default_toml())
            .map_err(|e| ConfigError::Io(e.to_string()))?;
        Ok(path)
    }

    /// Parse a hotkey string like `"ctrl-\\"` (`0x1c`), `"ctrl-]"` (`0x1d`), or `"ctrl-g"` (`0x07`)
    /// into its single raw control byte.
    pub fn parse_hotkey_byte(spec: Option<&str>) -> Option<u8> {
        let s = spec?.trim().to_ascii_lowercase();
        let rest = s.strip_prefix("ctrl-")?;
        match rest {
            "\\" => Some(0x1c),
            "]" => Some(0x1d),
            "^" => Some(0x1e),
            "_" => Some(0x1f),
            "space" | "@" => Some(0x00),
            c if c.len() == 1 => {
                let b = c.as_bytes()[0];
                if b.is_ascii_lowercase() {
                    Some(b - b'a' + 1)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Reserved bottom rows for the docked status bar.
    pub fn reserved_bottom_rows(&self) -> u16 {
        if self.status_bar.enabled {
            self.status_bar.height
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_includes_bottom_status_bar_and_top_right_clock() {
        let cfg = Config::default();
        assert_eq!(cfg.reserved_bottom_rows(), 1);
        assert_eq!(cfg.overlays.len(), 1);
        assert_eq!(cfg.overlays[0].id, "clock");
        assert_eq!(cfg.overlays[0].anchor, Anchor::TopRight);
        assert_eq!(
            cfg.overlays[0].collision,
            CollisionPolicy::DodgeToOppositeCorner {
                margin: 1,
                avoid_text: true
            }
        );
    }

    #[test]
    fn parses_custom_toml_with_themes_colors_multi_widget_overlays_and_pills() {
        let toml_input = r##"
            theme = "tokyo_night"
            shell = "/bin/zsh"
            zen_toggle_key = "ctrl-g"

            [status_bar]
            enabled = true
            height = 1
            separator = " "
            fg = "#c0caf5"
            bg = "#1a1b26"
            left = [
                { type = "cwd", fg = "black", bg = "cyan", bold = true, icon = "📁", padding = true },
                { type = "git_branch", fg = 232, bg = "#a6e3a1", pill = true, icon = "🌿" },
                { type = "sparkline", key = "cpu", width = 6 }
            ]
            right = [
                { type = "gauge", key = "build", width = 8, label = "Build", track_fg = "dark_gray" },
                { type = "battery", padding = true }
            ]

            [[overlays]]
            id = "top_panel"
            anchor = "top_right"
            border = "rounded"
            title = " HUD "
            border_fg = "cyan"
            title_fg = "#f9e2af"
            padding_x = 1
            collision = { mode = "auto_hide", margin = 2 }
            widgets = [
                { type = "clock", format = "%H:%M" },
                { type = "sparkline", key = "cpu", width = 6 }
            ]
        "##;

        let cfg = Config::from_toml(toml_input).expect("valid TOML");
        assert_eq!(cfg.theme, ThemePreset::TokyoNight);
        assert_eq!(cfg.shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(
            Config::parse_hotkey_byte(cfg.zen_toggle_key.as_deref()),
            Some(0x07)
        );
        assert_eq!(cfg.status_bar.left.len(), 3);
        assert_eq!(
            cfg.status_bar.left[0].style.bg,
            Some(Color::Rgb(137, 220, 235))
        );
        assert_eq!(cfg.status_bar.left[0].style.icon.as_deref(), Some("📁"));
        assert_eq!(cfg.status_bar.left[1].style.pill, Some(true));
        assert_eq!(cfg.overlays[0].border, BorderStyle::Rounded);
        assert_eq!(cfg.overlays[0].title.as_deref(), Some(" HUD "));
        assert_eq!(cfg.overlays[0].padding_x, 1);
        assert_eq!(cfg.overlays[0].widgets.len(), 2);

        // Verify dynamic `to_annotated_toml()` round-trips customized Config!
        let regenerated = cfg.to_annotated_toml();
        let reparsed = Config::from_toml(&regenerated).expect("regenerated TOML must be valid");
        assert_eq!(reparsed, cfg);
    }

    #[test]
    fn parses_ipc_json_messages() {
        let json = r#"{"action":"set_slot","key":"build","value":"PASS","ttl_ms":3000}"#;
        let msg: IpcMessage = serde_json::from_str(json).expect("valid IPC JSON");
        assert_eq!(
            msg,
            IpcMessage::SetSlot {
                key: "build".to_string(),
                value: "PASS".to_string(),
                ttl_ms: Some(3000),
            }
        );
    }

    #[test]
    fn annotated_default_toml_parses_to_exact_default_config() {
        let generated = Config::annotated_default_toml();
        let parsed = Config::from_toml(&generated).expect("annotated_default_toml must be valid");
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn write_default_file_creates_and_protects_existing_file_unless_forced() {
        let temp_path =
            std::env::temp_dir().join(format!("thud-config-test-{}.toml", std::process::id()));
        let _ = fs::remove_file(&temp_path);

        let written = Config::write_default_file(Some(temp_path.clone()), false).unwrap();
        assert_eq!(written, temp_path);
        assert_eq!(Config::from_file(&temp_path).unwrap(), Config::default());

        // Second write without `force` must fail
        assert!(Config::write_default_file(Some(temp_path.clone()), false).is_err());

        // Second write with `force = true` must succeed
        assert!(Config::write_default_file(Some(temp_path.clone()), true).is_ok());

        let _ = fs::remove_file(&temp_path);
    }
}
