use crate::compositor::{BuiltinValues, CommandKey};
use crate::config::{Config, ConfigError, IpcMessage, TermSize};
use crate::session::io::{Readiness, SessionIo};
use crate::session::session_loop::SessionLoop;
use chrono::{DateTime, Local, TimeZone};
use std::collections::{BTreeSet, VecDeque};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A command started by `SessionLoop` through `SessionIo::spawn_command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpawnedCommand {
    pub key: CommandKey,
    pub cmd: String,
    pub cwd: Option<PathBuf>,
}

/// Deterministic in-memory `SessionIo` adapter with a virtual clock.
///
/// Inputs are only enqueued; `SessionLoop::advance` moves virtual time while running
/// `run_once`. Each enqueued host or PTY chunk is delivered as one read.
pub(crate) struct MemorySessionIo {
    start: Instant,
    now: Instant,
    deadline: Instant,
    wall_clock_start: DateTime<Local>,
    host_size: TermSize,
    host_input: VecDeque<Vec<u8>>,
    pty_output: VecDeque<Vec<u8>>,
    ipc: VecDeque<IpcMessage>,
    resized: bool,
    terminated: bool,
    child_exit: Option<i32>,
    finished_commands: VecDeque<(CommandKey, Option<String>)>,
    watched_markers: BTreeSet<String>,
    line_submitted_at: Option<Instant>,
    builtins: BuiltinValues,
    cwd: Option<PathBuf>,
    config: Result<Config, ConfigError>,
    config_changed: bool,
    host_writes: Vec<Vec<u8>>,
    child_input: Vec<u8>,
    child_size: Option<TermSize>,
    spawned: Vec<SpawnedCommand>,
    last_wait: Option<Duration>,
}

impl MemorySessionIo {
    pub fn new(cols: u16, rows: u16) -> Self {
        let now = Instant::now();
        Self {
            start: now,
            now,
            deadline: now,
            wall_clock_start: Local.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap(),
            host_size: TermSize { cols, rows },
            host_input: VecDeque::new(),
            pty_output: VecDeque::new(),
            ipc: VecDeque::new(),
            resized: false,
            terminated: false,
            child_exit: None,
            finished_commands: VecDeque::new(),
            watched_markers: BTreeSet::new(),
            line_submitted_at: None,
            builtins: BuiltinValues::default(),
            cwd: None,
            config: Ok(Config::default()),
            config_changed: false,
            host_writes: Vec::new(),
            child_input: Vec::new(),
            child_size: None,
            spawned: Vec::new(),
            last_wait: None,
        }
    }

    /// Sets the wall-clock time reported at the current virtual instant.
    pub fn set_wall_clock(&mut self, wall_clock: DateTime<Local>) -> &mut Self {
        let elapsed = chrono::Duration::from_std(self.now - self.start).unwrap_or_default();
        self.wall_clock_start = wall_clock - elapsed;
        self
    }

    pub fn set_builtins(&mut self, builtins: BuiltinValues) -> &mut Self {
        self.builtins = builtins;
        self
    }

    pub fn set_cwd(&mut self, cwd: impl Into<PathBuf>) -> &mut Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn push_host_input(&mut self, bytes: &[u8]) -> &mut Self {
        self.host_input.push_back(bytes.to_vec());
        self
    }

    pub fn push_pty_output(&mut self, bytes: &[u8]) -> &mut Self {
        self.pty_output.push_back(bytes.to_vec());
        self
    }

    pub fn push_ipc(&mut self, msg: IpcMessage) -> &mut Self {
        self.ipc.push_back(msg);
        self
    }

    pub fn resize_host(&mut self, cols: u16, rows: u16) -> &mut Self {
        self.host_size = TermSize { cols, rows };
        self.resized = true;
        self
    }

    pub fn terminate(&mut self) -> &mut Self {
        self.terminated = true;
        self
    }

    pub fn exit_child(&mut self, code: i32) -> &mut Self {
        self.child_exit = Some(code);
        self
    }

    /// Replaces the config source contents and marks it as changed.
    pub fn change_config(&mut self, config: Result<Config, ConfigError>) -> &mut Self {
        self.set_config(config);
        self.config_changed = true;
        self
    }

    /// Replaces what the next `load_config` returns without marking the source as changed.
    pub fn set_config(&mut self, config: Result<Config, ConfigError>) -> &mut Self {
        self.config = config;
        self
    }

    pub fn complete_command(&mut self, key: CommandKey, output: &str) -> &mut Self {
        self.finished_commands
            .push_back((key, Some(output.to_string())));
        self
    }

    pub fn fail_command(&mut self, key: CommandKey) -> &mut Self {
        self.finished_commands.push_back((key, None));
        self
    }

    /// The submit time the loop last handed to `sample_builtins`, if any.
    pub fn line_submitted_at(&self) -> Option<Instant> {
        self.line_submitted_at
    }

    /// Marker names the loop last asked `sample_builtins` to look for.
    pub fn watched_markers(&self) -> &BTreeSet<String> {
        &self.watched_markers
    }

    pub fn take_spawned(&mut self) -> Vec<SpawnedCommand> {
        std::mem::take(&mut self.spawned)
    }

    /// Returns each `write_host` call made since the last call, in order.
    pub fn take_host_writes(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.host_writes)
    }

    pub fn take_child_input(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.child_input)
    }

    pub fn child_size(&self) -> Option<TermSize> {
        self.child_size
    }

    pub fn last_wait(&self) -> Option<Duration> {
        self.last_wait
    }

    fn has_readable_input(&self) -> bool {
        !self.host_input.is_empty() || !self.pty_output.is_empty()
    }
}

fn read_chunk(queue: &mut VecDeque<Vec<u8>>, buf: &mut [u8]) -> usize {
    let Some(mut chunk) = queue.pop_front() else {
        return 0;
    };
    let n = chunk.len().min(buf.len());
    buf[..n].copy_from_slice(&chunk[..n]);
    if n < chunk.len() {
        queue.push_front(chunk.split_off(n));
    }
    n
}

impl SessionIo for MemorySessionIo {
    fn now(&self) -> Instant {
        self.now
    }

    fn wall_clock(&self) -> DateTime<Local> {
        let elapsed = chrono::Duration::from_std(self.now - self.start).unwrap_or_default();
        self.wall_clock_start + elapsed
    }

    /// Returns immediately when input is queued; otherwise sleeps virtually for `timeout`,
    /// never past the deadline set by `SessionLoop::advance`.
    fn wait(&mut self, timeout: Duration) -> io::Result<Readiness> {
        self.last_wait = Some(timeout);
        if !self.has_readable_input() {
            let remaining = self.deadline.saturating_duration_since(self.now);
            self.now += timeout.min(remaining);
        }
        Ok(Readiness {
            host: !self.host_input.is_empty(),
            child: !self.pty_output.is_empty(),
            child_hangup: false,
        })
    }

    fn read_host(&mut self, buf: &mut [u8]) -> usize {
        read_chunk(&mut self.host_input, buf)
    }

    fn read_child(&mut self, buf: &mut [u8]) -> usize {
        read_chunk(&mut self.pty_output, buf)
    }

    fn host_size(&self) -> io::Result<TermSize> {
        Ok(self.host_size)
    }

    fn write_host(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.host_writes.push(bytes.to_vec());
        Ok(())
    }

    fn write_child(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.child_input.extend_from_slice(bytes);
        Ok(())
    }

    fn resize_child(&mut self, size: TermSize) -> io::Result<()> {
        self.child_size = Some(size);
        Ok(())
    }

    fn child_exit(&mut self) -> io::Result<Option<i32>> {
        Ok(self.child_exit)
    }

    fn take_resize(&mut self) -> bool {
        std::mem::take(&mut self.resized)
    }

    fn take_terminate(&mut self) -> bool {
        std::mem::take(&mut self.terminated)
    }

    fn drain_ipc(&mut self) -> Vec<IpcMessage> {
        self.ipc.drain(..).collect()
    }

    fn spawn_command(&mut self, key: CommandKey, cmd: String) {
        self.spawned.push(SpawnedCommand {
            key,
            cmd,
            cwd: self.cwd.clone(),
        });
    }

    fn drain_finished_commands(&mut self) -> Vec<(CommandKey, Option<String>)> {
        self.finished_commands.drain(..).collect()
    }

    fn sample_builtins(
        &mut self,
        _now: Instant,
        watched_markers: &BTreeSet<String>,
        line_submitted_at: Option<Instant>,
    ) -> BuiltinValues {
        self.watched_markers = watched_markers.clone();
        self.line_submitted_at = line_submitted_at;
        self.builtins.clone()
    }

    fn config_changed(&mut self) -> bool {
        self.config_changed
    }

    fn load_config(&mut self) -> Result<Config, ConfigError> {
        self.config_changed = false;
        self.config.clone()
    }
}

impl SessionLoop<MemorySessionIo> {
    /// Runs `run_once` until virtual time has advanced by `elapsed` and no queued host or PTY
    /// input remains. Returns the exit code if the session ended.
    pub fn advance(&mut self, elapsed: Duration) -> Option<i32> {
        let target = self.io().now + elapsed;
        self.io_mut().deadline = target;
        loop {
            if let Some(code) = self.run_once().expect("in-memory session I/O cannot fail") {
                return Some(code);
            }
            let io = self.io();
            if io.now >= target && !io.has_readable_input() {
                return None;
            }
        }
    }
}
