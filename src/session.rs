use crate::compositor::{BuiltinValues, Compositor, Event, Transition};
use crate::config::{Config, ConfigError, IpcMessage, TermSize};
use chrono::{DateTime, Duration as ChronoDuration, Local};
use std::collections::HashMap;
use std::ffi::CString;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

static SIGWINCH_RECEIVED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigwinch(_sig: libc::c_int) {
    SIGWINCH_RECEIVED.store(true, Ordering::SeqCst);
}

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
            std::env::temp_dir().join(format!("cth-{}.sock", std::process::id()))
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
        let mut cached_battery: Option<String> = None;
        let mut last_battery_probe: Option<Instant> = None;
        let builtins = probe_builtins(
            pty.master_fd,
            pty.child_pid,
            &mut cached_battery,
            &mut last_battery_probe,
            now,
        );
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

            if SIGWINCH_RECEIVED.swap(false, Ordering::SeqCst)
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
                // 1. Forward host terminal keystrokes to child PTY
                if (fds[0].revents & libc::POLLIN) != 0 {
                    let n = unsafe {
                        libc::read(
                            host.tty_fd(),
                            buf.as_mut_ptr() as *mut libc::c_void,
                            buf.len(),
                        )
                    };
                    if n > 0 {
                        pty.write_all(&buf[..n as usize])?;
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
                    } else if n == 0 || (fds[1].revents & libc::POLLHUP) != 0 {
                        if let Some(code) = pty.try_wait()? {
                            break code;
                        }
                    }
                }
            }

            // 3. Drain non-blocking IPC socket messages
            for ipc_msg in ipc_server.drain_messages() {
                let tx = compositor.step(Event::Ipc(ipc_msg), loop_now);
                Self::apply_os_transition(&tx, &mut host, &mut pty, &cmd_tx)?;
            }

            // 4. Drain completed async periodic command outputs
            while let Ok((id, output)) = cmd_rx.try_recv() {
                let tx = compositor.step(Event::CommandOutput { id, output }, loop_now);
                Self::apply_os_transition(&tx, &mut host, &mut pty, &cmd_tx)?;
            }

            // 5. Periodic tick (every 250ms) for clock, CWD, Git branch, and command scheduling
            if loop_now.saturating_duration_since(last_tick) >= Duration::from_millis(250) {
                last_tick = loop_now;
                let builtins = probe_builtins(
                    pty.master_fd,
                    pty.child_pid,
                    &mut cached_battery,
                    &mut last_battery_probe,
                    loop_now,
                );
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

    /// Sends a single `IpcMessage` over the Unix domain socket to a running `cth` session.
    pub fn send_ipc_message(
        socket_path: Option<PathBuf>,
        msg: &IpcMessage,
    ) -> Result<(), SessionError> {
        let path = match socket_path.or_else(|| std::env::var("CTH_SOCKET").ok().map(PathBuf::from))
        {
            Some(p) => p,
            None => {
                return Err(SessionError::Ipc(
                    "CTH_SOCKET is not set and no --socket path was provided".to_string(),
                ));
            }
        };

        let mut stream = UnixStream::connect(&path)?;
        let mut payload =
            serde_json::to_string(msg).map_err(|e| SessionError::Ipc(e.to_string()))?;
        payload.push('\n');
        stream.write_all(payload.as_bytes())?;
        stream.flush()?;
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

/// Deterministic in-memory test harness satisfying the internal runtime seam.
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
    fn new(config: Config, host_size: TermSize) -> Result<Self, SessionError> {
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
    pub fn stub_command_output(&mut self, id: impl Into<String>, output: impl Into<String>) -> &mut Self {
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
        self.emitted_ansi.extend_from_slice(&tx.host_ansi_diff);
    }
}

// ============================================================================
// Internal OS Adapters (Private to `session` module)
// ============================================================================

struct OsHostTerminal {
    tty_file: fs::File,
    orig_termios: libc::termios,
}

impl OsHostTerminal {
    fn enter() -> io::Result<Self> {
        let tty_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")?;
        let fd = tty_file.as_raw_fd();

        let mut orig_termios: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut orig_termios) } != 0 {
            return Err(io::Error::last_os_error());
        }

        let mut raw = orig_termios;
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }

        // Install SIGWINCH handler
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handle_sigwinch as *const () as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = libc::SA_RESTART;
            libc::sigaction(libc::SIGWINCH, &sa, std::ptr::null_mut());
        }

        let mut host = Self {
            tty_file,
            orig_termios,
        };
        // Enter outer alternate screen buffer and clear screen
        host.write_diff(b"\x1b[?1049h\x1b[2J\x1b[H")?;
        Ok(host)
    }

    fn tty_fd(&self) -> libc::c_int {
        self.tty_file.as_raw_fd()
    }

    fn size(&self) -> io::Result<TermSize> {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(self.tty_fd(), libc::TIOCGWINSZ, &mut ws) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(TermSize {
            cols: ws.ws_col.max(1),
            rows: ws.ws_row.max(1),
        })
    }

    fn write_diff(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.tty_file.write_all(bytes)?;
        self.tty_file.flush()
    }
}

