use crate::compositor::BuiltinValues;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// How long after a line is submitted at the shell prompt a sample may treat the command as
/// finished without having seen it in the foreground.
const SUBMIT_SETTLE: Duration = Duration::from_millis(100);

/// Decides when a foreground command finished, including commands too short to be seen in
/// the foreground between two samples.
#[derive(Debug, Default)]
struct CommandCompletion {
    last_fg_running: bool,
    pending_submit: Option<Instant>,
}

impl CommandCompletion {
    /// `fg_running`: a process other than the shell holds the PTY foreground.
    /// `submitted_at`: when a line was last submitted, if one was since the previous sample.
    fn observe(&mut self, fg_running: bool, submitted_at: Option<Instant>, now: Instant) -> bool {
        if submitted_at.is_some() {
            self.pending_submit = submitted_at;
        }
        let observed = self.last_fg_running && !fg_running;
        self.last_fg_running = fg_running;
        if fg_running {
            // The command is visible; its end will be observed directly.
            self.pending_submit = None;
            return false;
        }
        let settled = self
            .pending_submit
            .is_some_and(|at| now.saturating_duration_since(at) >= SUBMIT_SETTLE);
        if observed || settled {
            self.pending_submit = None;
        }
        observed || settled
    }
}

/// Stateful OS telemetry collector that caches battery status, tracks CPU tick deltas,
/// and samples foreground process CWD, Git branch, `when_files` markers, and command name
/// into `BuiltinValues`.
#[derive(Debug)]
pub(crate) struct SystemProbes {
    last_raw_cwd: Option<PathBuf>,
    completion: CommandCompletion,
    git_head: Option<PathBuf>,
    git_head_mtime: Option<SystemTime>,
    cached_git_branch: Option<String>,
    watched_markers: BTreeSet<String>,
    cached_found_markers: BTreeSet<String>,
    cached_battery: Option<u8>,
    last_battery_probe: Option<Instant>,
    battery_in_flight: Arc<AtomicBool>,
    battery_rx: Option<Receiver<Option<u8>>>,
    prev_cpu_ticks: Option<[u64; 4]>,
    last_cpu_probe: Option<Instant>,
}

impl Default for SystemProbes {
    fn default() -> Self {
        Self {
            last_raw_cwd: None,
            completion: CommandCompletion::default(),
            git_head: None,
            git_head_mtime: None,
            cached_git_branch: None,
            watched_markers: BTreeSet::new(),
            cached_found_markers: BTreeSet::new(),
            cached_battery: None,
            last_battery_probe: None,
            battery_in_flight: Arc::new(AtomicBool::new(false)),
            battery_rx: None,
            prev_cpu_ticks: None,
            last_cpu_probe: None,
        }
    }
}

