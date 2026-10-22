use crate::ui::TermSize;
use std::ffi::CString;
use std::io;
use std::path::Path;

/// OS adapter managing the child pseudo-terminal (`forkpty`), viewport resize (`TIOCSWINSZ`),
/// non-blocking I/O, and process wait/cleanup.
pub(crate) struct OsChildPty {
    pub master_fd: libc::c_int,
    pub child_pid: libc::pid_t,
    exited: Option<i32>,
}

impl OsChildPty {
    pub fn spawn(shell: &str, initial_size: TermSize, socket_path: &Path) -> io::Result<Self> {
        let mut master_fd: libc::c_int = -1;
        #[allow(unused_mut)]
        let mut ws = libc::winsize {
            ws_row: initial_size.rows,
            ws_col: initial_size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        #[cfg(target_os = "macos")]
        let winp = &mut ws;
        #[cfg(not(target_os = "macos"))]
        let winp = &ws;

        let pid = unsafe {
            libc::forkpty(
                &mut master_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                winp,
            )
        };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }

        if pid == 0 {
            // Child process: describe this session through THUD_SOCKET, THUD_ACTIVE, and
            // THUD_TTY (removed when the host TTY has no name) and exec the user's shell
            unsafe {
                std::env::set_var("THUD_SOCKET", socket_path.as_os_str());
                std::env::set_var("THUD_ACTIVE", "1");
                match crate::session::tty::get_tty_name(libc::STDIN_FILENO) {
                    Some(tty) => std::env::set_var("THUD_TTY", tty),
                    None => std::env::remove_var("THUD_TTY"),
                }
            }
            let c_shell = CString::new(shell).unwrap_or_else(|_| CString::new("/bin/sh").unwrap());
            unsafe {
                libc::execlp(
                    c_shell.as_ptr(),
                    c_shell.as_ptr(),
                    std::ptr::null::<libc::c_char>(),
                );
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

    pub fn resize(&mut self, size: TermSize) -> io::Result<()> {
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

    pub fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
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

    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
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
