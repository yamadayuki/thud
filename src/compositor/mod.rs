pub(crate) mod layout;
pub(crate) mod painter;
pub(crate) mod scheduler;
pub(crate) mod store;
pub(crate) mod vt;

pub use scheduler::{CommandKey, DueCommand};

use crate::compositor::layout::PlacementSolver;
use crate::compositor::painter::HudPainter;
use crate::compositor::scheduler::{CommandScheduler, ScheduleContext, watched_markers};
use crate::compositor::store::{TelemetryStore, TickChanges};
use crate::compositor::vt::{MouseEventKind, VtScreenAdapter, parse_mouse_event};
use crate::config::{Config, ConfigError, IpcMessage, TermSize, WidgetSpec};
use crate::ui::{Buffer, CellStyle};
use chrono::{DateTime, Local};
use std::collections::BTreeSet;
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
    /// `when_files` marker names (see `Compositor::watched_markers`) found in the CWD or its ancestors.
    pub found_markers: BTreeSet<String>,
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
    /// A dispatched shell command (`sh -c ...`) finished. `output` is its stdout, or `None` if
    /// it failed to run.
    CommandFinished {
        key: CommandKey,
        output: Option<String>,
    },
    /// Dynamic update received from the Unix domain socket IPC server.
    Ipc(IpcMessage),
    /// Replaces the active `Config` in-flight, resizing the child PTY if `status_bar` height changed
    /// and forcing a full screen redraw.
    ReloadConfig(Box<Config>),
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
    /// Shell commands whose schedule is now due for async execution.
    pub due_commands: Vec<DueCommand>,
    /// True when the user pressed `reload_key` or sent `IpcMessage::ReloadConfig`.
    pub reload_config: bool,
}

/// Sans-I/O Deep Module that orchestrates virtual terminal emulation (`VtScreenAdapter`),
/// telemetry & IPC state (`TelemetryStore`), collision-dodging placement (`PlacementSolver`),
/// and 2D cell rendering (`Buffer` / `Widget`).
pub struct Compositor {
    host_size: TermSize,
    config: Config,
    vt: VtScreenAdapter,
    store: TelemetryStore,
    scheduler: CommandScheduler,
    zen_mode: bool,
    dirty: bool,
    last_now: Instant,
    current_buffer: Buffer,
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
        let vt = VtScreenAdapter::new(child_size.cols, child_size.rows, config.scrollback_lines);
        let mut comp = Self {
            host_size: initial_host_size,
            config,
            vt,
            store: TelemetryStore::new(),
            scheduler: CommandScheduler::default(),
            zen_mode: false,
            dirty: false,
            last_now: now,
            current_buffer: Buffer::new(initial_host_size.cols, initial_host_size.rows),
            prev_buffer: None,
        };

        let due_commands = comp.collect_due_commands(now, TickChanges::default());
        let host_ansi_diff = comp.compose_and_diff(now);

