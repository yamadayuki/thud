pub mod harness;
pub(crate) mod ipc;
pub(crate) mod probes;
pub(crate) mod pty;
pub(crate) mod tty;

pub use harness::HeadlessHarness;

use crate::compositor::{Compositor, Event, Transition};
use crate::config::{Config, ConfigError, IpcMessage, TermSize, ToastLevel};
use crate::session::ipc::OsIpcServer;
use crate::session::probes::SystemProbes;
use crate::session::pty::OsChildPty;
use crate::session::tty::{OsHostTerminal, take_sigwinch};
use chrono::Local;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum SessionError {
    Config(ConfigError),
    Io(io::Error),
    Ipc(String),
    NestedSession,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Config(e) => write!(f, "{e}"),
            SessionError::Io(e) => write!(f, "I/O error: {e}"),
            SessionError::Ipc(msg) => write!(f, "IPC error: {msg}"),
            SessionError::NestedSession => write!(
                f,
                "cannot start a nested thud session inside an active thud session (use 'thud --force' to override, or 'exit' to leave)"
            ),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<ConfigError> for SessionError {
    fn from(e: ConfigError) -> Self {
        SessionError::Config(e)
    }
}

impl From<io::Error> for SessionError {
    fn from(e: io::Error) -> Self {
        SessionError::Io(e)
    }
}

/// Outer Deep Module providing a one-call session runner for production CLI use
/// and a deterministic `HeadlessHarness` for integration tests.
pub struct Session;

impl Session {
    /// Production entry point: switches the host terminal to raw + alternate screen mode,
    /// spawns the child shell inside a PTY, binds the Unix domain socket for IPC,
    /// runs the non-blocking compositor loop, and restores terminal state on exit.
    pub fn run(config: Config) -> Result<i32, SessionError> {
        Self::check_nested_session(config.force)?;

        let mut host = OsHostTerminal::enter()?;
        let initial_host_size = host.size()?;
        let now = Instant::now();

        let (mut compositor, init_tx) = Compositor::new(initial_host_size, config.clone(), now)?;
        let initial_child_size = compositor.child_size();

        let socket_path = config.ipc_socket.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("thud-{}.sock", std::process::id()))
        });
        let ipc_server = OsIpcServer::bind(&socket_path)?;

        let shell_prog = config
            .shell
            .clone()
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/sh".to_string());

        let mut pty = OsChildPty::spawn(&shell_prog, initial_child_size, &socket_path)?;
        let (cmd_tx, cmd_rx) = mpsc::channel::<(String, String)>();

        // Apply initial transition
        Self::apply_os_transition(&init_tx, &mut host, &mut pty, &cmd_tx)?;

        // Immediate initial builtin probe so CWD / Git branch appear on frame 1
        let mut last_tick = now;
        let mut watched_config_mtime = Self::current_config_mtime();
        let mut probes = SystemProbes::new();
        let builtins = probes.sample(pty.master_fd, pty.child_pid, now);
        let first_tick_tx = compositor.step(
            Event::Tick {
                wall_clock: Local::now(),
                builtins,
            },
            now,
        );
        Self::apply_os_transition(&first_tick_tx, &mut host, &mut pty, &cmd_tx)?;

        const FRAME_INTERVAL: Duration = Duration::from_micros(6944); // ~144 FPS
        let mut last_frame = now;

        if let Some(diff) = compositor.render(now) {
            host.write_diff(&diff)?;
            last_frame = now;
        }

        let mut buf = [0u8; 8192];
        let exit_code = loop {
            if tty::take_sigterm() {
                break 0;
            }

            if let Some(code) = pty.try_wait()? {
                break code;
            }

            if take_sigwinch()
                && let Ok(new_size) = host.size()
            {
                let tx = compositor.step(Event::HostResize(new_size), Instant::now());
                Self::apply_os_transition(&tx, &mut host, &mut pty, &cmd_tx)?;
            }

            // Dynamic poll timeout:
            // When dirty, wait only the remaining time until the next 144 FPS frame is due (0..=7ms)
            // When clean, wait up to 50ms for I/O.
            let poll_now = Instant::now();
            let poll_timeout_ms: libc::c_int = if compositor.is_dirty() {
                let elapsed = poll_now.saturating_duration_since(last_frame);
                if elapsed >= FRAME_INTERVAL {
                    0
                } else {
                    let rem = FRAME_INTERVAL - elapsed;
                    (rem.as_millis() as libc::c_int).max(1)
                }
            } else {
                50
            };

            let mut fds = [
                libc::pollfd {
                    fd: host.tty_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: pty.master_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];

            let poll_res = unsafe { libc::poll(fds.as_mut_ptr(), 2, poll_timeout_ms) };
            let loop_now = Instant::now();

            if poll_res > 0 {
                // 1. Route host terminal keystrokes through Compositor (handles hotkeys + forwards input)
                if (fds[0].revents & libc::POLLIN) != 0 {
                    let n = unsafe {
                        libc::read(
                            host.tty_fd(),
                            buf.as_mut_ptr() as *mut libc::c_void,
                            buf.len(),
                        )
                    };
                    if n > 0 {
                        let tx = compositor.step(Event::HostInput(&buf[..n as usize]), loop_now);
                        Self::apply_os_transition(&tx, &mut host, &mut pty, &cmd_tx)?;
                        if tx.reload_config {
                            watched_config_mtime = Self::current_config_mtime();
                            Self::reload_into_compositor(
                                &mut compositor,
                                &mut host,
                                &mut pty,
                                &cmd_tx,
                                loop_now,
                            )?;
                        }
                        // Immediate flush for interactive user typing: 0ms latency
                        if let Some(diff) = compositor.render(loop_now) {
                            host.write_diff(&diff)?;
                            last_frame = loop_now;
                        }
                    }
                }

                // 2. Read child PTY output and feed into Compositor
                if (fds[1].revents & (libc::POLLIN | libc::POLLHUP)) != 0 {
                    let n = unsafe {
                        libc::read(
                            pty.master_fd,
                            buf.as_mut_ptr() as *mut libc::c_void,
                            buf.len(),
                        )
                    };
                    if n > 0 {
                        let tx = compositor.step(Event::PtyOutput(&buf[..n as usize]), loop_now);
                        Self::apply_os_transition(&tx, &mut host, &mut pty, &cmd_tx)?;
                    } else if (n == 0 || (fds[1].revents & libc::POLLHUP) != 0)
                        && let Some(code) = pty.try_wait()?
                    {
                        break code;
                    }
                }
            }

            // 3. Drain non-blocking IPC socket messages
            let mut should_quit = false;
            for ipc_msg in ipc_server.drain_messages() {
                if ipc_msg == IpcMessage::Quit {
                    should_quit = true;
                    break;
                }
                let tx = compositor.step(Event::Ipc(ipc_msg), loop_now);
                Self::apply_os_transition(&tx, &mut host, &mut pty, &cmd_tx)?;
                if tx.reload_config {
                    watched_config_mtime = Self::current_config_mtime();
                    Self::reload_into_compositor(
                        &mut compositor,
                        &mut host,
                        &mut pty,
                        &cmd_tx,
                        loop_now,
                    )?;
                }
            }
            if should_quit {
                break 0;
            }

            // 4. Drain completed async periodic command outputs
            while let Ok((id, output)) = cmd_rx.try_recv() {
                let tx = compositor.step(Event::CommandOutput { id, output }, loop_now);
                Self::apply_os_transition(&tx, &mut host, &mut pty, &cmd_tx)?;
            }

            // 5. Periodic tick (every 250ms) for clock, CWD, Git branch, command scheduling, and config mtime check
            if loop_now.saturating_duration_since(last_tick) >= Duration::from_millis(250) {
                last_tick = loop_now;
                let current_mtime = Self::current_config_mtime();
                if current_mtime != watched_config_mtime {
                    watched_config_mtime = current_mtime;
                    Self::reload_into_compositor(
                        &mut compositor,
                        &mut host,
                        &mut pty,
                        &cmd_tx,
                        loop_now,
                    )?;
                }
                let builtins = probes.sample(pty.master_fd, pty.child_pid, loop_now);
                let tx = compositor.step(
                    Event::Tick {
                        wall_clock: Local::now(),
                        builtins,
                    },
                    loop_now,
                );
                Self::apply_os_transition(&tx, &mut host, &mut pty, &cmd_tx)?;
            }

            // 6. Frame pacing render (up to 144 FPS)
            if compositor.is_dirty()
                && loop_now.saturating_duration_since(last_frame) >= FRAME_INTERVAL
                && let Some(diff) = compositor.render(loop_now)
            {
                host.write_diff(&diff)?;
                last_frame = loop_now;
            }
        };

        Ok(exit_code)
    }

    /// Headless integration test entry point: creates an in-memory session harness with no OS I/O.
    pub fn run_headless(
        config: Config,
        cols: u16,
        rows: u16,
    ) -> Result<HeadlessHarness, SessionError> {
        HeadlessHarness::new(config, TermSize { cols, rows })
    }

    /// Sends a single `IpcMessage` over the Unix domain socket to a running `thud` session.
    pub fn send_ipc_message(
        socket_path: Option<PathBuf>,
        msg: &IpcMessage,
    ) -> Result<(), SessionError> {
        ipc::send_ipc_message(socket_path, msg)
    }

    pub(crate) fn check_nested_session(force: bool) -> Result<(), SessionError> {
        let current_tty = tty::get_tty_name(libc::STDIN_FILENO);
        Self::check_nested_session_impl(force, current_tty.as_deref())
    }

    pub(crate) fn check_nested_session_impl(
        force: bool,
        current_tty: Option<&str>,
    ) -> Result<(), SessionError> {
        if force {
            unsafe {
                std::env::remove_var("THUD_ACTIVE");
                std::env::remove_var("THUD_SOCKET");
                std::env::remove_var("THUD_TTY");
            }
            return Ok(());
        }

        if std::env::var("THUD_ACTIVE")
            .ok()
            .is_some_and(|v| !v.is_empty() && v != "0")
        {
            // If THUD_TTY is set and differs from current terminal's TTY, this is a separate terminal window/tab
            if let Ok(thud_tty) = std::env::var("THUD_TTY")
                && let Some(curr_tty) = current_tty
                && thud_tty != curr_tty
            {
                unsafe {
                    std::env::remove_var("THUD_ACTIVE");
                    std::env::remove_var("THUD_SOCKET");
                    std::env::remove_var("THUD_TTY");
                }
                return Ok(());
            }

            if let Ok(sock) = std::env::var("THUD_SOCKET") {
                if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
                    return Err(SessionError::NestedSession);
                }
                // Socket is unreachable: previous session terminated abnormally.
                // Clear stale inherited environment variables and proceed.
                let _ = std::fs::remove_file(&sock);
                unsafe {
                    std::env::remove_var("THUD_ACTIVE");
                    std::env::remove_var("THUD_SOCKET");
                    std::env::remove_var("THUD_TTY");
                }
            } else {
                unsafe {
                    std::env::remove_var("THUD_ACTIVE");
                }
            }
        }
        Ok(())
    }

    fn current_config_mtime() -> Option<std::time::SystemTime> {
        let path = Config::default_path()?;
        std::fs::metadata(path).ok()?.modified().ok()
    }

    fn reload_into_compositor(
        compositor: &mut Compositor,
        host: &mut OsHostTerminal,
        pty: &mut OsChildPty,
        cmd_tx: &Sender<(String, String)>,
        now: Instant,
    ) -> io::Result<()> {
        let tx = match Config::load_or_default() {
            Ok(new_config) => compositor.step(Event::ReloadConfig(new_config), now),
            Err(err) => {
                let first_line = err
                    .to_string()
                    .lines()
                    .next()
                    .unwrap_or("invalid config")
                    .to_string();
                compositor.step(
                    Event::Ipc(IpcMessage::Toast {
                        message: first_line,
                        level: ToastLevel::Error,
                        ttl_ms: 5000,
                    }),
                    now,
                )
            }
        };
        Self::apply_os_transition(&tx, host, pty, cmd_tx)?;
        if let Some(diff) = compositor.render(now) {
            host.write_diff(&diff)?;
        }
        Ok(())
    }

    fn apply_os_transition(
        tx: &Transition,
        host: &mut OsHostTerminal,
        pty: &mut OsChildPty,
        cmd_tx: &Sender<(String, String)>,
    ) -> io::Result<()> {
        if let Some(new_size) = tx.child_pty_resize {
            pty.resize(new_size)?;
        }
        if !tx.child_pty_input.is_empty() {
            pty.write_all(&tx.child_pty_input)?;
        }
        if !tx.host_ansi_diff.is_empty() {
            host.write_diff(&tx.host_ansi_diff)?;
        }
        if !tx.due_commands.is_empty() {
            let pty_cwd = SystemProbes::resolve_pty_cwd(pty.master_fd, pty.child_pid);
            for (id, cmd) in &tx.due_commands {
                let tx_clone = cmd_tx.clone();
                let id_owned = id.clone();
                let cmd_owned = cmd.clone();
                let cwd_owned = pty_cwd.clone();
                thread::spawn(move || {
                    let mut command = Command::new("/bin/sh");
                    command
                        .arg("-c")
                        .arg(&cmd_owned)
                        .stdin(Stdio::null())
                        .stderr(Stdio::null());
                    if let Some(dir) = cwd_owned {
                        command.current_dir(dir);
                    }
                    if let Ok(output) = command.output() {
                        let text = String::from_utf8_lossy(&output.stdout).to_string();
                        let _ = tx_clone.send((id_owned, text));
                    }
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::BuiltinValues;
    use chrono::TimeZone;

    #[test]
    fn headless_session_drives_fullscreen_vim_ipc_and_periodic_commands() {
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

        let mut sim = Session::run_headless(config, 32, 4).unwrap();
        assert_eq!(sim.child_size(), TermSize { cols: 32, rows: 3 });

        let t0 = Local.with_ymd_and_hms(2026, 10, 22, 14, 25, 0).unwrap();
        sim.stub_command_output("uptime", "UP:5d")
            .set_environment(
                t0,
                BuiltinValues {
                    cwd: Some("~/thud".to_string()),
                    git_branch: Some("git:main".to_string()),
                    battery: None,
                    fg_process: None,
                    cpu_usage: None,
                },
            )
            .feed_pty_output(b"\x1b[?1049h\x1b[1;1H-- VIM BUFFER --")
            .send_ipc_json(r#"{"action":"set_slot","key":"mode","value":"NORMAL"}"#)
            .unwrap()
            .tick(Duration::from_millis(1000));

        assert_eq!(
            sim.screen_text(),
            concat!(
                "-- VIM BUFFER --           14:25\n",
                "                                \n",
                "                                \n",
                "~/thud | git:main | NORMAL UP:5d"
            )
        );
    }

    #[test]
    fn unix_socket_ipc_roundtrip_delivers_messages_and_cleans_up_on_drop() {
        let sock_path = std::env::temp_dir().join(format!("thud-test-{}.sock", std::process::id()));
        {
            let server = OsIpcServer::bind(&sock_path).unwrap();
            Session::send_ipc_message(
                Some(sock_path.clone()),
                &IpcMessage::SetSlot {
                    key: "ci".to_string(),
                    value: "GREEN".to_string(),
                    ttl_ms: Some(5000),
                },
            )
            .unwrap();

            let received = server.drain_messages();
            assert_eq!(
                received,
                vec![IpcMessage::SetSlot {
                    key: "ci".to_string(),
                    value: "GREEN".to_string(),
                    ttl_ms: Some(5000),
                }]
            );
        }
        assert!(!sock_path.exists());
    }

    #[test]
    fn validates_nested_session_checks_socket_liveness() {
        let sock_path =
            std::env::temp_dir().join(format!("thud-alive-check-{}.sock", std::process::id()));
        let _server = OsIpcServer::bind(&sock_path).unwrap();

        unsafe {
            std::env::set_var("THUD_ACTIVE", "1");
            std::env::set_var("THUD_SOCKET", sock_path.as_os_str());
            std::env::remove_var("THUD_TTY");
        }

        // Alive socket without force rejects nested session
        assert!(matches!(
            Session::check_nested_session(false),
            Err(SessionError::NestedSession)
        ));

        // --force overrides even when socket is alive
        assert!(Session::check_nested_session(true).is_ok());
        assert!(std::env::var("THUD_ACTIVE").is_err());
        assert!(std::env::var("THUD_SOCKET").is_err());

        // When socket is dead / unreachable, stale env vars are cleared and session is allowed
        let dead_sock = std::env::temp_dir().join("thud-dead-sock-test.sock");
        let _ = std::fs::remove_file(&dead_sock);
        unsafe {
            std::env::set_var("THUD_ACTIVE", "1");
            std::env::set_var("THUD_SOCKET", dead_sock.as_os_str());
            std::env::set_var("THUD_TTY", "/dev/ttys_stale");
        }
        assert!(Session::check_nested_session(false).is_ok());
        assert!(std::env::var("THUD_ACTIVE").is_err());
        assert!(std::env::var("THUD_SOCKET").is_err());
        assert!(std::env::var("THUD_TTY").is_err());

        // When running in a different terminal window/tab (different TTY), allow and purge inherited env
        unsafe {
            std::env::set_var("THUD_ACTIVE", "1");
            std::env::set_var("THUD_SOCKET", sock_path.as_os_str());
            std::env::set_var("THUD_TTY", "/dev/ttys001");
        }
        assert!(Session::check_nested_session_impl(false, Some("/dev/ttys002")).is_ok());
        assert!(std::env::var("THUD_ACTIVE").is_err());
        assert!(std::env::var("THUD_SOCKET").is_err());
        assert!(std::env::var("THUD_TTY").is_err());

        // When running on the exact same TTY with alive socket, block
        unsafe {
            std::env::set_var("THUD_ACTIVE", "1");
            std::env::set_var("THUD_SOCKET", sock_path.as_os_str());
            std::env::set_var("THUD_TTY", "/dev/ttys001");
        }
        assert!(matches!(
            Session::check_nested_session_impl(false, Some("/dev/ttys001")),
            Err(SessionError::NestedSession)
        ));
    }
}
