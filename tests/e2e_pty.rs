use libc::{c_int, c_void, pollfd, winsize, POLLIN, TIOCSWINSZ};
use std::ffi::CString;
use std::io;
use std::time::{Duration, Instant};

/// An automated "Outer Terminal Emulator" harness for AI Agents and CI.
/// Spawns the real `cth` binary inside a pseudo-terminal (`forkpty`), feeds all ANSI output
/// from `cth` into a host-side `vt100::Parser` (simulating Ghostty/WezTerm/Alacritty),
/// sends real keystrokes to `cth`'s stdin, and inspects the resulting 2D screen grid.
struct OuterTerminalSimulator {
    master_fd: c_int,
    child_pid: libc::pid_t,
    vt: vt100::Parser,
    raw_output: Vec<u8>,
}

impl OuterTerminalSimulator {
    fn spawn_cth(cols: u16, rows: u16, args: &[&str]) -> io::Result<Self> {
        let bin_path = env!("CARGO_BIN_EXE_cth");
        let mut master_fd: c_int = -1;
        let mut ws = winsize {
            ws_row: rows,
            ws_col: cols,
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
            unsafe {
                std::env::set_var("TERM", "xterm-256color");
            }
            let c_bin = CString::new(bin_path).unwrap();
            let mut c_args: Vec<CString> = Vec::with_capacity(args.len() + 1);
            c_args.push(c_bin.clone());
            for a in args {
                c_args.push(CString::new(*a).unwrap());
            }
            let mut ptrs: Vec<*const libc::c_char> = c_args.iter().map(|s| s.as_ptr()).collect();
            ptrs.push(std::ptr::null());

            unsafe {
                libc::execv(c_bin.as_ptr(), ptrs.as_ptr());
                libc::_exit(127);
            }
        }

        unsafe {
            let flags = libc::fcntl(master_fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }

        Ok(Self {
            master_fd,
            child_pid: pid,
            vt: vt100::Parser::new(rows, cols, 0),
            raw_output: Vec::new(),
        })
    }

    fn send_keys(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut rem = bytes;
        while !rem.is_empty() {
            let n = unsafe {
                libc::write(
                    self.master_fd,
                    rem.as_ptr() as *const c_void,
                    rem.len(),
                )
            };
            if n > 0 {
                rem = &rem[n as usize..];
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

    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        let ws = winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(self.master_fd, TIOCSWINSZ, &ws) } != 0 {
            return Err(io::Error::last_os_error());
        }
        unsafe {
            libc::kill(self.child_pid, libc::SIGWINCH);
        }
        self.vt.set_size(rows, cols);
        Ok(())
    }

    fn pump_until(
        &mut self,
        timeout: Duration,
        predicate: impl Fn(&vt100::Screen) -> bool,
    ) -> bool {
        let start = Instant::now();
        let mut buf = [0u8; 8192];
        while start.elapsed() < timeout {
            let mut pfd = pollfd {
                fd: self.master_fd,
                events: POLLIN,
                revents: 0,
            };
            let res = unsafe { libc::poll(&mut pfd, 1, 30) };
            if res > 0 && (pfd.revents & POLLIN) != 0 {
                loop {
                    let n = unsafe {
                        libc::read(
                            self.master_fd,
                            buf.as_mut_ptr() as *mut c_void,
                            buf.len(),
                        )
                    };
                    if n > 0 {
                        let slice = &buf[..n as usize];
                        self.raw_output.extend_from_slice(slice);
                        self.vt.process(slice);
                    } else {
                        break;
                    }
                }
            }
            if predicate(self.vt.screen()) {
                return true;
            }
        }
        predicate(self.vt.screen())
    }

    fn row_text(&self, row: u16) -> String {
        let screen = self.vt.screen();
        let (_, cols) = screen.size();
        let mut s = String::new();
        let mut c = 0;
        while c < cols {
            if let Some(cell) = screen.cell(row, c) {
                if cell.is_wide_continuation() {
                    c += 1;
                    continue;
                }
                let ch = cell.contents();
                if ch.is_empty() {
                    s.push(' ');
                } else {
                    s.push_str(&ch);
                }
                c += if cell.is_wide() { 2 } else { 1 };
            } else {
                c += 1;
            }
        }
        s
    }

    fn wait_exit(&mut self, timeout: Duration) -> Option<i32> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            self.pump_until(Duration::from_millis(30), |_| false);
            let mut status: c_int = 0;
            let res = unsafe { libc::waitpid(self.child_pid, &mut status, libc::WNOHANG) };
            if res == self.child_pid {
                if libc::WIFEXITED(status) {
                    return Some(libc::WEXITSTATUS(status));
                }
                return Some(0);
            }
        }
        None
    }
}

