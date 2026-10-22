pub(crate) mod layout;
pub(crate) mod store;
pub(crate) mod vt;

use crate::compositor::layout::PlacementSolver;
use crate::compositor::store::TelemetryStore;
use crate::compositor::vt::VtScreenAdapter;
use crate::config::{
    Anchor, Color, Config, ConfigError, IpcMessage, OverlayConfig, ScreenCondition, TermSize,
};
use crate::ui::{Block, Buffer, CellStyle, Gauge, Line, Rect, StatusBar, Toast, Widget};
use chrono::{DateTime, Local};
use std::time::Instant;

/// Built-in environment values passed into `Event::Tick`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuiltinValues {
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    /// System battery percentage (`0..=100`), automatically feeding the `"battery"` metric slot.
    pub battery: Option<u8>,
    /// Active foreground process name inside the child PTY (None if idle at the root shell).
    pub fg_process: Option<String>,
    /// System-wide CPU load percentage (`0..=100`), automatically feeding the `"cpu"` metric slot.
    pub cpu_usage: Option<u8>,
}

/// All external occurrences that drive the Sans-I/O `Compositor` state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<'a> {
    /// Raw input bytes read from the host terminal's stdin (intercepts HUD hotkeys).
    HostInput(&'a [u8]),
    /// Raw output bytes read from the child PTY master fd.
    PtyOutput(&'a [u8]),
    /// Host terminal window resize (`SIGWINCH`).
    HostResize(TermSize),
    /// Periodic timer tick with current local wall clock and builtin environment snapshot.
    Tick {
        wall_clock: DateTime<Local>,
        builtins: BuiltinValues,
    },
    /// Completed output from an asynchronous periodic shell command (`sh -c ...`).
    CommandOutput { id: String, output: String },
    /// Dynamic update received from the Unix domain socket IPC server.
    Ipc(IpcMessage),
}

/// Deterministic output effects produced by a single `Compositor` transition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transition {
    /// Minimal ANSI escape sequence diff to write to the host terminal's stdout.
    pub host_ansi_diff: Vec<u8>,
    /// Bytes to write to the child PTY master fd (forwarded user input + terminal query replies).
    pub child_pty_input: Vec<u8>,
    /// New child PTY dimensions to apply via `TIOCSWINSZ` if changed.
    pub child_pty_resize: Option<TermSize>,
    /// Periodic shell commands `(id, cmd)` whose schedule is now due for async execution.
    pub due_commands: Vec<(String, String)>,
}

/// Sans-I/O Deep Module that orchestrates virtual terminal emulation (`VtScreenAdapter`),
/// telemetry & IPC state (`TelemetryStore`), collision-dodging placement (`PlacementSolver`),
/// and 2D cell rendering (`Buffer` / `Widget`).
pub struct Compositor {
    host_size: TermSize,
    config: Config,
    vt: VtScreenAdapter,
    store: TelemetryStore,
    zen_mode: bool,
    dashboard_mode: bool,
    prev_buffer: Option<Buffer>,
}

impl Compositor {
    /// Entry point 1: Initializes the `Compositor` and returns the initial `Transition`.
    pub fn new(
        initial_host_size: TermSize,
        config: Config,
        now: Instant,
    ) -> Result<(Self, Transition), ConfigError> {
        if initial_host_size.cols == 0 || initial_host_size.rows == 0 {
            return Err(ConfigError::InvalidGeometry(
                "host terminal dimensions must be at least 1x1".to_string(),
            ));
        }

        let child_size = PlacementSolver::compute_child_size(initial_host_size, &config, false);
        let vt = VtScreenAdapter::new(child_size.cols, child_size.rows);
        let mut store = TelemetryStore::new();
        let due_commands = store.collect_due_commands(&config, now, false, false);

        let mut comp = Self {
            host_size: initial_host_size,
            config,
            vt,
            store,
            zen_mode: false,
            dashboard_mode: false,
            prev_buffer: None,
        };

        let host_ansi_diff = comp.compose_and_diff(now);

        Ok((
            comp,
            Transition {
                host_ansi_diff,
                child_pty_input: Vec::new(),
                child_pty_resize: Some(child_size),
                due_commands,
            },
        ))
    }

