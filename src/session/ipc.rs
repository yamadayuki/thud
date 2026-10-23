use crate::config::IpcMessage;
use crate::session::SessionError;
use crate::session::inherited::remove_socket_file;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long an accepted connection may stay open without finishing its messages.
const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Non-blocking Unix domain socket listener for live HUD IPC commands.
pub(crate) struct OsIpcServer {
    listener: UnixListener,
    socket_path: PathBuf,
    clients: Vec<IpcClient>,
}

/// An accepted connection whose newline-delimited JSON messages may arrive over several
/// drains (a client can be accepted before it has written anything).
struct IpcClient {
    stream: UnixStream,
    buf: Vec<u8>,
    last_activity: Instant,
}

impl IpcClient {
    /// Reads what is available, appending complete messages to `out`. Returns `false` once
    /// the client has closed the connection or failed.
    fn read_available(&mut self, out: &mut Vec<IpcMessage>) -> bool {
        let mut chunk = [0u8; 4096];
        let open = loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => break false,
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    self.last_activity = Instant::now();
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break true,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break false,
            }
        };
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            push_message(&line, out);
        }
        if !open {
            push_message(&std::mem::take(&mut self.buf), out);
        }
        open
    }
}

fn push_message(line: &[u8], out: &mut Vec<IpcMessage>) {
    if let Ok(text) = std::str::from_utf8(line)
        && let Ok(msg) = serde_json::from_str::<IpcMessage>(text.trim())
    {
        out.push(msg);
    }
}

impl OsIpcServer {
    pub fn bind(path: &Path) -> io::Result<Self> {
        remove_socket_file(path)?;
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            socket_path: path.to_path_buf(),
            clients: Vec::new(),
        })
    }

    /// Accepts pending connections and returns every message received so far. Never blocks.
    pub fn drain_messages(&mut self) -> Vec<IpcMessage> {
        while let Ok((stream, _)) = self.listener.accept() {
            // Accepted sockets inherit the listener's non-blocking flag on BSD/macOS but not on
            // Linux; set it explicitly so reads never block the session loop.
            if stream.set_nonblocking(true).is_ok() {
                self.clients.push(IpcClient {
                    stream,
                    buf: Vec::new(),
                    last_activity: Instant::now(),
                });
            }
        }

        let mut messages = Vec::new();
        self.clients.retain_mut(|client| {
            client.read_available(&mut messages)
                && client.last_activity.elapsed() < CLIENT_IDLE_TIMEOUT
        });
        messages
    }
}

impl Drop for OsIpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
    }
}

/// Sends a single JSON-encoded `IpcMessage` over the Unix domain socket.
pub(crate) fn send_ipc_message(
    socket_path: Option<PathBuf>,
    msg: &IpcMessage,
) -> Result<(), SessionError> {
    let path = match socket_path.or_else(|| std::env::var("THUD_SOCKET").ok().map(PathBuf::from)) {
        Some(p) => p,
        None => return Err(SessionError::NoActiveSession { socket: None }),
    };

    let mut stream = UnixStream::connect(&path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound || e.kind() == io::ErrorKind::ConnectionRefused {
            SessionError::NoActiveSession { socket: Some(path) }
        } else {
            SessionError::Io(e)
        }
    })?;
    let mut payload = serde_json::to_string(msg).map_err(|e| SessionError::Ipc(e.to_string()))?;
    payload.push('\n');
    stream.write_all(payload.as_bytes())?;
    stream.flush()?;
    Ok(())
}
