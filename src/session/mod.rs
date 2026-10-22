pub(crate) mod io;
pub(crate) mod ipc;
#[cfg(test)]
pub(crate) mod memory_io;
pub(crate) mod probes;
pub(crate) mod pty;
pub(crate) mod session_loop;
pub(crate) mod tty;

use crate::config::{Config, ConfigError, IpcMessage};
use crate::session::io::OsSessionIo;
use crate::session::session_loop::SessionLoop;
use std::io as std_io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum SessionError {
    Config(ConfigError),
    Io(std_io::Error),
    Ipc(String),
    NestedSession,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Config(e) => write!(f, "{e}"),
            SessionError::Io(e) => write!(f, "I/O error: {e}"),
            SessionError::Ipc(msg) => write!(f, "IPC error: {msg}"),
            SessionError::NestedSession => write!(
                f,
                "cannot start a nested thud session inside an active thud session (use 'thud --force' to override, or 'exit' to leave)"
            ),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<ConfigError> for SessionError {
    fn from(e: ConfigError) -> Self {
        SessionError::Config(e)
    }
}

impl From<std_io::Error> for SessionError {
    fn from(e: std_io::Error) -> Self {
        SessionError::Io(e)
    }
}

/// Public entry point of a live session: builds the OS adapters and runs a `SessionLoop`
/// until the child shell exits or the session is told to quit.
pub struct Session;

impl Session {
    /// Production entry point: switches the host terminal to raw + alternate screen mode,
    /// spawns the child shell inside a PTY, binds the Unix domain socket for IPC,
    /// runs the session loop, and restores terminal state on exit.
    pub fn run(config: Config) -> Result<i32, SessionError> {
        Self::check_nested_session(config.force)?;
        let io = OsSessionIo::start(&config)?;
        SessionLoop::new(config, io)?.run()
    }

    /// Sends a single `IpcMessage` over the Unix domain socket to a running `thud` session.
    pub fn send_ipc_message(
        socket_path: Option<PathBuf>,
        msg: &IpcMessage,
    ) -> Result<(), SessionError> {
        ipc::send_ipc_message(socket_path, msg)
    }

    pub(crate) fn check_nested_session(force: bool) -> Result<(), SessionError> {
        let current_tty = tty::get_tty_name(libc::STDIN_FILENO);
        Self::check_nested_session_impl(force, current_tty.as_deref())
    }

    pub(crate) fn check_nested_session_impl(
        force: bool,
        current_tty: Option<&str>,
    ) -> Result<(), SessionError> {
        if force {
            unsafe {
                std::env::remove_var("THUD_ACTIVE");
                std::env::remove_var("THUD_SOCKET");
                std::env::remove_var("THUD_TTY");
            }
            return Ok(());
        }

        if std::env::var("THUD_ACTIVE")
            .ok()
            .is_some_and(|v| !v.is_empty() && v != "0")
        {
            // If THUD_TTY is set and differs from current terminal's TTY, this is a separate terminal window/tab
            if let Ok(thud_tty) = std::env::var("THUD_TTY")
                && let Some(curr_tty) = current_tty
                && thud_tty != curr_tty
            {
                unsafe {
                    std::env::remove_var("THUD_ACTIVE");
                    std::env::remove_var("THUD_SOCKET");
                    std::env::remove_var("THUD_TTY");
                }
                return Ok(());
            }

            if let Ok(sock) = std::env::var("THUD_SOCKET") {
                if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
                    return Err(SessionError::NestedSession);
                }
                // Socket is unreachable: previous session terminated abnormally.
                // Clear stale inherited environment variables and proceed.
                let _ = std::fs::remove_file(&sock);
                unsafe {
                    std::env::remove_var("THUD_ACTIVE");
                    std::env::remove_var("THUD_SOCKET");
                    std::env::remove_var("THUD_TTY");
                }
            } else {
                unsafe {
                    std::env::remove_var("THUD_ACTIVE");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::ipc::OsIpcServer;

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

    #[test]
    fn validates_nested_session_checks_socket_liveness() {
        let sock_path =
            std::env::temp_dir().join(format!("thud-alive-check-{}.sock", std::process::id()));
        let _server = OsIpcServer::bind(&sock_path).unwrap();

        unsafe {
            std::env::set_var("THUD_ACTIVE", "1");
            std::env::set_var("THUD_SOCKET", sock_path.as_os_str());
            std::env::remove_var("THUD_TTY");
        }

        // Alive socket without force rejects nested session
        assert!(matches!(
            Session::check_nested_session(false),
            Err(SessionError::NestedSession)
        ));

        // --force overrides even when socket is alive
        assert!(Session::check_nested_session(true).is_ok());
        assert!(std::env::var("THUD_ACTIVE").is_err());
        assert!(std::env::var("THUD_SOCKET").is_err());

        // When socket is dead / unreachable, stale env vars are cleared and session is allowed
        let dead_sock = std::env::temp_dir().join("thud-dead-sock-test.sock");
        let _ = std::fs::remove_file(&dead_sock);
        unsafe {
            std::env::set_var("THUD_ACTIVE", "1");
            std::env::set_var("THUD_SOCKET", dead_sock.as_os_str());
            std::env::set_var("THUD_TTY", "/dev/ttys_stale");
        }
        assert!(Session::check_nested_session(false).is_ok());
        assert!(std::env::var("THUD_ACTIVE").is_err());
        assert!(std::env::var("THUD_SOCKET").is_err());
        assert!(std::env::var("THUD_TTY").is_err());

        // When running in a different terminal window/tab (different TTY), allow and purge inherited env
        unsafe {
            std::env::set_var("THUD_ACTIVE", "1");
            std::env::set_var("THUD_SOCKET", sock_path.as_os_str());
            std::env::set_var("THUD_TTY", "/dev/ttys001");
        }
        assert!(Session::check_nested_session_impl(false, Some("/dev/ttys002")).is_ok());
        assert!(std::env::var("THUD_ACTIVE").is_err());
        assert!(std::env::var("THUD_SOCKET").is_err());
        assert!(std::env::var("THUD_TTY").is_err());

        // When running on the exact same TTY with alive socket, block
        unsafe {
            std::env::set_var("THUD_ACTIVE", "1");
            std::env::set_var("THUD_SOCKET", sock_path.as_os_str());
            std::env::set_var("THUD_TTY", "/dev/ttys001");
        }
        assert!(matches!(
            Session::check_nested_session_impl(false, Some("/dev/ttys001")),
            Err(SessionError::NestedSession)
        ));
    }
}
