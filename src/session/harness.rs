use crate::compositor::{BuiltinValues, Compositor, Event, Transition};
use crate::config::{Config, IpcMessage, TermSize};
use crate::session::SessionError;
use chrono::{DateTime, Duration as ChronoDuration, Local};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Deterministic in-memory test harness satisfying the `Session` seam.
pub struct HeadlessHarness {
    compositor: Compositor,
    now: Instant,
    wall_clock: DateTime<Local>,
    builtins: BuiltinValues,
    emitted_ansi: Vec<u8>,
    last_child_size: TermSize,
    stub_commands: HashMap<String, String>,
}

impl HeadlessHarness {
    pub(crate) fn new(config: Config, host_size: TermSize) -> Result<Self, SessionError> {
        let now = Instant::now();
        let wall_clock = Local::now();
        let (compositor, init_tx) = Compositor::new(host_size, config, now)?;
        let last_child_size = init_tx.child_pty_resize.unwrap_or(compositor.child_size());

        Ok(Self {
            compositor,
            now,
            wall_clock,
            builtins: BuiltinValues::default(),
            emitted_ansi: init_tx.host_ansi_diff,
            last_child_size,
            stub_commands: HashMap::new(),
        })
    }

    /// Sets the simulated wall-clock time and builtin values, then triggers a tick.
    pub fn set_environment(
        &mut self,
        wall_clock: DateTime<Local>,
        builtins: BuiltinValues,
    ) -> &mut Self {
        self.wall_clock = wall_clock;
        self.builtins = builtins.clone();
        let tx = self.compositor.step(
            Event::Tick {
                wall_clock,
                builtins,
            },
            self.now,
        );
        self.record_transition(tx);
        self
    }

    /// Registers a canned output for a periodic shell command ID in headless mode.
    pub fn stub_command_output(
        &mut self,
        id: impl Into<String>,
        output: impl Into<String>,
    ) -> &mut Self {
        self.stub_commands.insert(id.into(), output.into());
        self
    }

    /// Feeds child PTY output bytes into the virtual terminal.
    pub fn feed_pty_output(&mut self, bytes: &[u8]) -> &mut Self {
        let tx = self.compositor.step(Event::PtyOutput(bytes), self.now);
        self.record_transition(tx);
        self
    }

    /// Simulates a host terminal resize (`SIGWINCH`).
    pub fn resize_host(&mut self, cols: u16, rows: u16) -> &mut Self {
        let tx = self
            .compositor
            .step(Event::HostResize(TermSize { cols, rows }), self.now);
        self.record_transition(tx);
        self
    }

    /// Injects a JSON IPC payload just as if it arrived over the Unix domain socket.
    pub fn send_ipc_json(&mut self, json_payload: &str) -> Result<&mut Self, SessionError> {
        let msg: IpcMessage =
            serde_json::from_str(json_payload).map_err(|e| SessionError::Ipc(e.to_string()))?;
        let tx = self.compositor.step(Event::Ipc(msg), self.now);
        self.record_transition(tx);
        Ok(self)
    }

    /// Advances the virtual clock and evaluates due periodic commands and TTL expirations.
    pub fn tick(&mut self, elapsed: Duration) -> &mut Self {
        self.now += elapsed;
        if let Ok(chrono_delta) = ChronoDuration::from_std(elapsed) {
            self.wall_clock += chrono_delta;
        }
        let tx = self.compositor.step(
            Event::Tick {
                wall_clock: self.wall_clock,
                builtins: self.builtins.clone(),
            },
            self.now,
        );
        let due = tx.due_commands.clone();
        self.record_transition(tx);

        for (id, _cmd) in due {
            if let Some(out) = self.stub_commands.get(&id).cloned() {
                let cmd_tx = self
                    .compositor
                    .step(Event::CommandOutput { id, output: out }, self.now);
                self.record_transition(cmd_tx);
            }
        }
        self
    }

    /// Returns the current composited screen as plain text (`\n`-separated rows).
    pub fn screen_text(&self) -> String {
        self.compositor.rendered_text()
    }

    /// Returns the current child PTY dimensions.
    pub fn child_size(&self) -> TermSize {
        self.last_child_size
    }

    /// Drains and returns the raw ANSI diff sequences emitted since the last call.
    pub fn take_emitted_ansi(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.emitted_ansi)
    }

    fn record_transition(&mut self, tx: Transition) {
        if let Some(sz) = tx.child_pty_resize {
            self.last_child_size = sz;
        }
        if !tx.host_ansi_diff.is_empty() {
            self.emitted_ansi.extend_from_slice(&tx.host_ansi_diff);
        } else if let Some(diff) = self.compositor.render(self.now) {
            self.emitted_ansi.extend_from_slice(&diff);
        }
    }
}