impl Drop for OsHostTerminal {
    fn drop(&mut self) {
        let _ = self
            .tty_file
            .write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = self.tty_file.flush();
        unsafe {
            libc::tcsetattr(self.tty_fd(), libc::TCSANOW, &self.orig_termios);
        }
    }
}

struct OsChildPty {
    master_fd: libc::c_int,
    child_pid: libc::pid_t,
    exited: Option<i32>,
}

impl OsChildPty {
    fn spawn(shell: &str, initial_size: TermSize, socket_path: &Path) -> io::Result<Self> {
        let mut master_fd: libc::c_int = -1;
        let mut ws = libc::winsize {
            ws_row: initial_size.rows,
            ws_col: initial_size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        let pid = unsafe {
            libc::forkpty(
                &mut master_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut ws,
            )
        };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }

        if pid == 0 {
            // Child process: set CTH_SOCKET and exec the user's shell
            unsafe {
                std::env::set_var("CTH_SOCKET", socket_path.as_os_str());
                std::env::set_var("CTH_ACTIVE", "1");
            }
            let c_shell = CString::new(shell).unwrap_or_else(|_| CString::new("/bin/sh").unwrap());
            unsafe {
                libc::execlp(c_shell.as_ptr(), c_shell.as_ptr(), std::ptr::null::<libc::c_char>());
                libc::_exit(127);
            }
        }

        // Set master_fd to non-blocking
        unsafe {
            let flags = libc::fcntl(master_fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }

        Ok(Self {
            master_fd,
            child_pid: pid,
            exited: None,
        })
    }

    fn resize(&mut self, size: TermSize) -> io::Result<()> {
        let ws = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(self.master_fd, libc::TIOCSWINSZ, &ws) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let n = unsafe {
                libc::write(
                    self.master_fd,
                    bytes.as_ptr() as *const libc::c_void,
                    bytes.len(),
                )
            };
            if n > 0 {
                bytes = &bytes[n as usize..];
            } else {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
        }
        Ok(())
    }

    fn try_wait(&mut self) -> io::Result<Option<i32>> {
        if let Some(code) = self.exited {
            return Ok(Some(code));
        }
        let mut status: libc::c_int = 0;
        let res = unsafe { libc::waitpid(self.child_pid, &mut status, libc::WNOHANG) };
        if res == self.child_pid {
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else if libc::WIFSIGNALED(status) {
                128 + libc::WTERMSIG(status)
            } else {
                0
            };
            self.exited = Some(code);
            Ok(Some(code))
        } else if res == 0 {
            Ok(None)
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Drop for OsChildPty {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.master_fd);
        }
        if self.exited.is_none() {
            unsafe {
                libc::kill(self.child_pid, libc::SIGHUP);
            }
        }
    }
}

struct OsIpcServer {
    listener: UnixListener,
    socket_path: PathBuf,
}

impl OsIpcServer {
    fn bind(path: &Path) -> io::Result<Self> {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            socket_path: path.to_path_buf(),
        })
    }

    fn drain_messages(&self) -> Vec<IpcMessage> {
        let mut messages = Vec::new();
        while let Ok((stream, _)) = self.listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(20)));
            let reader = BufReader::new(stream);
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(msg) = serde_json::from_str::<IpcMessage>(line.trim()) {
                    messages.push(msg);
                }
            }
        }
        messages
    }
}