impl SystemProbes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolves the raw working directory of the child PTY's foreground process group (falling back to the shell PID).
    pub fn resolve_pty_cwd(pty_master_fd: libc::c_int, child_pid: libc::pid_t) -> Option<PathBuf> {
        let fg_pid = unsafe {
            let pgrp = libc::tcgetpgrp(pty_master_fd);
            if pgrp > 0 { pgrp } else { child_pid }
        };
        read_process_cwd(fg_pid).or_else(|| read_process_cwd(child_pid))
    }

    /// Samples the child PTY's foreground process group for CWD, Git branch, foreground command,
    /// whether a command finished, which of `watched_markers` exist in the CWD or its ancestors,
    /// cached battery status, and system CPU load percentage (`0..=100`).
    ///
    /// `line_submitted_at` is when the user last pressed Enter, if since the previous sample;
    /// it lets commands that start and finish between two samples count as finished.
    ///
    /// The Git branch is re-read when its `HEAD` file changes (one `stat` per sample); the
    /// repository and markers are looked up again only when the CWD changes, a command
    /// finishes, or the watched marker set changes.
    pub fn sample(
        &mut self,
        pty_master_fd: libc::c_int,
        child_pid: libc::pid_t,
        now: Instant,
        watched_markers: &BTreeSet<String>,
        line_submitted_at: Option<Instant>,
    ) -> BuiltinValues {
        let fg_pid = unsafe {
            let pgrp = libc::tcgetpgrp(pty_master_fd);
            if pgrp > 0 { pgrp } else { child_pid }
        };

        let raw_cwd = read_process_cwd(fg_pid).or_else(|| read_process_cwd(child_pid));
        let fg_process = if fg_pid != child_pid {
            read_process_name(fg_pid)
        } else {
            None
        };
        let command_finished = self
            .completion
            .observe(fg_pid != child_pid, line_submitted_at, now);
        self.refresh_cwd_derived(raw_cwd.clone(), command_finished, watched_markers);
        let cwd = raw_cwd.map(|p| shorten_home(&p));

        // Drain any async battery sample from the background worker thread
        if let Some(ref rx) = self.battery_rx {
            while let Ok(val) = rx.try_recv() {
                self.cached_battery = val;
            }
        }

        let should_refresh_battery = self
            .last_battery_probe
            .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_secs(30));
        if should_refresh_battery && !self.battery_in_flight.load(Ordering::Relaxed) {
            self.last_battery_probe = Some(now);
            self.battery_in_flight.store(true, Ordering::Relaxed);
            let in_flight = Arc::clone(&self.battery_in_flight);
            let (tx, rx) = mpsc::channel();
            self.battery_rx = Some(rx);
            thread::spawn(move || {
                let batt = probe_battery();
                let _ = tx.send(batt);
                in_flight.store(false, Ordering::Relaxed);
            });
        }

        let cpu_usage = self.sample_cpu(now);

        BuiltinValues {
            cwd,
            git_branch: self.cached_git_branch.clone(),
            battery: self.cached_battery,
            fg_process,
            command_finished,
            cpu_usage,
            found_markers: self.cached_found_markers.clone(),
        }
    }

    fn refresh_cwd_derived(
        &mut self,
        raw_cwd: Option<PathBuf>,
        command_finished: bool,
        watched_markers: &BTreeSet<String>,
    ) {
        let relocate = raw_cwd != self.last_raw_cwd || command_finished;
        if relocate {
            self.git_head = raw_cwd.as_deref().and_then(find_git_head);
        }
        let head_mtime = self.git_head.as_deref().and_then(modified_time);
        if relocate || head_mtime != self.git_head_mtime {
            self.cached_git_branch = self.git_head.as_deref().and_then(read_git_branch);
            self.git_head_mtime = head_mtime;
        }
        if relocate || *watched_markers != self.watched_markers {
            self.cached_found_markers = raw_cwd
                .as_deref()
                .map(|dir| find_markers(dir, watched_markers))
                .unwrap_or_default();
            self.watched_markers = watched_markers.clone();
        }
        self.last_raw_cwd = raw_cwd;
    }

    /// Samples system-wide CPU usage percentage at ~1s intervals (plus an initial baseline on frame 1).
    fn sample_cpu(&mut self, now: Instant) -> Option<u8> {
        let is_due = self
            .last_cpu_probe
            .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_millis(900));
        if !is_due {
            return None;
        }
        self.last_cpu_probe = Some(now);

        let cur = read_cpu_ticks()?;
        let pct = match self.prev_cpu_ticks {
            Some(prev) => {
                let d_user = cur[0].saturating_sub(prev[0]);
                let d_sys = cur[1].saturating_sub(prev[1]);
                let d_idle = cur[2].saturating_sub(prev[2]);
                let d_nice = cur[3].saturating_sub(prev[3]);
                let busy = d_user + d_sys + d_nice;
                let total = busy + d_idle;
                (busy * 100).checked_div(total).unwrap_or(0).min(100) as u8
            }
            None => 0,
        };
        self.prev_cpu_ticks = Some(cur);
        Some(pct)
    }
}

