pub(crate) mod layout;
pub(crate) mod store;
pub(crate) mod vt;

use crate::compositor::layout::PlacementSolver;
use crate::compositor::store::TelemetryStore;
use crate::compositor::vt::VtScreenAdapter;
use crate::config::{Config, ConfigError, IpcMessage, OverlayConfig, ScreenCondition, TermSize};
use crate::ui::{Block, Buffer, CellStyle, Gauge, Rect, StatusBar, Toast, Widget};
use chrono::{DateTime, Local};
use std::time::Instant;

/// Built-in environment values passed into `Event::Tick`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuiltinValues {
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub battery: Option<String>,
    /// Active foreground process name inside the child PTY (None if idle at the root shell).
    pub fg_process: Option<String>,
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
        let due_commands = store.collect_due_commands(&config, now);

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

        // 1. Project child virtual terminal screen cells & cursor into `buf`
        self.vt.paint_into_buffer(child_size, &mut buf);

        if self.zen_mode {
            return buf;
        }

        // 2. Render docked bottom status bar if enabled and terminal is tall enough
        let reserved = self.config.reserved_bottom_rows();
        if self.config.status_bar.enabled && self.host_size.rows > reserved && reserved > 0 {
            let status_row = self.host_size.rows - reserved;
            let bar_style = CellStyle::ansi(
                self.config.status_bar.fg_ansi,
                self.config.status_bar.bg_ansi,
                false,
            );
            let sep = &self.config.status_bar.separator;
            let bar = StatusBar {
                base_style: bar_style,
                left: self.store.build_segment_line(
                    &self.config.status_bar.left,
                    sep,
                    bar_style,
                    now,
                ),
                center: self.store.build_segment_line(
                    &self.config.status_bar.center,
                    sep,
                    bar_style,
                    now,
                ),
                right: self.store.build_segment_line(
                    &self.config.status_bar.right,
                    sep,
                    bar_style,
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

        let base_style = CellStyle::ansi(ov.fg_ansi, ov.bg_ansi, ov.bold);
        let lines = self
            .store
            .evaluate_widget_lines(&ov.widget, base_style, now);
        if lines.is_empty() {
            return;
        }

        let block = Block::new(ov.border, ov.title.as_deref(), base_style, lines);
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
        let mut current_row = i32::from(child_size.rows) - 1;

        // 1. Render any active unreferenced progress metrics (`thud progress <key> <pct>`)
        for (key, series) in &self.store.metrics {
            if current_row < 0 {
                break;
            }
            if self.store.is_metric_referenced(&self.config, key) {
                continue;
            }
            let label = series.label.as_deref().unwrap_or(key.as_str());
            let style = CellStyle::ansi(231, 25, true);
            let line = Gauge::new(series.latest, 10, Some(label), style)
                .to_line()
                .with_affixes(Some(" "), Some(" "), style);
            let width = line.width();
            if width == 0 || width > child_size.cols {
                continue;
            }
            let col = child_size.cols - width;
            (&line).render(Rect::new(col, current_row as u16, width, 1), buf);
            current_row -= 1;
        }

        // 2. Render stacked toast notifications
        for toast_entry in self.store.toasts.iter().rev() {
            if current_row < 0 {
                break;
            }
            let (fg_ansi, bg_ansi, icon) = toast_entry.level.palette();
            let style = CellStyle::ansi(fg_ansi, bg_ansi, true);
            let line = Toast {
                icon,
                message: &toast_entry.message,
                style,
            }
            .to_line();
            let width = line.width();
            if width == 0 || width > child_size.cols {
                continue;
            }
            let col = child_size.cols - width;
            (&line).render(Rect::new(col, current_row as u16, width, 1), buf);
            current_row -= 1;
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
            widget = { type = "clock", format = "%H:%M" }
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
    fn renders_bordered_overlay_sparkline_gauge_and_toast_stack() {
        let now = Instant::now();
        let cfg = Config::from_toml(
            r#"
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
            widget = { type = "text", text = "OK" }
        "#,
        )
        .unwrap();

        let (mut comp, _) = Compositor::new(TermSize { cols: 24, rows: 6 }, cfg, now).unwrap();

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
        // Top-right 3-line rounded border box: ╭SYS╮ / │OK │ / ╰───╯
        assert_eq!(lines[0], "                   ╭SYS╮");
        assert_eq!(lines[1], "                   │OK │");
        assert_eq!(lines[2], "                   ╰───╯");
        // Stacked toasts at bottom-right of child area (rows 3 and 4)
        assert!(lines[3].ends_with(" ℹ T1 "));
        assert!(lines[4].ends_with(" ✓ T2 "));
        // Bottom status bar with sparkline & gauge
        assert!(lines[5].contains("▁▁▁█ B ━━── 50%"));
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
            widget = { type = "text", text = "DASHBOARD" }
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
}
