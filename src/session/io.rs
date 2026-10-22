use crate::compositor::layout::PlacementSolver;
use crate::compositor::{BuiltinValues, CommandKey};
use crate::config::{Config, ConfigError, IpcMessage, TermSize};
use crate::session::SessionError;
use crate::session::ipc::OsIpcServer;
use crate::session::probes::SystemProbes;
use crate::session::pty::OsChildPty;
use crate::session::tty::{self, OsHostTerminal};
use chrono::{DateTime, Local};
use std::collections::BTreeSet;
use std::io;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// Which inputs became readable during `SessionIo::wait`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Readiness {
    pub host: bool,
    pub child: bool,
    pub child_hangup: bool,
}

/// The single port through which `SessionLoop` reaches everything outside the process:
/// host terminal, child PTY, IPC socket, signals, periodic commands, telemetry probes,
/// clocks, and the config source.
pub(crate) trait SessionIo {
    fn now(&self) -> Instant;
    fn wall_clock(&self) -> DateTime<Local>;

    /// Blocks for up to `timeout` until host input or child output is readable.
    fn wait(&mut self, timeout: Duration) -> io::Result<Readiness>;
    /// Reads pending host input into `buf`, returning `0` when nothing could be read.
    fn read_host(&mut self, buf: &mut [u8]) -> usize;
    /// Reads pending child output into `buf`, returning `0` on EOF or when nothing could be read.
    fn read_child(&mut self, buf: &mut [u8]) -> usize;

    fn host_size(&self) -> io::Result<TermSize>;
    fn write_host(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn write_child(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn resize_child(&mut self, size: TermSize) -> io::Result<()>;
    fn child_exit(&mut self) -> io::Result<Option<i32>>;

    /// Returns true once per host window resize (`SIGWINCH`).
    fn take_resize(&mut self) -> bool;
    /// Returns true once per termination request (`SIGTERM`, `SIGHUP`, `SIGINT`).
    fn take_terminate(&mut self) -> bool;

    fn drain_ipc(&mut self) -> Vec<IpcMessage>;
    /// Starts `cmd` asynchronously in the child's working directory. Its completion is always
    /// reported by `drain_finished_commands` under `key`: stdout on success, `None` on failure.
    fn spawn_command(&mut self, key: CommandKey, cmd: String);
    fn drain_finished_commands(&mut self) -> Vec<(CommandKey, Option<String>)>;
    /// Samples builtin telemetry, including which of `watched_markers` exist in the CWD or
    /// its ancestors.
    fn sample_builtins(
        &mut self,
        now: Instant,
        watched_markers: &BTreeSet<String>,
    ) -> BuiltinValues;

    /// Returns true when the config source changed since the last `load_config`.
    fn config_changed(&mut self) -> bool;
    fn load_config(&mut self) -> Result<Config, ConfigError>;
}

/// Production `SessionIo` adapter over the real host terminal, `forkpty` child, Unix socket,
/// OS probes, and background command threads.
pub(crate) struct OsSessionIo {
    // Field order is drop order: hang up the child first, then remove the socket,
    // and restore the host terminal last.
    pty: OsChildPty,
    ipc: OsIpcServer,
    probes: SystemProbes,
    cmd_tx: Sender<(CommandKey, Option<String>)>,
    cmd_rx: Receiver<(CommandKey, Option<String>)>,
    config_mtime: Option<SystemTime>,
    host: OsHostTerminal,
}

impl OsSessionIo {
    /// Enters raw mode on the host terminal, binds the IPC socket, and spawns the child shell
    /// sized for the HUD layout described by `config`.
    pub fn start(config: &Config) -> Result<Self, SessionError> {
        let host = OsHostTerminal::enter()?;
        let host_size = host.size()?;

        let socket_path = config.ipc_socket.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("thud-{}.sock", std::process::id()))
        });
        let ipc = OsIpcServer::bind(&socket_path)?;