impl Drop for OsIpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn probe_builtins(
    pty_master_fd: libc::c_int,
    child_pid: libc::pid_t,
    cached_battery: &mut Option<String>,
    last_battery_probe: &mut Option<Instant>,
    now: Instant,
) -> BuiltinValues {
    let fg_pid = unsafe {
        let pgrp = libc::tcgetpgrp(pty_master_fd);
        if pgrp > 0 { pgrp } else { child_pid }
    };

    let raw_cwd = read_process_cwd(fg_pid).or_else(|| read_process_cwd(child_pid));
    let git_branch = raw_cwd.as_deref().and_then(resolve_git_branch);
    let cwd = raw_cwd.map(|p| shorten_home(&p));

    let should_refresh_battery = last_battery_probe
        .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_secs(30));
    if should_refresh_battery {
        *last_battery_probe = Some(now);
        *cached_battery = probe_battery();
    }

    BuiltinValues {
        cwd,
        git_branch,
        battery: cached_battery.clone(),
    }
}

#[cfg(target_os = "macos")]
fn read_process_cwd(pid: libc::pid_t) -> Option<PathBuf> {
    const PROC_PIDVNODEPATHINFO: libc::c_int = 9;
    #[repr(C)]
    struct VnodeInfoPath {
        _opaque: [u8; 152],
        vip_path: [u8; 1024],
    }
    #[repr(C)]
    struct ProcVnodePathInfo {
        pvi_cdir: VnodeInfoPath,
        _pvi_rdir: VnodeInfoPath,
    }

    unsafe extern "C" {
        fn proc_pidinfo(
            pid: libc::c_int,
            flavor: libc::c_int,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }

    let mut info: ProcVnodePathInfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<ProcVnodePathInfo>() as libc::c_int;
    let ret = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if ret <= 0 {
        return None;
    }
    let bytes = &info.pvi_cdir.vip_path;
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    if len == 0 {
        return None;
    }
    let s = std::str::from_utf8(&bytes[..len]).ok()?;
    Some(PathBuf::from(s))
}

#[cfg(not(target_os = "macos"))]
fn read_process_cwd(pid: libc::pid_t) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

fn shorten_home(path: &Path) -> String {
    if let Ok(home) = std::env::var("HOME") {
        let home_path = Path::new(&home);
        if let Ok(stripped) = path.strip_prefix(home_path) {
            if stripped.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", stripped.display());
        }
    }
    path.display().to_string()
}

fn resolve_git_branch(start_dir: &Path) -> Option<String> {
    let mut current = Some(start_dir);
    while let Some(dir) = current {
        let dot_git = dir.join(".git");
        let head_path = if dot_git.is_dir() {
            Some(dot_git.join("HEAD"))
        } else if dot_git.is_file() {
            // Git worktree or submodule `.git` file pointing to `gitdir: ...`
            let content = fs::read_to_string(&dot_git).ok()?;
            let gitdir = content.strip_prefix("gitdir:")?.trim();
            let resolved = dir.join(gitdir);
            Some(resolved.join("HEAD"))
        } else {
            None
        };

        if let Some(head_file) = head_path
            && let Ok(head_contents) = fs::read_to_string(head_file)
        {
            let trimmed = head_contents.trim();
            if let Some(branch) = trimmed.strip_prefix("ref: refs/heads/") {
                return Some(format!("git:{branch}"));
            }
            if trimmed.len() >= 7 {
                return Some(format!("git:{}", &trimmed[..7]));
            }
        }
        current = dir.parent();
    }
    None
}

fn probe_battery() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("pmset")
            .args(["-g", "batt"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        for token in text.split_whitespace() {
            let cleaned = token.trim_end_matches(';');
            if cleaned.ends_with('%') {
                return Some(format!("🔋{cleaned}"));
            }
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        let cap = fs::read_to_string("/sys/class/power_supply/BAT0/capacity").ok()?;
        Some(format!("🔋{}%", cap.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let sock_path =
            std::env::temp_dir().join(format!("cth-test-{}.sock", std::process::id()));
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
