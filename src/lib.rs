pub mod compositor;
pub mod config;

pub use compositor::{BuiltinValues, Compositor, Event, Transition};
pub use config::{
    Anchor, CellStyle, CollisionPolicy, Color, Config, ConfigError, IpcMessage, OverlayConfig,
    ScreenCondition, StatusBarConfig, TermSize, WidgetSpec,
};
