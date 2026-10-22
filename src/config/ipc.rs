use crate::config::OverlayConfig;
use serde::{Deserialize, Serialize};

/// Toast notification severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToastLevel {
    #[default]
    Info,
    Success,
    Warn,
    Error,
}

impl ToastLevel {
    /// Returns the icon prefix for this toast level. Colors come from the theme palette.
    pub fn icon(self) -> &'static str {
        match self {
            ToastLevel::Info => "ℹ ",
            ToastLevel::Success => "✓ ",
            ToastLevel::Warn => "⚠ ",
            ToastLevel::Error => "✗ ",
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
    /// Push a numeric metric sample (`0..=100`) for sparkline and gauge widgets.
    PushMetric {
        key: String,
        value: u8,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        ttl_ms: Option<u64>,
    },
    /// Enqueue a stacked toast notification overlay.
    Toast {
        message: String,
        #[serde(default)]
        level: ToastLevel,
        #[serde(default = "default_toast_ttl_ms")]
        ttl_ms: u64,
    },
    /// Dynamically add or update a floating overlay.
    UpsertOverlay { overlay: OverlayConfig },
    /// Remove a floating overlay by ID.
    RemoveOverlay { id: String },
    /// Reload the configuration file (`config.toml`) in the active `thud` session.
    ReloadConfig,
    /// Gracefully terminate the active `thud` session.
    Quit,
}

fn default_toast_ttl_ms() -> u64 {
    4000
}