        let shell = config
            .shell
            .clone()
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/sh".to_string());
        let child_size = PlacementSolver::compute_child_size(host_size, config, false);
        let pty = OsChildPty::spawn(&shell, child_size, &socket_path)?;

        let (cmd_tx, cmd_rx) = mpsc::channel();
        Ok(Self {
            pty,
            ipc,
            probes: SystemProbes::new(),
            cmd_tx,
            cmd_rx,
            config_mtime: current_config_mtime(),
            host,
        })
    }
}

fn current_config_mtime() -> Option<SystemTime> {
    let path = Config::default_path()?;
    std::fs::metadata(path).ok()?.modified().ok()
}

fn read_fd(fd: libc::c_int, buf: &mut [u8]) -> usize {
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n > 0 { n as usize } else { 0 }
}

impl SessionIo for OsSessionIo {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wall_clock(&self) -> DateTime<Local> {
        Local::now()
    }

    fn wait(&mut self, timeout: Duration) -> io::Result<Readiness> {
        let mut fds = [
            libc::pollfd {
                fd: self.host.tty_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.pty.master_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let timeout_ms = timeout.as_millis().min(libc::c_int::MAX as u128) as libc::c_int;
        let res = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if res <= 0 {
            return Ok(Readiness::default());
        }
        Ok(Readiness {
            host: (fds[0].revents & libc::POLLIN) != 0,
            child: (fds[1].revents & (libc::POLLIN | libc::POLLHUP)) != 0,
            child_hangup: (fds[1].revents & libc::POLLHUP) != 0,
        })
    }

    fn read_host(&mut self, buf: &mut [u8]) -> usize {
        read_fd(self.host.tty_fd(), buf)
    }

    fn read_child(&mut self, buf: &mut [u8]) -> usize {
        read_fd(self.pty.master_fd, buf)
    }

    fn host_size(&self) -> io::Result<TermSize> {
        self.host.size()
    }

    fn write_host(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.host.write_diff(bytes)
    }

    fn write_child(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.pty.write_all(bytes)
    }

    fn resize_child(&mut self, size: TermSize) -> io::Result<()> {
        self.pty.resize(size)
    }

    fn child_exit(&mut self) -> io::Result<Option<i32>> {
        self.pty.try_wait()
    }

    fn take_resize(&mut self) -> bool {
        tty::take_sigwinch()
    }

    fn take_terminate(&mut self) -> bool {
        tty::take_sigterm()
    }

    fn drain_ipc(&mut self) -> Vec<IpcMessage> {
        self.ipc.drain_messages()
    }

    fn spawn_command(&mut self, key: CommandKey, cmd: String) {
        let cwd = SystemProbes::resolve_pty_cwd(self.pty.master_fd, self.pty.child_pid);
        let tx = self.cmd_tx.clone();
        thread::spawn(move || {
            let mut command = Command::new("/bin/sh");
            command
                .arg("-c")
                .arg(&cmd)
                .stdin(Stdio::null())
                .stderr(Stdio::null());
            if let Some(dir) = cwd {
                command.current_dir(dir);
            }
            let output = command
                .output()
                .ok()
                .map(|out| String::from_utf8_lossy(&out.stdout).to_string());
            let _ = tx.send((key, output));
        });
    }

    fn drain_finished_commands(&mut self) -> Vec<(CommandKey, Option<String>)> {
        self.cmd_rx.try_iter().collect()
    }

    fn sample_builtins(
        &mut self,
        now: Instant,
        watched_markers: &BTreeSet<String>,
    ) -> BuiltinValues {
        self.probes
            .sample(self.pty.master_fd, self.pty.child_pid, now, watched_markers)
    }

    fn config_changed(&mut self) -> bool {
        current_config_mtime() != self.config_mtime
    }

    fn load_config(&mut self) -> Result<Config, ConfigError> {
        self.config_mtime = current_config_mtime();
        Config::load_or_default()
    }
}
