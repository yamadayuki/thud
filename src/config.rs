use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

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
    /// Returns the horizontally opposite anchor used when dodging the cursor.
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
}

/// Declarative widget data source specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WidgetSpec {
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
}

fn default_clock_format() -> String {
    "%H:%M:%S".to_string()
}

fn default_command_interval_ms() -> u64 {
    5000
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
    #[serde(default = "default_status_fg")]
    pub fg_ansi: u8,
    #[serde(default = "default_status_bg")]
    pub bg_ansi: u8,
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

fn default_status_fg() -> u8 {
    255 // Bright white in 256-color palette
}

fn default_status_bg() -> u8 {
    236 // Dark charcoal gray in 256-color palette
}

impl Default for StatusBarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            height: 1,
            separator: default_separator(),
            fg_ansi: default_status_fg(),
            bg_ansi: default_status_bg(),
            left: vec![
                WidgetSpec::Cwd,
                WidgetSpec::GitBranch,
                WidgetSpec::IpcSlot {
                    key: "status".to_string(),
                },
            ],
            center: vec![],
            right: vec![WidgetSpec::Battery],
        }
    }
}

/// Floating non-destructive overlay configuration (e.g., top-right clock).
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
    #[serde(default = "default_overlay_fg")]
    pub fg_ansi: u8,
    #[serde(default = "default_overlay_bg")]
    pub bg_ansi: u8,
    #[serde(default = "default_true")]
    pub bold: bool,
    pub widget: WidgetSpec,
    #[serde(default)]
    pub ttl_ms: Option<u64>,
}

fn default_overlay_fg() -> u8 {
    231
}

fn default_overlay_bg() -> u8 {
    238
}

/// Top-level declarative configuration for a `cth` session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub ipc_socket: Option<PathBuf>,
    #[serde(default)]
    pub status_bar: StatusBarConfig,
    #[serde(default = "default_overlays")]
    pub overlays: Vec<OverlayConfig>,
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
        fg_ansi: default_overlay_fg(),
        bg_ansi: default_overlay_bg(),
        bold: true,
        widget: WidgetSpec::Clock {
            format: " %H:%M:%S ".to_string(),
        },
        ttl_ms: None,
    }]
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shell: None,
            ipc_socket: None,
            status_bar: StatusBarConfig::default(),
            overlays: default_overlays(),
        }
    }
}