    /// Entry point 2: Processes a single `Event` deterministically and returns the resulting `Transition`.
    /// This method is infallible.
    pub fn step(&mut self, event: Event<'_>, now: Instant) -> Transition {
        let mut child_pty_input = Vec::new();
        let mut child_pty_resize = None;
        let mut due_commands = Vec::new();

        match event {
            Event::HostInput(bytes) => {
                let zen_byte = Config::parse_hotkey_byte(self.config.zen_toggle_key.as_deref());
                let dash_byte =
                    Config::parse_hotkey_byte(self.config.dashboard_toggle_key.as_deref());

                for &b in bytes {
                    if Some(b) == zen_byte {
                        self.zen_mode = !self.zen_mode;
                        let new_child_size = self.child_size();
                        self.vt.set_size(new_child_size.cols, new_child_size.rows);
                        child_pty_resize = Some(new_child_size);
                    } else if Some(b) == dash_byte {
                        self.dashboard_mode = !self.dashboard_mode;
                    } else {
                        child_pty_input.push(b);
                    }
                }
            }
            Event::PtyOutput(bytes) => {
                child_pty_input = self.vt.process_and_respond(bytes);
            }
            Event::HostResize(new_size) => {
                let clamped = TermSize {
                    cols: new_size.cols.max(1),
                    rows: new_size.rows.max(1),
                };
                if clamped != self.host_size {
                    self.host_size = clamped;
                    let new_child_size = self.child_size();
                    self.vt.set_size(new_child_size.cols, new_child_size.rows);
                    self.prev_buffer = None;
                    child_pty_resize = Some(new_child_size);
                }
            }
            Event::Tick {
                wall_clock,
                builtins,
            } => {
                due_commands = self.store.on_tick(wall_clock, builtins, &self.config, now);
            }
            Event::CommandOutput { id, output } => {
                self.store.on_command_output(id, &output);
            }
            Event::Ipc(msg) => {
                self.store.apply_ipc(msg, now);
            }
        }

        let host_ansi_diff = self.compose_and_diff(now);
        Transition {
            host_ansi_diff,
            child_pty_input,
            child_pty_resize,
            due_commands,
        }
    }

    /// Returns the current composited screen as plain text (one line per row, separated by `\n`).
    pub fn rendered_text(&self) -> String {
        match &self.prev_buffer {
            Some(buf) => buf.to_plain_text(),
            None => self.build_buffer(Instant::now()).to_plain_text(),
        }
    }

    /// Returns the child PTY dimensions calculated for the current host terminal size and Zen Mode state.
    pub fn child_size(&self) -> TermSize {
        PlacementSolver::compute_child_size(self.host_size, &self.config, self.zen_mode)
    }

    /// Returns whether Zen Mode (all HUD hidden) is currently active.
    pub fn is_zen_mode(&self) -> bool {
        self.zen_mode
    }

    /// Returns whether Dashboard Mode is currently active.
    pub fn is_dashboard_mode(&self) -> bool {
        self.dashboard_mode
    }

    fn compose_and_diff(&mut self, now: Instant) -> Vec<u8> {
        let next = self.build_buffer(now);
        let diff = next.diff(self.prev_buffer.as_ref());
        self.prev_buffer = Some(next);
        diff
    }

    fn build_buffer(&self, now: Instant) -> Buffer {
        let mut buf = Buffer::new(self.host_size.cols, self.host_size.rows);
        let child_size = self.child_size();
        let palette = self.config.theme.palette();

        // 1. Project child virtual terminal screen cells & cursor into `buf`
        self.vt.paint_into_buffer(child_size, &mut buf);

        if self.zen_mode {
            return buf;
        }

        // 2. Render docked bottom status bar if enabled and terminal is tall enough
        let reserved = self.config.reserved_bottom_rows();
        if self.config.status_bar.enabled && self.host_size.rows > reserved && reserved > 0 {
            let status_row = self.host_size.rows - reserved;
            let bar_style = CellStyle::new(
                self.config.status_bar.fg.unwrap_or(palette.bar_fg),
                self.config.status_bar.bg.unwrap_or(palette.bar_bg),
                false,
            );
            let sep = &self.config.status_bar.separator;
            let theme = self.config.theme;
            let bar = StatusBar {
                base_style: bar_style,
                left: self.store.build_segment_line(
                    &self.config.status_bar.left,
                    sep,
                    bar_style,
                    theme,
                    now,
                ),
                center: self.store.build_segment_line(
                    &self.config.status_bar.center,
                    sep,
                    bar_style,
                    theme,
                    now,
                ),
                right: self.store.build_segment_line(
                    &self.config.status_bar.right,
                    sep,
                    bar_style,
                    theme,
                    now,
                ),
            };
            bar.render(
                Rect::new(0, status_row, self.host_size.cols, reserved),
                &mut buf,
            );
        }

        // 3. Render floating overlays sorted by z_index
        let mut overlays: Vec<&OverlayConfig> = self.config.overlays.iter().collect();
        for dyn_ov in self.store.dynamic_overlays.values() {
            overlays.push(&dyn_ov.config);
        }
        overlays.sort_by_key(|ov| ov.z_index);

        let is_alt = self.vt.is_alternate_screen();
        for ov in overlays {
            self.render_overlay(&mut buf, ov, child_size, is_alt, now);
        }

        // 4. Render stacked toast notifications and unreferenced progress gauges
        self.render_toast_stack(&mut buf, child_size);

        buf
    }

