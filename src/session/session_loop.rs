use crate::compositor::{Compositor, Event, Transition};
use crate::config::{Config, IpcMessage, ToastLevel};
use crate::session::SessionError;
use crate::session::io::SessionIo;
use std::io;
use std::time::{Duration, Instant};

/// Minimum interval between paced frames while the child streams output (~144 FPS).
const FRAME_INTERVAL: Duration = Duration::from_micros(6944);
/// Maximum time to wait for I/O while the screen is clean.
const IDLE_WAIT: Duration = Duration::from_millis(50);
/// Interval of telemetry sampling, command scheduling, and config change checks.
const TICK_INTERVAL: Duration = Duration::from_millis(250);

/// Owns every runtime policy of a live session and drives `Compositor` one iteration at a
/// time over a `SessionIo`.
pub(crate) struct SessionLoop<Io: SessionIo> {
    io: Io,
    compositor: Compositor,
    last_tick: Instant,
    last_frame: Instant,
    buf: Box<[u8; 8192]>,
}

impl<Io: SessionIo> SessionLoop<Io> {
    /// Composes the first frame: applies the initial transition, samples telemetry once so
    /// CWD and Git branch appear immediately, and flushes the result to the host.
    pub fn new(config: Config, io: Io) -> Result<Self, SessionError> {
        let now = io.now();
        let host_size = io.host_size()?;
        let (compositor, init_tx) = Compositor::new(host_size, config, now)?;
        let mut session = Self {
            io,
            compositor,
            last_tick: now,
            last_frame: now,
            buf: Box::new([0u8; 8192]),
        };
        apply(&mut session.io, &init_tx)?;
        session.tick(now)?;
        session.flush(now)?;
        Ok(session)
    }

    #[cfg(test)]
    pub fn io(&self) -> &Io {
        &self.io
    }

    #[cfg(test)]
    pub fn io_mut(&mut self) -> &mut Io {
        &mut self.io
    }

    /// Returns the current composited screen as plain text (`\n`-separated rows).
    #[cfg(test)]
    pub fn screen_text(&self) -> String {
        self.compositor.rendered_text()
    }

    /// Runs until the child exits or the session is told to quit, returning the exit code.
    pub fn run(&mut self) -> Result<i32, SessionError> {
        loop {
            if let Some(code) = self.run_once()? {
                return Ok(code);
            }
        }
    }

    /// Performs one wait-and-dispatch iteration. Returns the exit code once the session ends.
    pub fn run_once(&mut self) -> Result<Option<i32>, SessionError> {
        if self.io.take_terminate() {
            return Ok(Some(0));
        }
        if let Some(code) = self.io.child_exit()? {
            return Ok(Some(code));
        }
        if self.io.take_resize()
            && let Ok(size) = self.io.host_size()
        {
            self.step(Event::HostResize(size), self.io.now())?;
        }

        let ready = self.io.wait(self.wait_timeout())?;
        let now = self.io.now();

        if ready.host {
            let n = self.io.read_host(&mut self.buf[..]);
            if n > 0 {
                let tx = step(
                    &mut self.compositor,
                    &mut self.io,
                    Event::HostInput(&self.buf[..n]),
                    now,
                )?;
                if tx.reload_config {
                    self.reload(now)?;
                }
                // Interactive typing is flushed immediately instead of waiting for the next frame.
                self.flush(now)?;
            }
        }

        if ready.child {
            let n = self.io.read_child(&mut self.buf[..]);
            if n > 0 {
                step(
                    &mut self.compositor,
                    &mut self.io,
                    Event::PtyOutput(&self.buf[..n]),
                    now,
                )?;
            } else if let Some(code) = self.io.child_exit()? {
                return Ok(Some(code));
            }
        }

        for msg in self.io.drain_ipc() {
            if msg == IpcMessage::Quit {
                return Ok(Some(0));
            }
            let tx = self.step(Event::Ipc(msg), now)?;
            if tx.reload_config {
                self.reload(now)?;
            }
        }

        for (id, output) in self.io.drain_command_outputs() {
            self.step(Event::CommandOutput { id, output }, now)?;
        }

        if now.saturating_duration_since(self.last_tick) >= TICK_INTERVAL {
            if self.io.config_changed() {
                self.reload(now)?;
            }
            self.tick(now)?;
        }

        if now.saturating_duration_since(self.last_frame) >= FRAME_INTERVAL {
            self.flush(now)?;
        }
        Ok(None)
    }