#[cfg(target_os = "macos")]
fn read_cpu_ticks() -> Option<[u64; 4]> {
    const HOST_CPU_LOAD_INFO: libc::c_int = 3;
    const HOST_CPU_LOAD_INFO_COUNT: u32 = 4;

    #[repr(C)]
    struct HostCpuLoadInfo {
        cpu_ticks: [u32; 4],
    }

    unsafe extern "C" {
        fn mach_host_self() -> u32;
        fn host_statistics(
            host_priv: u32,
            flavor: libc::c_int,
            host_info_out: *mut HostCpuLoadInfo,
            host_info_out_cnt: *mut u32,
        ) -> libc::c_int;
    }

    let mut info = HostCpuLoadInfo { cpu_ticks: [0; 4] };
    let mut count = HOST_CPU_LOAD_INFO_COUNT;
    let kr =
        unsafe { host_statistics(mach_host_self(), HOST_CPU_LOAD_INFO, &mut info, &mut count) };
    if kr != 0 {
        return None;
    }
    Some([
        u64::from(info.cpu_ticks[0]),
        u64::from(info.cpu_ticks[1]),
        u64::from(info.cpu_ticks[2]),
        u64::from(info.cpu_ticks[3]),
    ])
}

#[cfg(not(target_os = "macos"))]
fn read_cpu_ticks() -> Option<[u64; 4]> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().next()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "cpu" {
        return None;
    }
    let user: u64 = parts.next()?.parse().ok()?;
    let nice: u64 = parts.next()?.parse().ok()?;
    let sys: u64 = parts.next()?.parse().ok()?;
    let idle: u64 = parts.next()?.parse().ok()?;
    Some([user, sys, idle, nice])
}

/// Returns the names in `markers` that exist in `start_dir` or any of its ancestors.
fn find_markers(start_dir: &Path, markers: &BTreeSet<String>) -> BTreeSet<String> {
    markers
        .iter()
        .filter(|name| start_dir.ancestors().any(|dir| dir.join(name).exists()))
        .cloned()
        .collect()
}

fn modified_time(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

/// Finds the `HEAD` file of the Git repository (or worktree) containing `start_dir`.
fn find_git_head(start_dir: &Path) -> Option<PathBuf> {
    start_dir.ancestors().find_map(|dir| {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            Some(dot_git.join("HEAD"))
        } else if dot_git.is_file() {
            let content = fs::read_to_string(&dot_git).ok()?;
            let gitdir = content.strip_prefix("gitdir:")?.trim();
            Some(dir.join(gitdir).join("HEAD"))
        } else {
            None
        }
    })
}

/// Reads the branch name (or short detached commit) from a `HEAD` file as `git:<name>`.
fn read_git_branch(head: &Path) -> Option<String> {
    let contents = fs::read_to_string(head).ok()?;
    let trimmed = contents.trim();
    if let Some(branch) = trimmed.strip_prefix("ref: refs/heads/") {
        return Some(format!("git:{branch}"));
    }
    (trimmed.len() >= 7).then(|| format!("git:{}", &trimmed[..7]))
}