    fn render_overlay(
        &self,
        buf: &mut Buffer,
        ov: &OverlayConfig,
        child_size: TermSize,
        is_alt: bool,
        now: Instant,
    ) {
        match ov.screen_mode {
            ScreenCondition::Always => {}
            ScreenCondition::PrimaryOnly if is_alt => return,
            ScreenCondition::AlternateOnly if !is_alt => return,
            ScreenCondition::DashboardOnly if !self.dashboard_mode => return,
            _ => {}
        }

        let palette = self.config.theme.palette();
        let base_style = CellStyle::new(
            ov.fg.unwrap_or(palette.overlay_fg),
            ov.bg.unwrap_or(palette.overlay_bg),
            ov.bold,
        );

        let mut lines: Vec<Line> = Vec::new();
        for w in &ov.widgets {
            let evaluated =
                self.store
                    .evaluate_widget_lines(w, base_style, self.config.theme, false, now);
            lines.extend(evaluated);
        }
        if lines.is_empty() {
            return;
        }

        let border_fg = ov.border_fg.or(Some(palette.border_fg));
        let title_fg = ov.title_fg.or(Some(palette.title_fg));
        let block = Block::new(ov.border, ov.title.as_deref(), base_style, lines)
            .with_border_and_title_fg(border_fg, title_fg)
            .with_padding_x(ov.padding_x);
        let (box_width, box_height) = block.measure();

        let Some(rect) = PlacementSolver::resolve_overlay_rect(
            buf,
            child_size,
            ov.anchor,
            ov.offset_x,
            ov.offset_y,
            box_width,
            box_height,
            ov.collision,
        ) else {
            return;
        };

        block.render(rect, buf);
    }

