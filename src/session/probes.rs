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
use std::time::{Duration, Instant};

/// Stateful OS telemetry collector that caches battery status, tracks CPU tick deltas,
/// and samples foreground process CWD, Git branch, `when_files` markers, and command name
/// into `BuiltinValues`.
#[derive(Debug)]
pub(crate) struct SystemProbes {
    last_raw_cwd: Option<PathBuf>,
    last_fg_process: Option<String>,
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
            last_fg_process: None,
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
    /// which of `watched_markers` exist in the CWD or its ancestors, cached battery status, and
    /// system CPU load percentage (`0..=100`).
    ///
    /// Git branch and markers touch the filesystem only when the CWD changes, a foreground
    /// command finishes, or the watched marker set changes.
    pub fn sample(
        &mut self,
        pty_master_fd: libc::c_int,
        child_pid: libc::pid_t,
        now: Instant,
        watched_markers: &BTreeSet<String>,
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
        self.refresh_cwd_derived(raw_cwd.clone(), fg_process.clone(), watched_markers);
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
            cpu_usage,
            found_markers: self.cached_found_markers.clone(),
        }
    }

    fn refresh_cwd_derived(
        &mut self,
        raw_cwd: Option<PathBuf>,
        fg_process: Option<String>,
        watched_markers: &BTreeSet<String>,
    ) {
        let cwd_changed = raw_cwd != self.last_raw_cwd;
        let cmd_finished = self.last_fg_process.is_some() && fg_process.is_none();
        if cwd_changed || cmd_finished {
            self.cached_git_branch = raw_cwd.as_deref().and_then(resolve_git_branch);
        }
        if cwd_changed || cmd_finished || *watched_markers != self.watched_markers {
            self.cached_found_markers = raw_cwd
                .as_deref()
                .map(|dir| find_markers(dir, watched_markers))
                .unwrap_or_default();
            self.watched_markers = watched_markers.clone();
        }
        self.last_raw_cwd = raw_cwd;
        self.last_fg_process = fg_process;
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

pub(crate) fn resolve_git_branch(start_dir: &Path) -> Option<String> {
    let mut current = Some(start_dir);
    while let Some(dir) = current {
        let dot_git = dir.join(".git");
        let head_path = if dot_git.is_dir() {
            Some(dot_git.join("HEAD"))
        } else if dot_git.is_file() {
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

    #[test]
    fn rechecks_git_branch_and_markers_only_on_cwd_change_or_command_finish() {
        let root = std::env::temp_dir().join(format!("thud-probe-memo-{}", std::process::id()));
        let nested = root.join("crate");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let watched: BTreeSet<String> = ["Cargo.toml".to_string()].into();

        let mut probes = SystemProbes::new();
        probes.refresh_cwd_derived(Some(nested.clone()), None, &watched);
        assert_eq!(probes.cached_git_branch.as_deref(), Some("git:main"));
        assert!(probes.cached_found_markers.is_empty());

        // A command creates a marker and switches branch: unchanged until it finishes.
        fs::write(nested.join("Cargo.toml"), "").unwrap();
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/next\n").unwrap();
        probes.refresh_cwd_derived(Some(nested.clone()), Some("cargo".to_string()), &watched);
        assert_eq!(probes.cached_git_branch.as_deref(), Some("git:main"));
        assert!(probes.cached_found_markers.is_empty());

        probes.refresh_cwd_derived(Some(nested.clone()), None, &watched);
        assert_eq!(probes.cached_git_branch.as_deref(), Some("git:next"));
        assert_eq!(probes.cached_found_markers, watched);

        // Leaving the tree clears the markers.
        probes.refresh_cwd_derived(Some(root.clone()), None, &watched);
        assert!(probes.cached_found_markers.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn resolves_git_branch_from_ancestor_head_file() {
        let root = std::env::temp_dir().join(format!("thud-git-probe-{}", std::process::id()));
        let nested = root.join("src/deep");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(&nested).unwrap();

        fs::write(root.join(".git/HEAD"), "ref: refs/heads/feature/hud\n").unwrap();
        assert_eq!(
            resolve_git_branch(&nested).as_deref(),
            Some("git:feature/hud")
        );

        fs::write(
            root.join(".git/HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        assert_eq!(resolve_git_branch(&nested).as_deref(), Some("git:0123456"));

        let _ = fs::remove_dir_all(&root);
    }
}