    /// While dirty, waits only for the remainder of the current frame interval; otherwise idles.
    fn wait_timeout(&self) -> Duration {
        if !self.compositor.is_dirty() {
            return IDLE_WAIT;
        }
        let elapsed = self.io.now().saturating_duration_since(self.last_frame);
        if elapsed >= FRAME_INTERVAL {
            Duration::ZERO
        } else {
            (FRAME_INTERVAL - elapsed).max(Duration::from_millis(1))
        }
    }

    fn step(&mut self, event: Event<'_>, now: Instant) -> io::Result<Transition> {
        step(&mut self.compositor, &mut self.io, event, now)
    }

    fn tick(&mut self, now: Instant) -> io::Result<()> {
        self.last_tick = now;
        let builtins = self.io.sample_builtins(now);
        let wall_clock = self.io.wall_clock();
        self.step(
            Event::Tick {
                wall_clock,
                builtins,
            },
            now,
        )?;
        Ok(())
    }

    /// Reloads the config source in place, or shows its first error line as a toast.
    fn reload(&mut self, now: Instant) -> io::Result<()> {
        let event = match self.io.load_config() {
            Ok(config) => Event::ReloadConfig(config),
            Err(err) => Event::Ipc(IpcMessage::Toast {
                message: err
                    .to_string()
                    .lines()
                    .next()
                    .unwrap_or("invalid config")
                    .to_string(),
                level: ToastLevel::Error,
                ttl_ms: 5000,
            }),
        };
        self.step(event, now)?;
        self.flush(now)
    }

    fn flush(&mut self, now: Instant) -> io::Result<()> {
        if let Some(diff) = self.compositor.render(now) {
            self.io.write_host(&diff)?;
            self.last_frame = now;
        }
        Ok(())
    }
}

/// Feeds `event` to the compositor and carries out the resulting transition.
/// Takes fields separately so callers can pass slices borrowed from the read buffer.
fn step<Io: SessionIo>(
    compositor: &mut Compositor,
    io: &mut Io,
    event: Event<'_>,
    now: Instant,
) -> io::Result<Transition> {
    let tx = compositor.step(event, now);
    apply(io, &tx)?;
    Ok(tx)
}