/// Dynamic IPC payload sent over the Unix domain socket in JSON format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum IpcMessage {
    /// Set or update a named IPC slot (`{ "action": "set_slot", "key": "mode", "value": "NORMAL" }`).
    SetSlot {
        key: String,
        value: String,
        #[serde(default)]
        ttl_ms: Option<u64>,
    },
    /// Clear a named IPC slot.
    ClearSlot { key: String },
    /// Dynamically add or update a floating overlay (e.g., toast notification).
    UpsertOverlay { overlay: OverlayConfig },
    /// Remove a floating overlay by ID.
    RemoveOverlay { id: String },
    /// Gracefully terminate the active `cth` session.
    Quit,
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

    /// Returns the default configuration file path (`$CTH_CONFIG` if set, otherwise `~/.config/cth/config.toml`).
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(custom_path) = std::env::var("CTH_CONFIG")
            && !custom_path.is_empty()
        {
            return Some(PathBuf::from(custom_path));
        }
        std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join(".config/cth/config.toml"))
    }

    /// Loads configuration from `$CTH_CONFIG` or `~/.config/cth/config.toml` if present,
    /// falling back to `Config::default()`.
    pub fn load_or_default() -> Result<Self, ConfigError> {
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
            ConfigError::Io("could not determine default config path ($HOME is not set)".to_string())
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

    /// Returns a self-documenting TOML configuration string containing all default values,
    /// available options, widget types, and CLI/IPC usage examples.
    pub fn annotated_default_toml() -> &'static str {
        r#"# =============================================================================
# cth - Terminal HUD & Compositor Configuration
# Default path: ~/.config/cth/config.toml (or override via $CTH_CONFIG / --config)
# =============================================================================

# Optional: Shell executable to spawn inside the PTY.
# Defaults to $SHELL (or "/bin/sh" if $SHELL is unset).
# shell = "/opt/homebrew/bin/fish"

# Optional: Custom Unix domain socket path for IPC commands (`cth set`, `cth toast`, etc.).
# Defaults to "/tmp/cth-<pid>.sock" and is exported as $CTH_SOCKET inside the session.
# ipc_socket = "/tmp/cth.sock"

# -----------------------------------------------------------------------------
# Docked Bottom Status Bar (Vim-like status line)
# Reserves `height` row(s) at the bottom of the terminal so child applications
# (including fullscreen TUI apps like vim, htop, fzf, less) never overwrite it.
# -----------------------------------------------------------------------------
[status_bar]
enabled = true
height = 1
separator = " │ "
# 256-color ANSI palette index (0-255): 255 = bright white, 236 = dark charcoal
fg_ansi = 255
bg_ansi = 236

# Widgets placed on the left side of the status bar.
# Available widget types (`type = ...`):
#   1. { type = "cwd" }
#      -> Current working directory of the foreground process (e.g. "~/dev/cth")
#   2. { type = "git_branch" }
#      -> Current Git branch or short commit SHA (e.g. "git:main")
#   3. { type = "battery" }
#      -> System battery level (e.g. "BAT 100%")
#   4. { type = "clock", format = "%Y-%m-%d %H:%M:%S" }
#      -> Local time formatted with strftime syntax (default: "%H:%M:%S")
#   5. { type = "text", text = "PROD" }
#      -> Static label string
#   6. { type = "command", id = "k8s", cmd = "kubectl config current-context", interval_ms = 5000 }
#      -> Periodic non-blocking `sh -c` command output refreshed every `interval_ms`
#   7. { type = "ipc_slot", key = "status" }
#      -> Dynamic slot updated from shell hooks or external scripts via:
#         `cth set status "BUILDING..." --ttl-ms 5000`
#         `cth clear status`
left = [
    { type = "cwd" },
    { type = "git_branch" },
    { type = "ipc_slot", key = "status" },
]

# Widgets placed in the center of the status bar.
center = []

# Widgets placed on the right side of the status bar.
right = [
    { type = "battery" },
]

# -----------------------------------------------------------------------------
# Floating Overlays (Non-destructive HUD layers composited over the terminal)
# Overlays do not shrink the child PTY or mutate its screen buffer; when moved
# or hidden, the underlying terminal characters (including wide CJK chars) are
# restored automatically.
# -----------------------------------------------------------------------------
[[overlays]]
id = "clock"

# Anchor position on the child terminal viewport:
#   "top_right" | "top_left" | "top_center" | "bottom_right" | "bottom_left" | "bottom_center"
anchor = "top_right"

# Fine-grained cell offset from the anchor position.
offset_x = 0
offset_y = 0

# Drawing order when multiple overlays exist (higher z_index renders on top).
z_index = 10

# Which screen buffer mode this overlay is visible in:
#   "always"         -> Both normal shell and fullscreen TUI apps (vim, htop, less)
#   "primary_only"   -> Only normal shell screen; hidden during fullscreen TUI apps
#   "alternate_only" -> Only during fullscreen TUI apps
screen_mode = "always"

# 256-color ANSI palette index (0-255) and text weight
fg_ansi = 231
bg_ansi = 238
bold = true

# Optional: Time-to-live in milliseconds (mainly used for temporary IPC overlays/toasts).
# ttl_ms = 4000

# Behavior when the child cursor or terminal text reaches the overlay region:
#   1. { mode = "dodge_to_opposite_corner", margin = 1, avoid_text = true }
#      -> Moves to an unoccupied corner or slides down along the edge when the cursor
#         approaches within `margin` cells or when underlying text is present.
#   2. { mode = "auto_hide", margin = 1, avoid_text = true }
#      -> Temporarily hides while the cursor or text overlaps the overlay.
#   3. { mode = "ignore" }
#      -> Always renders at the fixed anchor position.
[overlays.collision]
mode = "dodge_to_opposite_corner"
margin = 1
avoid_text = true

[overlays.widget]
type = "clock"
format = " %H:%M:%S "
"#
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
    fn parses_custom_toml_with_builtins_commands_and_ipc_slots() {
        let toml_input = r#"
            shell = "/bin/zsh"

            [status_bar]
            enabled = true
            height = 1
            separator = " | "
            left = [
                { type = "cwd" },
                { type = "git_branch" },
                { type = "ipc_slot", key = "vim_mode" }
            ]
            right = [
                { type = "command", id = "k8s", cmd = "kubectl config current-context", interval_ms = 10000 },
                { type = "battery" }
            ]

            [[overlays]]
            id = "top_clock"
            anchor = "top_right"
            collision = { mode = "auto_hide", margin = 2 }
            widget = { type = "clock", format = "%H:%M" }
        "#;

        let cfg = Config::from_toml(toml_input).expect("valid TOML");
        assert_eq!(cfg.shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(cfg.status_bar.left.len(), 3);
        assert_eq!(cfg.status_bar.right.len(), 2);
        assert_eq!(cfg.overlays.len(), 1);
        assert_eq!(
            cfg.overlays[0].collision,
            CollisionPolicy::AutoHide {
                margin: 2,
                avoid_text: true
            }
        );
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
        let parsed = Config::from_toml(Config::annotated_default_toml())
            .expect("annotated_default_toml must be valid TOML");
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn write_default_file_creates_and_protects_existing_file_unless_forced() {
        let temp_path =
            std::env::temp_dir().join(format!("cth-config-test-{}.toml", std::process::id()));
        let _ = fs::remove_file(&temp_path);

        let written = Config::write_default_file(Some(temp_path.clone()), false).unwrap();
        assert_eq!(written, temp_path);
        assert_eq!(
            Config::from_file(&temp_path).unwrap(),
            Config::default()
        );

        // Second write without `force` must fail
        assert!(Config::write_default_file(Some(temp_path.clone()), false).is_err());

        // Second write with `force = true` must succeed
        assert!(Config::write_default_file(Some(temp_path.clone()), true).is_ok());

        let _ = fs::remove_file(&temp_path);
    }
}