        Ok((
            comp,
            Transition {
                host_ansi_diff,
                child_pty_input: Vec::new(),
                child_pty_resize: Some(child_size),
                due_commands,
                reload_config: false,
            },
        ))
    }

    /// Entry point 2: Processes a single `Event` deterministically and returns the resulting `Transition`.
    /// This method is infallible.
    pub fn step(&mut self, event: Event<'_>, now: Instant) -> Transition {
        self.last_now = now;
        let mut child_pty_input = Vec::new();
        let mut child_pty_resize = None;
        let mut due_commands = Vec::new();
        let mut reload_config = false;

        match event {
            Event::HostInput(bytes) => {
                let zen_byte = Config::parse_hotkey_byte(self.config.zen_toggle_key.as_deref());
                let reload_byte = Config::parse_hotkey_byte(self.config.reload_key.as_deref());
                let child_rect = PlacementSolver::compute_child_rect(
                    self.host_size,
                    &self.config,
                    self.zen_mode,
                );

                let mut i = 0;
                while i < bytes.len() {
                    if let Some(mouse_ev) = parse_mouse_event(&bytes[i..]) {
                        if self.vt.wants_mouse() {
                            if mouse_ev.is_sgr {
                                let adj_row = mouse_ev.row.saturating_sub(child_rect.y).max(1);
                                let suffix = if mouse_ev.release { 'm' } else { 'M' };
                                let _ = std::io::Write::write_fmt(
                                    &mut child_pty_input,
                                    format_args!(
                                        "\x1b[<{};{};{}{}",
                                        mouse_ev.btn, mouse_ev.col, adj_row, suffix
                                    ),
                                );
                            } else {
                                child_pty_input.extend_from_slice(&bytes[i..i + mouse_ev.len]);
                            }
                        } else if self.vt.is_alternate_screen() {
                            match mouse_ev.kind {
                                MouseEventKind::WheelUp => {
                                    child_pty_input.extend_from_slice(b"\x1b[A");
                                }
                                MouseEventKind::WheelDown => {
                                    child_pty_input.extend_from_slice(b"\x1b[B");
                                }
                                MouseEventKind::Other => {}
                            }
                        } else {
                            match mouse_ev.kind {
                                MouseEventKind::WheelUp => {
                                    self.vt.scroll_up(3);
                                }
                                MouseEventKind::WheelDown => {
                                    self.vt.scroll_down(3);
                                }
                                MouseEventKind::Other => {}
                            }
                        }
                        i += mouse_ev.len;
                        continue;
                    }

                    let b = bytes[i];
                    if Some(b) == zen_byte {
                        self.zen_mode = !self.zen_mode;
                        let new_child_size = self.child_size();
                        self.vt.set_size(new_child_size.cols, new_child_size.rows);
                        child_pty_resize = Some(new_child_size);
                    } else if Some(b) == reload_byte {
                        reload_config = true;
                    } else {
                        self.vt.reset_scrollback();
                        child_pty_input.push(b);
                    }
                    i += 1;
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
                    self.current_buffer.resize(clamped.cols, clamped.rows);
                    self.prev_buffer = None;
                    child_pty_resize = Some(new_child_size);
                }
            }
            Event::Tick {
                wall_clock,
                builtins,
            } => {
                let changes = self.store.on_tick(wall_clock, builtins, now);
                due_commands = self.collect_due_commands(now, changes);
            }
            Event::CommandFinished { key, output } => {
                if let Some(sample) = self.scheduler.finish(key, output.as_deref()) {
                    self.store.push_metric(sample.key, sample.value);
                }
            }
            Event::Ipc(msg) => {
                if msg == IpcMessage::ReloadConfig {
                    reload_config = true;
                } else {
                    self.store.apply_ipc(msg, now);
                }
            }
            Event::ReloadConfig(new_config) => {
                let old_child_size = self.child_size();
                self.config = *new_config;
                let new_child_size = self.child_size();
                if new_child_size != old_child_size {
                    self.vt.set_size(new_child_size.cols, new_child_size.rows);
                    child_pty_resize = Some(new_child_size);
                }
                self.prev_buffer = None;
                due_commands = self.collect_due_commands(now, TickChanges::default());
            }
        }

        self.dirty = true;
        Transition {
            host_ansi_diff: Vec::new(),
            child_pty_input,
            child_pty_resize,
            due_commands,
            reload_config,
        }
    }

    /// Returns true if the virtual terminal, telemetry store, or overlays have changed
    /// since the last frame was rendered.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Forces a full screen redraw and diff emission on next render.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Renders the composited screen frame and generates minimal ANSI diff if dirty.
    /// Resets `dirty` to false. Returns `None` if not dirty.
    pub fn render(&mut self, now: Instant) -> Option<Vec<u8>> {
        if !self.dirty {
            return None;
        }
        let diff = self.compose_and_diff(now);
        self.dirty = false;
        Some(diff)
    }

    /// Returns the current composited screen as plain text (one line per row, separated by `\n`).
    pub fn rendered_text(&self) -> String {
        self.build_buffer(self.last_now).to_plain_text()
    }

    /// Returns the child PTY dimensions calculated for the current host terminal size and Zen Mode state.
    pub fn child_size(&self) -> TermSize {
        PlacementSolver::compute_child_size(self.host_size, &self.config, self.zen_mode)
    }

    /// Returns whether Zen Mode (all HUD hidden) is currently active.
    pub fn is_zen_mode(&self) -> bool {
        self.zen_mode
    }

    /// Marker file names referenced by `when_files` filters, to be resolved against the CWD
    /// and reported back through `BuiltinValues::found_markers`.
    pub fn watched_markers(&self) -> BTreeSet<String> {
        watched_markers(effective_widgets(&self.config, &self.store))
    }

    fn collect_due_commands(&mut self, now: Instant, changes: TickChanges) -> Vec<DueCommand> {
        self.scheduler.collect_due(
            effective_widgets(&self.config, &self.store),
            ScheduleContext {
                now,
                cwd_changed: changes.cwd_changed,
                cmd_finished: changes.cmd_finished,
                found_markers: &self.store.builtins().found_markers,
            },
        )
    }

    fn compose_and_diff(&mut self, now: Instant) -> Vec<u8> {
        let mut buf = std::mem::take(&mut self.current_buffer);
        buf.clear(CellStyle::default());
        self.paint_frame(&mut buf, now);
        let diff = buf.diff(self.prev_buffer.as_ref());
        match self.prev_buffer {
            Some(ref mut prev) => {
                if prev.cols == buf.cols && prev.rows == buf.rows {
                    prev.cells.copy_from_slice(&buf.cells);
                    prev.cursor = buf.cursor;
                } else {
                    *prev = buf.clone();
                }
            }
            None => {
                self.prev_buffer = Some(buf.clone());
            }
        }
        self.current_buffer = buf;
        diff
    }

    fn build_buffer(&self, now: Instant) -> Buffer {
        let mut buf = Buffer::new(self.host_size.cols, self.host_size.rows);
        self.paint_frame(&mut buf, now);
        buf
    }

    fn paint_frame(&self, buf: &mut Buffer, now: Instant) {
        let child_rect =
            PlacementSolver::compute_child_rect(self.host_size, &self.config, self.zen_mode);
        self.vt.paint_into_buffer(child_rect, buf);
        if self.zen_mode {
            return;
        }
        HudPainter {
            config: &self.config,
            store: &self.store,
            commands: &self.scheduler,
            now,
        }
        .paint(buf, child_rect, self.vt.is_alternate_screen());
    }
}