fn apply<Io: SessionIo>(io: &mut Io, tx: &Transition) -> io::Result<()> {
    if let Some(size) = tx.child_pty_resize {
        io.resize_child(size)?;
    }
    if !tx.child_pty_input.is_empty() {
        io.write_child(&tx.child_pty_input)?;
    }
    if !tx.host_ansi_diff.is_empty() {
        io.write_host(&tx.host_ansi_diff)?;
    }
    for (id, cmd) in &tx.due_commands {
        io.spawn_command(id.clone(), cmd.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::BuiltinValues;
    use crate::config::{ConfigError, TermSize};
    use crate::session::memory_io::{MemorySessionIo, SpawnedCommand};
    use chrono::{Local, TimeZone};

    fn text_bar_config(text: &str) -> Config {
        Config::from_toml(&format!(
            r#"
            overlays = []

            [status_bar]
            left = [{{ type = "text", text = "{text}" }}]
            "#
        ))
        .unwrap()
    }

    fn start(config: Config, io: MemorySessionIo) -> SessionLoop<MemorySessionIo> {
        let mut session = SessionLoop::new(config, io).unwrap();
        session.io_mut().take_host_writes();
        session
    }

    #[test]
    fn drives_fullscreen_vim_ipc_and_periodic_commands() {
        let config = Config::from_toml(
            r#"
            [status_bar]
            height = 1
            separator = " | "
            left = [
                { type = "cwd" },
                { type = "git_branch" },
                { type = "ipc_slot", key = "mode" }
            ]
            right = [
                { type = "command", id = "uptime", cmd = "echo up", interval_ms = 1000 }
            ]

            [[overlays]]
            id = "clock"
            anchor = "top_right"
            collision = { mode = "dodge_to_opposite_corner", margin = 1 }
            widgets = [{ type = "clock", format = "%H:%M" }]
        "#,
        )
        .unwrap();

        let mut io = MemorySessionIo::new(32, 4);
        io.set_wall_clock(Local.with_ymd_and_hms(2026, 10, 22, 14, 25, 0).unwrap())
            .set_cwd("/work/thud")
            .set_builtins(BuiltinValues {
                cwd: Some("~/thud".to_string()),
                git_branch: Some("git:main".to_string()),
                ..BuiltinValues::default()
            });
        let mut session = start(config, io);

        assert_eq!(
            session.io().child_size(),
            Some(TermSize { cols: 32, rows: 3 })
        );
        assert_eq!(
            session.io_mut().take_spawned(),
            vec![SpawnedCommand {
                id: "uptime".to_string(),
                cmd: "echo up".to_string(),
                cwd: Some("/work/thud".into()),
            }]
        );

        session
            .io_mut()
            .complete_command("uptime", "UP:5d")
            .push_pty_output(b"\x1b[?1049h\x1b[1;1H-- VIM BUFFER --")
            .push_ipc(IpcMessage::SetSlot {
                key: "mode".to_string(),
                value: "NORMAL".to_string(),
                ttl_ms: None,
            });
        assert_eq!(session.advance(Duration::ZERO), None);

        assert_eq!(
            session.screen_text(),
            concat!(
                "-- VIM BUFFER --           14:25\n",
                "                                \n",
                "                                \n",
                "~/thud | git:main | NORMAL UP:5d"
            )
        );

        // The interval command is dispatched again on the first tick after its interval elapses.
        session.advance(Duration::from_millis(1000) + TICK_INTERVAL);
        assert_eq!(session.io_mut().take_spawned().len(), 1);
    }

    #[test]
    fn paces_pty_bursts_but_flushes_host_input_immediately() {
        let mut session = start(text_bar_config("BAR"), MemorySessionIo::new(20, 4));

        for _ in 0..5 {
            session.io_mut().push_pty_output(b"x");
        }
        session.advance(Duration::ZERO);
        assert!(
            session.io_mut().take_host_writes().is_empty(),
            "PTY output must wait for the next frame interval"
        );

        session.advance(Duration::from_millis(7));
        assert_eq!(session.io_mut().take_host_writes().len(), 1);
        assert!(session.screen_text().starts_with("xxxxx"));
        assert_eq!(session.io().last_wait(), Some(IDLE_WAIT));

        session.io_mut().push_host_input(b"a");
        session.advance(Duration::ZERO);
        assert_eq!(session.io_mut().take_host_writes().len(), 1);
        assert_eq!(session.io_mut().take_child_input(), b"a");
    }

    #[test]
    fn forwards_terminal_query_replies_to_child() {
        let mut session = start(text_bar_config("BAR"), MemorySessionIo::new(20, 4));
        session.io_mut().push_pty_output(b"\x1b[c");
        session.advance(Duration::ZERO);
        assert!(!session.io_mut().take_child_input().is_empty());
    }

    #[test]
    fn reloads_config_on_change_reload_key_and_ipc() {
        let mut session = start(text_bar_config("OLD"), MemorySessionIo::new(60, 6));

        session
            .io_mut()
            .change_config(Ok(text_bar_config("CHANGED")));
        session.advance(TICK_INTERVAL);
        assert!(session.screen_text().contains("CHANGED"));

        session
            .io_mut()
            .change_config(Err(ConfigError::TomlParse("bad key".to_string())));
        session.advance(TICK_INTERVAL);
        let screen = session.screen_text();
        assert!(
            screen.contains("invalid TOML configuration: bad key"),
            "{screen}"
        );
        assert!(
            screen.contains("CHANGED"),
            "invalid config keeps the old one"
        );

        session
            .io_mut()
            .set_config(Ok(text_bar_config("BY_KEY")))
            .push_host_input(&[0x1d]);
        session.advance(Duration::ZERO);
        assert!(session.screen_text().contains("BY_KEY"));
        assert!(session.io_mut().take_child_input().is_empty());

        session
            .io_mut()
            .set_config(Ok(text_bar_config("BY_IPC")))
            .push_ipc(IpcMessage::ReloadConfig);
        session.advance(Duration::ZERO);
        assert!(session.screen_text().contains("BY_IPC"));
    }

    #[test]
    fn ends_on_quit_terminate_and_child_exit() {
        let mut session = start(text_bar_config("BAR"), MemorySessionIo::new(20, 4));
        session.io_mut().push_ipc(IpcMessage::Quit);
        assert_eq!(session.advance(Duration::ZERO), Some(0));

        let mut session = start(text_bar_config("BAR"), MemorySessionIo::new(20, 4));
        session.io_mut().terminate();
        assert_eq!(session.advance(Duration::ZERO), Some(0));

        let mut session = start(text_bar_config("BAR"), MemorySessionIo::new(20, 4));
        session.io_mut().exit_child(3);
        assert_eq!(session.advance(Duration::ZERO), Some(3));
    }

    #[test]
    fn resizes_child_when_host_window_changes() {
        let mut session = start(text_bar_config("BAR"), MemorySessionIo::new(20, 4));
        assert_eq!(
            session.io().child_size(),
            Some(TermSize { cols: 20, rows: 3 })
        );

        session.io_mut().resize_host(30, 6);
        session.advance(Duration::ZERO);
        assert_eq!(
            session.io().child_size(),
            Some(TermSize { cols: 30, rows: 5 })
        );
    }
}
