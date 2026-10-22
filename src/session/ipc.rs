use crate::config::IpcMessage;
use crate::session::SessionError;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Non-blocking Unix domain socket listener for live HUD IPC commands.
pub(crate) struct OsIpcServer {
    listener: UnixListener,
    socket_path: PathBuf,
}

impl OsIpcServer {
    pub fn bind(path: &Path) -> io::Result<Self> {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            socket_path: path.to_path_buf(),
        })
    }

    pub fn drain_messages(&self) -> Vec<IpcMessage> {
        let mut messages = Vec::new();
        while let Ok((stream, _)) = self.listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(20)));
            let reader = BufReader::new(stream);
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(msg) = serde_json::from_str::<IpcMessage>(line.trim()) {
                    messages.push(msg);
                }
            }
        }
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
        None => {
            return Err(SessionError::Ipc(
                "THUD_SOCKET is not set and no --socket path was provided".to_string(),
            ));
        }
    };

    let mut stream = UnixStream::connect(&path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound || e.kind() == io::ErrorKind::ConnectionRefused {
            SessionError::Ipc(format!(
                "no active thud session reachable at {} ({e})",
                path.display()
            ))
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
