use crate::ui::TermSize;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static SIGWINCH_RECEIVED: AtomicBool = AtomicBool::new(false);
static SIGTERM_RECEIVED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigwinch(_sig: libc::c_int) {
    SIGWINCH_RECEIVED.store(true, Ordering::SeqCst);
}

extern "C" fn handle_sigterm(_sig: libc::c_int) {
    SIGTERM_RECEIVED.store(true, Ordering::SeqCst);
}

/// Atomically takes and resets the `SIGWINCH` signal flag.
pub(crate) fn take_sigwinch() -> bool {
    SIGWINCH_RECEIVED.swap(false, Ordering::SeqCst)
}

/// Atomically takes and resets the termination signal flag (`SIGTERM`, `SIGHUP`, `SIGINT`).
pub(crate) fn take_sigterm() -> bool {
    SIGTERM_RECEIVED.swap(false, Ordering::SeqCst)
}

/// Returns the TTY device name associated with `fd`, or None if not a terminal device.
pub(crate) fn get_tty_name(fd: libc::c_int) -> Option<String> {
    let ptr = unsafe { libc::ttyname(fd) };
    if ptr.is_null() {
        return None;
    }
    let c_str = unsafe { std::ffi::CStr::from_ptr(ptr) };
    c_str.to_str().ok().map(|s| s.to_string())
}

/// OS adapter managing raw termios mode, `SIGWINCH` registration, and alternate screen buffer
/// on the outer host terminal (`STDIN_FILENO` / `STDOUT_FILENO`).
pub(crate) struct OsHostTerminal {
    stdin_fd: libc::c_int,
    stdout: io::Stdout,
    orig_termios: libc::termios,
}

impl OsHostTerminal {
    pub fn enter() -> io::Result<Self> {
        let stdin_fd = libc::STDIN_FILENO;

        let mut orig_termios: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(stdin_fd, &mut orig_termios) } != 0 {
            return Err(io::Error::last_os_error());
        }

        let mut raw = orig_termios;
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        if unsafe { libc::tcsetattr(stdin_fd, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }

        // Install SIGWINCH and graceful termination handlers (SIGTERM, SIGHUP, SIGINT)
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handle_sigwinch as *const () as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = libc::SA_RESTART;
            libc::sigaction(libc::SIGWINCH, &sa, std::ptr::null_mut());

            let mut term_sa: libc::sigaction = std::mem::zeroed();
            term_sa.sa_sigaction = handle_sigterm as *const () as usize;
            libc::sigemptyset(&mut term_sa.sa_mask);
            term_sa.sa_flags = libc::SA_RESTART;
            libc::sigaction(libc::SIGTERM, &term_sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGHUP, &term_sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGINT, &term_sa, std::ptr::null_mut());
        }

        let mut host = Self {
            stdin_fd,
            stdout: io::stdout(),
            orig_termios,
        };
        // Enter outer alternate screen buffer, enable SGR mouse wheel/button tracking, and clear screen
        host.write_diff(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[2J\x1b[H")?;
        Ok(host)
    }

    pub fn tty_fd(&self) -> libc::c_int {
        self.stdin_fd
    }

    pub fn size(&self) -> io::Result<TermSize> {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(self.stdin_fd, libc::TIOCGWINSZ, &mut ws) } != 0
            && unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(TermSize {
            cols: ws.ws_col.max(1),
            rows: ws.ws_row.max(1),
        })
    }

    pub fn write_diff(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut lock = self.stdout.lock();
        lock.write_all(bytes)?;
        lock.flush()
    }
}

impl Drop for OsHostTerminal {
    fn drop(&mut self) {
        // Also undo child modes mirrored to the host (bracketed paste, cursor keys, keypad).
        let _ = self.write_diff(
            b"\x1b[0m\x1b[?25h\x1b[?2004l\x1b[?1l\x1b>\x1b[?1006l\x1b[?1000l\x1b[?1049l",
        );
        unsafe {
            libc::tcsetattr(self.stdin_fd, libc::TCSANOW, &self.orig_termios);
        }
    }
}