    fn render_toast_stack(&self, buf: &mut Buffer, child_size: TermSize) {
        let mut items: Vec<(Line, CellStyle)> = Vec::new();

        // 1. Collect any active unreferenced IPC progress metrics (`thud progress <key> <pct>`)
        for (key, series) in &self.store.metrics {
            if !series.from_ipc || self.store.is_metric_referenced(&self.config, key) {
                continue;
            }
            let label = series.label.as_deref().unwrap_or(key.as_str());
            let style = CellStyle::ansi(231, 25, true);
            let line = Gauge::new(series.latest, 10, Some(label), style)
                .to_line()
                .with_affixes(Some(" "), Some(" "), style);
            items.push((line, style));
        }

        // 2. Collect stacked toast notifications
        for toast_entry in self.store.toasts.iter().rev() {
            let (fg_ansi, bg_ansi, icon) = toast_entry.level.palette();
            let style = CellStyle::ansi(fg_ansi, bg_ansi, true);
            let line = Toast {
                icon,
                message: &toast_entry.message,
                style,
            }
            .to_line();
            items.push((line, style));
        }

        if items.is_empty() {
            return;
        }

        let toast_cfg = self.config.toast;
        let is_top = matches!(
            toast_cfg.anchor,
            Anchor::TopRight | Anchor::TopLeft | Anchor::TopCenter
        );
        let mut current_row = if is_top {
            i32::from(toast_cfg.margin_y)
        } else {
            i32::from(child_size.rows) - 1 - i32::from(toast_cfg.margin_y)
        };

        for (mut line, style) in items {
            if toast_cfg.pill {
                line = line.decorate_segment(None, false, true, None, None, style, Color::Default);
            }

            let block = Block::new(toast_cfg.border, None, style, vec![line]);
            let (box_w, box_h) = block.measure();
            if box_w == 0 || box_h == 0 || box_w > child_size.cols {
                continue;
            }

            let top_row = if is_top {
                current_row
            } else {
                current_row - i32::from(box_h) + 1
            };
            if top_row < 0 || top_row + i32::from(box_h) > i32::from(child_size.rows) {
                break;
            }

            let col = match toast_cfg.anchor {
                Anchor::TopRight | Anchor::BottomRight => child_size
                    .cols
                    .saturating_sub(box_w.saturating_add(toast_cfg.margin_x)),
                Anchor::TopLeft | Anchor::BottomLeft => toast_cfg
                    .margin_x
                    .min(child_size.cols.saturating_sub(box_w)),
                Anchor::TopCenter | Anchor::BottomCenter => {
                    child_size.cols.saturating_sub(box_w) / 2
                }
            };

            block.render(Rect::new(col, top_row as u16, box_w, box_h), buf);

            let step = i32::from(box_h.saturating_add(toast_cfg.gap));
            if is_top {
                current_row += step;
            } else {
                current_row -= step;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CollisionPolicy, ToastLevel};
    use chrono::TimeZone;
    use std::time::Duration;

    fn sample_config() -> Config {
        Config::from_toml(
            r#"
            [status_bar]
            enabled = true
            height = 1
            separator = " | "
            left = [
                { type = "cwd" },
                { type = "ipc_slot", key = "mode" }
            ]
            right = [
                { type = "command", id = "custom_cmd", cmd = "echo ok", interval_ms = 2000 }
            ]

            [[overlays]]
            id = "clock"
            anchor = "top_right"
            collision = { mode = "dodge_to_opposite_corner", margin = 1 }
            widgets = [{ type = "clock", format = "%H:%M" }]
        "#,
        )
        .unwrap()
    }

    #[test]
    fn reserves_bottom_status_row_and_composes_top_right_clock() {
        let now = Instant::now();
        let (mut comp, init_tx) =
            Compositor::new(TermSize { cols: 20, rows: 4 }, sample_config(), now).unwrap();

        assert_eq!(
            init_tx.child_pty_resize,
            Some(TermSize { cols: 20, rows: 3 })
        );
        assert_eq!(
            init_tx.due_commands,
            vec![("custom_cmd".to_string(), "echo ok".to_string())]
        );

        let fixed_time = Local.with_ymd_and_hms(2026, 10, 22, 14, 30, 0).unwrap();
        comp.step(
            Event::Tick {
                wall_clock: fixed_time,
                builtins: BuiltinValues {
                    cwd: Some("~/cth".to_string()),
                    git_branch: None,
                    battery: None,
                    fg_process: None,
                    cpu_usage: None,
                },
            },
            now,
        );
        comp.step(Event::PtyOutput(b"hello world"), now);
        comp.step(
            Event::Ipc(IpcMessage::SetSlot {
                key: "mode".to_string(),
                value: "NORM".to_string(),
                ttl_ms: None,
            }),
            now,
        );
        comp.step(
            Event::CommandOutput {
                id: "custom_cmd".to_string(),
                output: "R1".to_string(),
            },
            now,
        );

        assert_eq!(
            comp.rendered_text(),
            concat!(
                "hello world    14:30\n",
                "                    \n",
                "                    \n",
                "~/cth | NORM      R1"
            )
        );
    }

    #[test]
    fn heals_split_cjk_wide_characters_and_restores_cleanly_in_alternate_screen() {
        let now = Instant::now();
        let mut cfg = sample_config();
        cfg.overlays[0].collision = CollisionPolicy::Ignore;
        let (mut comp, _) = Compositor::new(TermSize { cols: 20, rows: 4 }, cfg, now).unwrap();

        let fixed_time = Local.with_ymd_and_hms(2026, 10, 22, 9, 5, 0).unwrap();
        comp.step(
            Event::Tick {
                wall_clock: fixed_time,
                builtins: BuiltinValues::default(),
            },
            now,
        );

        comp.step(
            Event::PtyOutput(b"\x1b[?1049h\x1b[1;11H\xe3\x81\x82\xe3\x81\x84\xe3\x81\x86\xe3\x81\x88\xe3\x81\x8a\x1b[1;1H"),
            now,
        );

        assert_eq!(
            comp.rendered_text().lines().next().unwrap(),
            "          あい 09:05"
        );

        comp.config.overlays.clear();
        comp.step(Event::PtyOutput(b""), now);
        assert_eq!(
            comp.rendered_text().lines().next().unwrap(),
            "          あいうえお"
        );
    }

    #[test]
    fn dodges_top_right_overlay_to_top_left_when_cursor_approaches() {
        let now = Instant::now();
        let (mut comp, _) =
            Compositor::new(TermSize { cols: 20, rows: 4 }, sample_config(), now).unwrap();

        let fixed_time = Local.with_ymd_and_hms(2026, 10, 22, 14, 30, 0).unwrap();
        comp.step(
            Event::Tick {
                wall_clock: fixed_time,
                builtins: BuiltinValues::default(),
            },
            now,
        );

        comp.step(Event::PtyOutput(b"\x1b[1;16H"), now);

        assert_eq!(
            comp.rendered_text().lines().next().unwrap(),
            "14:30               "
        );
    }

    #[test]
    fn ipc_ttl_expires_slots_and_dynamic_overlays_automatically() {
        let t0 = Instant::now();
        let (mut comp, _) =
            Compositor::new(TermSize { cols: 20, rows: 4 }, sample_config(), t0).unwrap();

        comp.step(
            Event::Ipc(IpcMessage::SetSlot {
                key: "mode".to_string(),
                value: "BUILD".to_string(),
                ttl_ms: Some(1000),
            }),
            t0,
        );
        assert!(comp.rendered_text().contains("BUILD"));

        let t1 = t0 + Duration::from_millis(1500);
        comp.step(
            Event::Tick {
                wall_clock: Local.with_ymd_and_hms(2026, 10, 22, 14, 30, 1).unwrap(),
                builtins: BuiltinValues::default(),
            },
            t1,
        );
        assert!(!comp.rendered_text().contains("BUILD"));
    }

    #[test]
    fn dodges_underlying_text_when_first_line_reaches_top_right() {
        let now = Instant::now();
        let (mut comp, _) =
            Compositor::new(TermSize { cols: 20, rows: 4 }, sample_config(), now).unwrap();

        let fixed_time = Local.with_ymd_and_hms(2026, 10, 22, 14, 30, 0).unwrap();
        comp.step(
            Event::Tick {
                wall_clock: fixed_time,
                builtins: BuiltinValues::default(),
            },
            now,
        );

        comp.step(Event::PtyOutput(b"0123456789abcdefghij\r\nOK"), now);

        let rendered = comp.rendered_text();
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "0123456789abcdefghij");
        assert_eq!(lines[1], "OK             14:30");
    }

    #[test]
    fn responds_to_fish_and_vt_terminal_queries() {
        let now = Instant::now();
        let (mut comp, _) =
            Compositor::new(TermSize { cols: 80, rows: 24 }, sample_config(), now).unwrap();

        let tx = comp.step(
            Event::PtyOutput(b"hello\x1b[?u\x1b[>0q\x1b]11;?\x1b\\\x1b[0c\x1b[6n"),
            now,
        );
        let reply = String::from_utf8_lossy(&tx.child_pty_input);
        assert!(reply.contains("\x1b[?0u"));
        assert!(reply.contains("\x1bP>|thud(0.1.0)\x1b\\"));
        assert!(reply.contains("\x1b]11;rgb:1c1c/1c1c/1c1c\x1b\\"));
        assert!(reply.contains("\x1b[?62;22c"));
        assert!(reply.contains("\x1b[1;6R"));
    }

    #[test]
    fn renders_bordered_multi_widget_overlay_sparkline_gauge_and_toast_stack() {
        let now = Instant::now();
        let cfg = Config::from_toml(
            r#"
            theme = "tokyo_night"

            [status_bar]
            enabled = true
            height = 1
            separator = " "
            left = [
                { type = "sparkline", key = "cpu", width = 4 },
                { type = "gauge", key = "build", width = 4 }
            ]
            right = []

            [[overlays]]
            id = "sys"
            anchor = "top_right"
            border = "rounded"
            title = "SYS"
            collision = { mode = "ignore" }
            widgets = [
                { type = "text", text = "OK" },
                { type = "text", text = "UP" }
            ]
        "#,
        )
        .unwrap();

        let (mut comp, _) = Compositor::new(TermSize { cols: 24, rows: 8 }, cfg, now).unwrap();

        comp.step(
            Event::Ipc(IpcMessage::PushMetric {
                key: "cpu".to_string(),
                value: 0,
                label: None,
                ttl_ms: None,
            }),
            now,
        );
        comp.step(
            Event::Ipc(IpcMessage::PushMetric {
                key: "cpu".to_string(),
                value: 100,
                label: None,
                ttl_ms: None,
            }),
            now,
        );
        comp.step(
            Event::Ipc(IpcMessage::PushMetric {
                key: "build".to_string(),
                value: 50,
                label: Some("B".to_string()),
                ttl_ms: None,
            }),
            now,
        );
        comp.step(
            Event::Ipc(IpcMessage::Toast {
                message: "T1".to_string(),
                level: ToastLevel::Info,
                ttl_ms: 4000,
            }),
            now,
        );
        comp.step(
            Event::Ipc(IpcMessage::Toast {
                message: "T2".to_string(),
                level: ToastLevel::Success,
                ttl_ms: 4000,
            }),
            now,
        );

        let rendered = comp.rendered_text();
        let lines: Vec<&str> = rendered.lines().collect();
        // Top-right 4-line multi-widget rounded border box: ╭SYS╮ / │OK │ / │UP │ / ╰───╯
        assert_eq!(lines[0], "                   ╭SYS╮");
        assert_eq!(lines[1], "                   │OK │");
        assert_eq!(lines[2], "                   │UP │");
        assert_eq!(lines[3], "                   ╰───╯");
        // Default toast margins: margin_x = 2 (2 spaces on right), margin_y = 1 (row 6 left empty above status bar!)
        // so stacked toasts float at rows 4 and 5 with 2 trailing spaces!
        assert!(lines[4].ends_with(" ℹ T1   "));
        assert!(lines[5].ends_with(" ✓ T2   "));
        assert_eq!(lines[6].trim(), "");
        // Bottom status bar with sparkline & 3-char left-padded gauge percentage (` 50%`)
        assert!(lines[7].contains("▁▁▁█ B ━━──  50%"));
    }

    #[test]
    fn intercepts_zen_mode_and_dashboard_hotkeys_and_tracks_active_command() {
        let t0 = Instant::now();
        let cfg = Config::from_toml(
            r#"
            zen_toggle_key = "ctrl-\\"
            dashboard_toggle_key = "ctrl-]"

            [status_bar]
            height = 1
            left = [{ type = "active_command", min_secs = 2 }]
            right = []

            [[overlays]]
            id = "dash"
            anchor = "top_left"
            screen_mode = "dashboard_only"
            collision = { mode = "ignore" }
            widgets = [{ type = "text", text = "DASHBOARD" }]
        "#,
        )
        .unwrap();

        let (mut comp, _) = Compositor::new(TermSize { cols: 20, rows: 4 }, cfg, t0).unwrap();

        // 1. Foreground process "cargo" starts at t0 -> not shown yet (min_secs = 2)
        comp.step(
            Event::Tick {
                wall_clock: Local::now(),
                builtins: BuiltinValues {
                    fg_process: Some("cargo".to_string()),
                    ..Default::default()
                },
            },
            t0,
        );
        // At t0 + 3s, "cargo" has been running for 3s -> shown on status bar!
        let t3 = t0 + Duration::from_secs(3);
        comp.step(
            Event::Tick {
                wall_clock: Local::now(),
                builtins: BuiltinValues {
                    fg_process: Some("cargo".to_string()),
                    ..Default::default()
                },
            },
            t3,
        );
        assert!(comp.rendered_text().contains("⚙ cargo"));

        // 2. Toggle Dashboard Mode via Ctrl+] (0x1d) -> "DASHBOARD" overlay appears!
        let tx_dash = comp.step(Event::HostInput(b"a\x1db"), t3);
        assert_eq!(tx_dash.child_pty_input, b"ab");
        assert!(comp.is_dashboard_mode());
        assert!(comp.rendered_text().contains("DASHBOARD"));

        // 3. Toggle Zen Mode via Ctrl+\ (0x1c) -> HUD hidden and child PTY expands to 20x4!
        let tx_zen = comp.step(Event::HostInput(b"\x1c"), t3);
        assert!(comp.is_zen_mode());
        assert_eq!(
            tx_zen.child_pty_resize,
            Some(TermSize { cols: 20, rows: 4 })
        );
        assert!(!comp.rendered_text().contains("DASHBOARD"));
        assert!(!comp.rendered_text().contains("⚙ cargo"));
    }

    #[test]
    fn cpu_sparkline_renders_automatically_from_builtin_cpu_telemetry() {
        let now = Instant::now();
        let cfg = Config::from_toml(
            r#"
            theme = "tokyo_night"

            [status_bar]
            separator = " "
            left = []
            right = [
                { type = "sparkline", key = "cpu", width = 8, label = "CPU", padding = true },
                { type = "gauge", key = "battery", width = 4, pill = true }
            ]
            overlays = []
        "#,
        )
        .unwrap();

        let (mut comp, _) = Compositor::new(TermSize { cols: 40, rows: 3 }, cfg, now).unwrap();
        comp.step(
            Event::Tick {
                wall_clock: Local::now(),
                builtins: BuiltinValues {
                    cpu_usage: Some(50),
                    battery: Some(75),
                    ..Default::default()
                },
            },
            now,
        );

        let rendered = comp.rendered_text();
        let last_line = rendered.lines().last().unwrap();
        assert!(last_line.contains(" CPU ▁▁▁▁▁▁▁▄ "));
        assert!(last_line.contains("━━━─  75%"));
    }

    #[test]
    fn event_driven_command_runs_on_cwd_and_cmd_finish_and_respects_when_files() {
        let now = Instant::now();
        let cfg = Config::from_toml(
            r#"
            [status_bar]
            separator = " | "
            left = []
            right = [
                { type = "command", id = "rust", cmd = "rustc -V", trigger = "change", when_files = ["Cargo.toml"] }
            ]
            overlays = []
        "#,
        )
        .unwrap();

        let (mut comp, init_tx) =
            Compositor::new(TermSize { cols: 30, rows: 3 }, cfg, now).unwrap();
        // Before first tick, CWD is None and when_files is non-empty -> not due
        assert!(init_tx.due_commands.is_empty());

        let repo_dir = std::env::current_dir()
            .unwrap()
            .join("src")
            .display()
            .to_string();

        // 1. Enter repository subdirectory (`src/` has ancestor `Cargo.toml`) -> due!
        let tx1 = comp.step(
            Event::Tick {
                wall_clock: Local::now(),
                builtins: BuiltinValues {
                    cwd: Some(repo_dir.clone()),
                    ..Default::default()
                },
            },
            now,
        );
        assert_eq!(
            tx1.due_commands,
            vec![("rust".to_string(), "rustc -V".to_string())]
        );
        comp.step(
            Event::CommandOutput {
                id: "rust".to_string(),
                output: "1.85.0".to_string(),
            },
            now,
        );
        assert!(comp.rendered_text().contains("1.85.0"));

        // 2. Subsequent tick in same directory with no foreground command completion -> NOT due
        let tx2 = comp.step(
            Event::Tick {
                wall_clock: Local::now(),
                builtins: BuiltinValues {
                    cwd: Some(repo_dir.clone()),
                    ..Default::default()
                },
            },
            now + Duration::from_secs(10),
        );
        assert!(tx2.due_commands.is_empty());

        // 3. Foreground command starts and then finishes -> due again!
        comp.step(
            Event::Tick {
                wall_clock: Local::now(),
                builtins: BuiltinValues {
                    cwd: Some(repo_dir.clone()),
                    fg_process: Some("rustup".to_string()),
                    ..Default::default()
                },
            },
            now + Duration::from_secs(11),
        );
        let tx3 = comp.step(
            Event::Tick {
                wall_clock: Local::now(),
                builtins: BuiltinValues {
                    cwd: Some(repo_dir),
                    fg_process: None,
                    ..Default::default()
                },
            },
            now + Duration::from_secs(12),
        );
        assert_eq!(
            tx3.due_commands,
            vec![("rust".to_string(), "rustc -V".to_string())]
        );

        // 4. Leave directory to `/` (no `Cargo.toml`) -> not due and cached output cleared immediately!
        let tx4 = comp.step(
            Event::Tick {
                wall_clock: Local::now(),
                builtins: BuiltinValues {
                    cwd: Some("/".to_string()),
                    ..Default::default()
                },
            },
            now + Duration::from_secs(13),
        );
        assert!(tx4.due_commands.is_empty());
        assert!(!comp.rendered_text().contains("1.85.0"));
    }
}