#[cfg(target_os = "macos")]
fn read_process_name(pid: libc::pid_t) -> Option<String> {
    unsafe extern "C" {
        fn proc_name(pid: libc::c_int, buffer: *mut libc::c_void, buffersize: u32) -> libc::c_int;
    }
    let mut buf = [0u8; 256];
    let ret = unsafe { proc_name(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
    if ret <= 0 {
        return None;
    }
    let len = (ret as usize).min(buf.len());
    let name = std::str::from_utf8(&buf[..len])
        .ok()?
        .trim_matches('\0')
        .trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

#[cfg(not(target_os = "macos"))]
fn read_process_name(pid: libc::pid_t) -> Option<String> {
    let comm = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let trimmed = comm.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
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

fn probe_battery() -> Option<u8> {
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
            if let Some(num_str) = cleaned.strip_suffix('%')
                && let Ok(pct) = num_str.parse::<u8>()
            {
                return Some(pct.min(100));
            }
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        let cap = fs::read_to_string("/sys/class/power_supply/BAT0/capacity").ok()?;
        let pct = cap.trim().parse::<u8>().ok()?;
        Some(pct.min(100))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rewrites `HEAD` with a distinct mtime so the change is visible even within one tick.
    fn write_head(root: &Path, contents: &str, mtime_secs: u64) {
        let head = root.join(".git/HEAD");
        fs::write(&head, contents).unwrap();
        fs::File::options()
            .write(true)
            .open(&head)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs))
            .unwrap();
    }

    fn temp_repo(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("thud-{name}-{}", std::process::id()));
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("crate")).unwrap();
        root
    }

    #[test]
    fn follows_head_changes_without_a_command_finishing() {
        let root = temp_repo("probe-head");
        let nested = root.join("crate");
        write_head(&root, "ref: refs/heads/main\n", 1_000);
        let none = BTreeSet::new();

        let mut probes = SystemProbes::new();
        probes.refresh_cwd_derived(Some(nested.clone()), false, &none);
        assert_eq!(probes.cached_git_branch.as_deref(), Some("git:main"));

        // e.g. `git switch` finished between two samples, or ran in another terminal / IDE
        write_head(&root, "ref: refs/heads/next\n", 2_000);
        probes.refresh_cwd_derived(Some(nested.clone()), false, &none);
        assert_eq!(probes.cached_git_branch.as_deref(), Some("git:next"));

        write_head(&root, "0123456789abcdef0123456789abcdef01234567\n", 3_000);
        probes.refresh_cwd_derived(Some(nested), false, &none);
        assert_eq!(probes.cached_git_branch.as_deref(), Some("git:0123456"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rechecks_repository_and_markers_on_cwd_change_or_command_finish() {
        let root = temp_repo("probe-markers");
        let nested = root.join("crate");
        let outside = std::env::temp_dir().join(format!("thud-probe-out-{}", std::process::id()));
        fs::create_dir_all(&outside).unwrap();
        write_head(&root, "ref: refs/heads/main\n", 1_000);
        let watched: BTreeSet<String> = ["Cargo.toml".to_string()].into();

        let mut probes = SystemProbes::new();
        probes.refresh_cwd_derived(Some(nested.clone()), false, &watched);
        assert!(probes.cached_found_markers.is_empty());

        // A command creates a marker: picked up only once it finishes.
        fs::write(nested.join("Cargo.toml"), "").unwrap();
        probes.refresh_cwd_derived(Some(nested.clone()), false, &watched);
        assert!(probes.cached_found_markers.is_empty());
        probes.refresh_cwd_derived(Some(nested.clone()), true, &watched);
        assert_eq!(probes.cached_found_markers, watched);

        // `git init` outside a repository is found once the command finishes.
        probes.refresh_cwd_derived(Some(outside.clone()), false, &watched);
        assert_eq!(probes.cached_git_branch, None);
        assert!(probes.cached_found_markers.is_empty());
        fs::create_dir_all(outside.join(".git")).unwrap();
        fs::write(outside.join(".git/HEAD"), "ref: refs/heads/trunk\n").unwrap();
        probes.refresh_cwd_derived(Some(outside.clone()), true, &watched);
        assert_eq!(probes.cached_git_branch.as_deref(), Some("git:trunk"));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn detects_commands_too_short_to_be_sampled_in_the_foreground() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut completion = CommandCompletion::default();

        // Enter, command runs and exits between samples
        assert!(
            !completion.observe(false, Some(ms(0)), ms(40)),
            "too early to tell"
        );
        assert!(completion.observe(false, None, ms(290)));
        assert!(!completion.observe(false, None, ms(540)), "reported once");

        // A command seen in the foreground is reported when it leaves
        assert!(!completion.observe(false, Some(ms(600)), ms(610)));
        assert!(!completion.observe(true, None, ms(860)));
        assert!(completion.observe(false, None, ms(1110)));
        assert!(!completion.observe(false, None, ms(1360)));

        // Enter inside a full-screen app does not count when it is still running
        assert!(!completion.observe(true, Some(ms(1400)), ms(1610)));
        assert!(!completion.observe(true, None, ms(1860)));
    }
}
