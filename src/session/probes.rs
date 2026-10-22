use crate::compositor::BuiltinValues;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Stateful OS telemetry collector that caches battery status and samples foreground process
/// CWD, Git branch, and command name into `BuiltinValues`.
#[derive(Debug, Default)]
pub(crate) struct SystemProbes {
    cached_battery: Option<String>,
    last_battery_probe: Option<Instant>,
}

impl SystemProbes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Samples the child PTY's foreground process group for CWD, Git branch, foreground command,
    /// and cached battery status.
    pub fn sample(
        &mut self,
        pty_master_fd: libc::c_int,
        child_pid: libc::pid_t,
        now: Instant,
    ) -> BuiltinValues {
        let fg_pid = unsafe {
            let pgrp = libc::tcgetpgrp(pty_master_fd);
            if pgrp > 0 { pgrp } else { child_pid }
        };

        let raw_cwd = read_process_cwd(fg_pid).or_else(|| read_process_cwd(child_pid));
        let git_branch = raw_cwd.as_deref().and_then(resolve_git_branch);
        let cwd = raw_cwd.map(|p| shorten_home(&p));
        let fg_process = if fg_pid != child_pid {
            read_process_name(fg_pid)
        } else {
            None
        };

        let should_refresh_battery = self
            .last_battery_probe
            .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_secs(30));
        if should_refresh_battery {
            self.last_battery_probe = Some(now);
            self.cached_battery = probe_battery();
        }

        BuiltinValues {
            cwd,
            git_branch,
            battery: self.cached_battery.clone(),
            fg_process,
        }
    }
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
                return Some(format!("BAT {cleaned}"));
            }
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        let cap = fs::read_to_string("/sys/class/power_supply/BAT0/capacity").ok()?;
        Some(format!("BAT {}%", cap.trim()))
    }
}
