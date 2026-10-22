pub mod harness;
pub(crate) mod ipc;
pub(crate) mod probes;
pub(crate) mod pty;
pub(crate) mod tty;

pub use harness::HeadlessHarness;

use crate::compositor::{Compositor, Event, Transition};
use crate::config::{Config, ConfigError, IpcMessage, TermSize};
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
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Config(e) => write!(f, "{e}"),
            SessionError::Io(e) => write!(f, "I/O error: {e}"),
            SessionError::Ipc(msg) => write!(f, "IPC error: {msg}"),
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

        let mut buf = [0u8; 8192];
        let exit_code = loop {
            if let Some(code) = pty.try_wait()? {
                break code;
            }

            if take_sigwinch()
                && let Ok(new_size) = host.size()
            {
                let tx = compositor.step(Event::HostResize(new_size), Instant::now());
                Self::apply_os_transition(&tx, &mut host, &mut pty, &cmd_tx)?;
            }

            // Poll stdin and child PTY master fd with a short timeout (50ms)
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

            let poll_res = unsafe { libc::poll(fds.as_mut_ptr(), 2, 50) };
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
            }
            if should_quit {
                break 0;
            }

            // 4. Drain completed async periodic command outputs
            while let Ok((id, output)) = cmd_rx.try_recv() {
                let tx = compositor.step(Event::CommandOutput { id, output }, loop_now);
                Self::apply_os_transition(&tx, &mut host, &mut pty, &cmd_tx)?;
            }

            // 5. Periodic tick (every 250ms) for clock, CWD, Git branch, and command scheduling
            if loop_now.saturating_duration_since(last_tick) >= Duration::from_millis(250) {
                last_tick = loop_now;
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
        for (id, cmd) in &tx.due_commands {
            let tx_clone = cmd_tx.clone();
            let id_owned = id.clone();
            let cmd_owned = cmd.clone();
            thread::spawn(move || {
                if let Ok(output) = Command::new("/bin/sh")
                    .arg("-c")
                    .arg(&cmd_owned)
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                {
                    let text = String::from_utf8_lossy(&output.stdout).to_string();
                    let _ = tx_clone.send((id_owned, text));
                }
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::BuiltinValues;
    use crate::session::probes::resolve_git_branch;
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
            widget = { type = "clock", format = "%H:%M" }
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
                    cwd: Some("~/cth".to_string()),
                    git_branch: Some("git:main".to_string()),
                    battery: None,
                    fg_process: None,
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
                "~/cth | git:main | NORMAL  UP:5d"
            )
        );
    }

    #[test]
    fn resolves_git_branch_from_repository_root() {
        let cwd = std::env::current_dir().unwrap();
        let branch = resolve_git_branch(&cwd);
        assert_eq!(branch.as_deref(), Some("git:main"));
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
}
