pub(crate) mod inherited;
pub(crate) mod io;
pub(crate) mod ipc;
#[cfg(test)]
pub(crate) mod memory_io;
pub(crate) mod probes;
pub(crate) mod pty;
pub(crate) mod session_loop;
pub(crate) mod tty;

use crate::config::{Config, ConfigError, IpcMessage};
use crate::session::inherited::{Assessment, InheritedSession, remove_socket_file};
use crate::session::io::OsSessionIo;
use crate::session::session_loop::SessionLoop;
use std::io as std_io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

#[derive(Debug)]
pub enum SessionError {
    Config(ConfigError),
    Io(std_io::Error),
    Ipc(String),
    NestedSession,
    /// No running session was reachable; `socket` is the path that was tried, if any.
    NoActiveSession {
        socket: Option<PathBuf>,
    },
    /// Invalid command-line usage.
    Usage(String),
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
            SessionError::NoActiveSession { socket: Some(path) } => {
                write!(f, "no active thud session reachable at {}", path.display())
            }
            SessionError::NoActiveSession { socket: None } => write!(
                f,
                "no active thud session (THUD_SOCKET is not set and no --socket path was provided)"
            ),
            SessionError::Usage(msg) => write!(f, "{msg}"),
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
    ///
    /// Live reloads read `config_path`, or the default config location when `None`.
    pub fn run(config: Config, config_path: Option<PathBuf>) -> Result<i32, SessionError> {
        Self::check_nested_session(config.force)?;
        let io = OsSessionIo::start(&config, config_path.or_else(Config::default_path))?;
        SessionLoop::new(config, io)?.run()
    }

    /// Sends a single `IpcMessage` over the Unix domain socket to a running `thud` session.
    pub fn send_ipc_message(
        socket_path: Option<PathBuf>,
        msg: &IpcMessage,
    ) -> Result<(), SessionError> {
        ipc::send_ipc_message(socket_path, msg)
    }

    /// Refuses to start inside a live session on the same terminal (unless `force`), and
    /// removes the leftover socket of a session that died.
    fn check_nested_session(force: bool) -> Result<(), SessionError> {
        if force {
            return Ok(());
        }
        let current_tty = tty::get_tty_name(libc::STDIN_FILENO);
        match InheritedSession::from_env().assess(current_tty.as_deref(), |socket| {
            UnixStream::connect(socket).is_ok()
        }) {
            Assessment::Nested => Err(SessionError::NestedSession),
            Assessment::Stale {
                dead_socket: Some(socket),
            } => {
                let _ = remove_socket_file(&socket);
                Ok(())
            }
            Assessment::Fresh | Assessment::OtherTerminal | Assessment::Stale { .. } => Ok(()),
        }
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
            let mut server = OsIpcServer::bind(&sock_path).unwrap();
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
    fn ipc_server_keeps_clients_accepted_before_they_write() {
        use std::io::Write;

        let sock_path =
            std::env::temp_dir().join(format!("thud-early-{}.sock", std::process::id()));
        let mut server = OsIpcServer::bind(&sock_path).unwrap();

        // Accepted while the client has not written yet (the macOS race behind EPIPE).
        let mut client = UnixStream::connect(&sock_path).unwrap();
        assert!(server.drain_messages().is_empty());

        client
            .write_all(br#"{"action":"set_slot","key":"a","value":"1"}"#)
            .unwrap();
        client.write_all(b"\n{\"action\":\"clear_slot\",").unwrap();
        assert_eq!(
            server.drain_messages(),
            vec![IpcMessage::SetSlot {
                key: "a".to_string(),
                value: "1".to_string(),
                ttl_ms: None,
            }]
        );

        // The rest of a split message, terminated by closing the connection.
        client.write_all(br#""key":"a"}"#).unwrap();
        drop(client);
        assert_eq!(
            server.drain_messages(),
            vec![IpcMessage::ClearSlot {
                key: "a".to_string()
            }]
        );
    }
}