impl Drop for OuterTerminalSimulator {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.master_fd);
            libc::kill(self.child_pid, libc::SIGKILL);
            let mut status: c_int = 0;
            libc::waitpid(self.child_pid, &mut status, 0);
        }
    }
}

#[test]
fn e2e_outer_pty_interactive_input_status_bar_ipc_and_clean_exit() {
    let mut term = OuterTerminalSimulator::spawn_cth(80, 24, &["--shell", "/bin/sh"]).unwrap();

    // 1. Wait for status bar (row 23) and top-right clock (row 0) to render
    assert!(
        term.pump_until(Duration::from_secs(3), |screen| {
            screen.alternate_screen() && screen.contents().contains("git:")
        }),
        "timed out waiting for initial HUD frame; screen:\n{}",
        term.vt.screen().contents()
    );

    let top_row = term.row_text(0);
    let bottom_row = term.row_text(23);
    // Top-right clock has format " HH:MM:SS " at the right end of row 0
    assert!(
        top_row.trim_end().ends_with(':') || top_row.contains(':'),
        "expected top-right clock on row 0, got: {top_row:?}"
    );
    assert!(
        bottom_row.contains("git:"),
        "expected git branch on bottom status bar row 23, got: {bottom_row:?}"
    );

    // 2. Send real interactive keystrokes through outer PTY -> cth STDIN -> child shell
    let bin_path = env!("CARGO_BIN_EXE_cth");
    let cmd = format!("echo E2E_TYPING_WORKS && {bin_path} set status LIVE_IPC_OK\r");
    term.send_keys(cmd.as_bytes()).unwrap();

    assert!(
        term.pump_until(Duration::from_secs(3), |screen| {
            let contents = screen.contents();
            contents.contains("E2E_TYPING_WORKS") && contents.contains("LIVE_IPC_OK")
        }),
        "interactive typing or IPC slot update failed; screen:\n{}",
        term.vt.screen().contents()
    );

    // 3. Test dynamic window resize (SIGWINCH) from 80x24 -> 100x30
    term.resize(100, 30).unwrap();
    assert!(
        term.pump_until(Duration::from_secs(2), |_| true),
        "resize failed"
    );
    // Wait until row 29 (new bottom row) has the status bar
    assert!(
        term.pump_until(Duration::from_secs(2), |screen| {
            let mut row29 = String::new();
            for c in 0..100 {
                if let Some(cell) = screen.cell(29, c) {
                    row29.push_str(&cell.contents());
                }
            }
            row29.contains("LIVE_IPC_OK")
        }),
        "status bar did not move to row 29 after resize to 100x30; row 29: {:?}",
        term.row_text(29)
    );

    // 4. Exit shell and verify cth exits with code 0 and leaves alternate screen (?1049l)
    term.send_keys(b"exit\r").unwrap();
    let exit_code = term.wait_exit(Duration::from_secs(3));
    assert_eq!(exit_code, Some(0), "expected clean exit code 0");
    assert!(
        !term.vt.screen().alternate_screen(),
        "expected cth to restore outer terminal from alternate screen (?1049l) on exit"
    );
}