/// Every widget currently configured: status bar sections, config overlays, and
/// IPC-injected dynamic overlays.
pub(crate) fn effective_widgets<'a>(
    config: &'a Config,
    store: &'a TelemetryStore,
) -> impl Iterator<Item = &'a WidgetSpec> {
    config
        .status_bar
        .left
        .iter()
        .chain(&config.status_bar.center)
        .chain(&config.status_bar.right)
        .chain(config.overlays.iter().flat_map(|ov| ov.widgets.iter()))
        .chain(store.dynamic_overlays().flat_map(|ov| ov.widgets.iter()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CollisionPolicy, ToastLevel};
    use chrono::TimeZone;
    use std::time::Duration;

    fn due(id: &str, cmd: &str) -> DueCommand {
        DueCommand {
            key: CommandKey::Widget(id.to_string()),
            cmd: cmd.to_string(),
        }
    }

    fn finished(id: &str, output: &str) -> Event<'static> {
        Event::CommandFinished {
            key: CommandKey::Widget(id.to_string()),
            output: Some(output.to_string()),
        }
    }

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
        assert_eq!(init_tx.due_commands, vec![due("custom_cmd", "echo ok")]);

        let fixed_time = Local.with_ymd_and_hms(2026, 10, 22, 14, 30, 0).unwrap();
        comp.step(
            Event::Tick {
                wall_clock: fixed_time,
                builtins: BuiltinValues {
                    cwd: Some("~/thud".to_string()),
                    ..BuiltinValues::default()
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
        comp.step(finished("custom_cmd", "R1"), now);

        assert_eq!(
            comp.rendered_text(),
            concat!(
                "hello world    14:30\n",
                "                    \n",
                "                    \n",
                "~/thud | NORM     R1"
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

        let mut without_overlays = sample_config();
        without_overlays.overlays.clear();
        comp.step(Event::ReloadConfig(Box::new(without_overlays)), now);
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
    fn intercepts_zen_mode_hotkey_and_tracks_active_command() {
        let t0 = Instant::now();
        let cfg = Config::from_toml(
            r#"
            zen_toggle_key = "ctrl-\\"

            [status_bar]
            height = 1
            left = [{ type = "active_command", min_secs = 2 }]
            right = []
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

        // 2. Toggle Zen Mode via Ctrl+\ (0x1c) -> HUD hidden and child PTY expands to 20x4!
        let tx_zen = comp.step(Event::HostInput(b"a\x1cb"), t3);
        assert_eq!(tx_zen.child_pty_input, b"ab");
        assert!(comp.is_zen_mode());
        assert_eq!(
            tx_zen.child_pty_resize,
            Some(TermSize { cols: 20, rows: 4 })
        );
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
        // Before the first tick no marker has been found -> not due
        assert!(init_tx.due_commands.is_empty());
        assert_eq!(comp.watched_markers(), ["Cargo.toml".to_string()].into());

        let in_crate = |fg_process: Option<&str>| BuiltinValues {
            cwd: Some("~/thud/src".to_string()),
            fg_process: fg_process.map(str::to_string),
            found_markers: ["Cargo.toml".to_string()].into(),
            ..BuiltinValues::default()
        };
        let tick = |builtins| Event::Tick {
            wall_clock: Local::now(),
            builtins,
        };

        // 1. Enter a directory whose ancestors contain `Cargo.toml` -> due!
        let tx1 = comp.step(tick(in_crate(None)), now);
        assert_eq!(tx1.due_commands, vec![due("rust", "rustc -V")]);
        comp.step(finished("rust", "1.85.0"), now);
        assert!(comp.rendered_text().contains("1.85.0"));

        // 2. Same directory, no foreground command completion -> NOT due
        let tx2 = comp.step(tick(in_crate(None)), now + Duration::from_secs(10));
        assert!(tx2.due_commands.is_empty());

        // 3. Foreground command starts and then finishes -> due again!
        comp.step(
            tick(in_crate(Some("rustup"))),
            now + Duration::from_secs(11),
        );
        let tx3 = comp.step(tick(in_crate(None)), now + Duration::from_secs(12));
        assert_eq!(tx3.due_commands, vec![due("rust", "rustc -V")]);

        // 4. Leave to a directory without `Cargo.toml` -> not due and cached output cleared
        let tx4 = comp.step(
            tick(BuiltinValues {
                cwd: Some("/".to_string()),
                ..BuiltinValues::default()
            }),
            now + Duration::from_secs(13),
        );
        assert!(tx4.due_commands.is_empty());
        assert!(!comp.rendered_text().contains("1.85.0"));
    }

    #[test]
    fn in_flight_commands_are_not_redispatched_and_failures_keep_previous_output() {
        let t0 = Instant::now();
        let (mut comp, init_tx) =
            Compositor::new(TermSize { cols: 20, rows: 4 }, sample_config(), t0).unwrap();
        assert_eq!(init_tx.due_commands, vec![due("custom_cmd", "echo ok")]);

        let tick = |comp: &mut Compositor, at: Instant| {
            comp.step(
                Event::Tick {
                    wall_clock: Local::now(),
                    builtins: BuiltinValues::default(),
                },
                at,
            )
            .due_commands
        };

        // Interval elapsed but the first run is still in flight -> skipped
        assert!(tick(&mut comp, t0 + Duration::from_secs(3)).is_empty());

        comp.step(finished("custom_cmd", "R1"), t0 + Duration::from_secs(3));
        assert_eq!(
            tick(&mut comp, t0 + Duration::from_secs(4)),
            vec![due("custom_cmd", "echo ok")]
        );

        // A failed run releases the command and keeps the previous output on screen
        comp.step(
            Event::CommandFinished {
                key: CommandKey::Widget("custom_cmd".to_string()),
                output: None,
            },
            t0 + Duration::from_secs(4),
        );
        assert!(comp.rendered_text().contains("R1"));
        assert_eq!(
            tick(&mut comp, t0 + Duration::from_secs(6)),
            vec![due("custom_cmd", "echo ok")]
        );
    }

    #[test]
    fn reload_reruns_only_commands_whose_spec_changed() {
        let t0 = Instant::now();
        let config = |uptime_cmd: &str| {
            Config::from_toml(&format!(
                r#"
                overlays = []

                [status_bar]
                left = [
                    {{ type = "command", id = "uptime", cmd = "{uptime_cmd}", trigger = "cwd" }},
                    {{ type = "command", id = "user", cmd = "whoami", trigger = "cwd" }}
                ]
                "#
            ))
            .unwrap()
        };
        let (mut comp, init_tx) =
            Compositor::new(TermSize { cols: 40, rows: 3 }, config("uptime"), t0).unwrap();
        assert_eq!(init_tx.due_commands.len(), 2);
        comp.step(finished("uptime", "UP"), t0);
        comp.step(finished("user", "me"), t0);

        let tx = comp.step(Event::ReloadConfig(Box::new(config("uptime"))), t0);
        assert!(tx.due_commands.is_empty());

        let tx = comp.step(Event::ReloadConfig(Box::new(config("uptime -p"))), t0);
        assert_eq!(tx.due_commands, vec![due("uptime", "uptime -p")]);
        assert!(comp.rendered_text().contains("me"));
    }

    #[test]
    fn metric_commands_and_widget_commands_have_distinct_keys() {
        let t0 = Instant::now();
        let cfg = Config::from_toml(
            r#"
            overlays = []

            [status_bar]
            separator = " "
            left = [
                { type = "command", id = "__metric__:load", cmd = "echo text" },
                { type = "gauge", key = "load", width = 4, cmd = "echo 40" }
            ]
            "#,
        )
        .unwrap();
        let (mut comp, init_tx) = Compositor::new(TermSize { cols: 40, rows: 3 }, cfg, t0).unwrap();
        assert_eq!(
            init_tx.due_commands,
            vec![
                due("__metric__:load", "echo text"),
                DueCommand {
                    key: CommandKey::Metric("load".to_string()),
                    cmd: "echo 40".to_string(),
                },
            ]
        );

        comp.step(finished("__metric__:load", "TEXT"), t0);
        comp.step(
            Event::CommandFinished {
                key: CommandKey::Metric("load".to_string()),
                output: Some("40%".to_string()),
            },
            t0,
        );
        let bar = comp.rendered_text().lines().last().unwrap().to_string();
        assert!(bar.contains("TEXT"), "{bar}");
        assert!(bar.contains("40%"), "{bar}");
    }

    #[test]
    fn reload_key_and_reload_config_event_update_compositor_in_place() {
        let now = Instant::now();
        let cfg = Config::default();
        let (mut comp, _) = Compositor::new(TermSize { cols: 30, rows: 3 }, cfg, now).unwrap();

        // Ctrl+] (0x1d) is intercepted and triggers reload_config without forwarding to child PTY
        let tx = comp.step(Event::HostInput(b"x\x1dy"), now);
        assert_eq!(tx.child_pty_input, b"xy");
        assert!(tx.reload_config);

        // Applying Event::ReloadConfig swaps active configuration (including `position = "top"`)
        let updated_cfg = Config::from_toml(
            r#"
            overlays = []

            [status_bar]
            position = "top"
            separator = " | "
            left = [{ type = "ipc_slot", key = "msg" }]
            right = []
        "#,
        )
        .unwrap();
        comp.step(Event::PtyOutput(b"shell-line"), now);
        comp.step(
            Event::Ipc(IpcMessage::SetSlot {
                key: "msg".to_string(),
                value: "RELOADED".to_string(),
                ttl_ms: None,
            }),
            now,
        );
        comp.step(Event::ReloadConfig(Box::new(updated_cfg)), now);
        let rendered = comp.rendered_text();
        let lines: Vec<&str> = rendered.lines().collect();
        assert!(lines[0].contains("RELOADED"));
        assert!(lines[1].starts_with("shell-line"));
    }

    #[test]
    fn mouse_wheel_scrolls_virtual_screen_scrollback_and_key_snaps_back() {
        let now = Instant::now();
        let cfg = Config::from_toml(
            r#"
            overlays = []
            [status_bar]
            enabled = false
        "#,
        )
        .unwrap();

        // 2-row terminal
        let (mut comp, _) = Compositor::new(TermSize { cols: 20, rows: 2 }, cfg, now).unwrap();

        // Generate 6 lines of output
        comp.step(
            Event::PtyOutput(b"line 1\r\nline 2\r\nline 3\r\nline 4\r\nline 5\r\nline 6"),
            now,
        );
        let bottom_text = comp.rendered_text();
        assert!(bottom_text.contains("line 5"));
        assert!(bottom_text.contains("line 6"));
        assert!(!bottom_text.contains("line 1"));

        // Wheel Up (SGR: button 64 at col 10, row 2) -> scrolls up by 3 rows
        let tx_scroll = comp.step(Event::HostInput(b"\x1b[<64;10;2M"), now);
        assert!(tx_scroll.child_pty_input.is_empty());
        let scrolled_text = comp.rendered_text();
        assert!(scrolled_text.contains("line 2") || scrolled_text.contains("line 3"));

        // Wheel Down (SGR: button 65 at col 10, row 2) -> scrolls down toward bottom
        comp.step(Event::HostInput(b"\x1b[<65;10;2M"), now);
        let scrolled_down = comp.rendered_text();
        assert!(scrolled_down.contains("line 5") || scrolled_down.contains("line 6"));

        // Wheel up again, then typing any key snaps scrollback to 0 and forwards key to child PTY
        comp.step(Event::HostInput(b"\x1b[<64;10;2M"), now);
        let tx_key = comp.step(Event::HostInput(b"ls\r"), now);
        assert_eq!(tx_key.child_pty_input, b"ls\r");
        let restored_text = comp.rendered_text();
        assert!(restored_text.contains("line 6"));
    }

    #[test]
    fn mouse_events_forward_to_child_app_when_requested_or_in_alternate_screen() {
        let now = Instant::now();
        let cfg = Config::default();
        let (mut comp, _) = Compositor::new(TermSize { cols: 40, rows: 10 }, cfg, now).unwrap();

        // 1. Child in alternate screen without mouse mode (e.g. `less`) translates wheel up/down to Up/Down arrow keys
        comp.step(Event::PtyOutput(b"\x1b[?1049h"), now);
        let tx_less_up = comp.step(Event::HostInput(b"\x1b[<64;10;5M"), now);
        assert_eq!(tx_less_up.child_pty_input, b"\x1b[A");
        let tx_less_down = comp.step(Event::HostInput(b"\x1b[<65;10;5M"), now);
        assert_eq!(tx_less_down.child_pty_input, b"\x1b[B");

        // 2. Child enables SGR mouse mode (e.g. `vim`) -> mouse events are forwarded directly with adjusted row
        comp.step(Event::PtyOutput(b"\x1b[?1000h\x1b[?1006h"), now);
        let tx_vim = comp.step(Event::HostInput(b"\x1b[<0;15;4M"), now);
        assert_eq!(tx_vim.child_pty_input, b"\x1b[<0;15;4M");
    }

    #[test]
    fn pty_ingestion_decoupled_from_render_and_dirty_flag_lifecycle() {
        let now = Instant::now();
        let cfg = Config::default();
        let (mut comp, _) = Compositor::new(TermSize { cols: 40, rows: 10 }, cfg, now).unwrap();

        // After initialization, compositor starts clean
        assert!(!comp.is_dirty());
        assert!(comp.render(now).is_none());

        // Ingesting PTY output marks dirty but does not emit host_ansi_diff in Transition
        let tx = comp.step(Event::PtyOutput(b"streamed log chunk"), now);
        assert!(tx.host_ansi_diff.is_empty());
        assert!(comp.is_dirty());

        // Dedicated render call generates diff and clears dirty flag
        let diff = comp.render(now);
        assert!(diff.is_some());
        assert!(!comp.is_dirty());

        // Subsequent render call without events returns None (no wasted composition)
        assert!(comp.render(now).is_none());
    }
}
