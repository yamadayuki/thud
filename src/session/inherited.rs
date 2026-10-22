use std::ffi::OsString;
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

/// The `THUD_*` variables a new `thud` process inherited from its parent shell.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InheritedSession {
    active: bool,
    socket: Option<PathBuf>,
    tty: Option<String>,
}

/// How a launch relates to the session described by its inherited variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Assessment {
    /// No session was inherited.
    Fresh,
    /// The inherited session belongs to a different terminal window or tab.
    OtherTerminal,
    /// The inherited session is gone; `dead_socket` is its leftover socket path, if any.
    Stale { dead_socket: Option<PathBuf> },
    /// Launched from inside a live session on the same terminal.
    Nested,
}

impl InheritedSession {
    pub fn from_env() -> Self {
        Self::from_vars(
            std::env::var_os("THUD_ACTIVE"),
            std::env::var_os("THUD_SOCKET"),
            std::env::var_os("THUD_TTY"),
        )
    }

    fn from_vars(
        active: Option<OsString>,
        socket: Option<OsString>,
        tty: Option<OsString>,
    ) -> Self {
        Self {
            active: active.is_some_and(|v| !v.is_empty() && v != "0"),
            socket: socket.filter(|s| !s.is_empty()).map(PathBuf::from),
            tty: tty
                .filter(|t| !t.is_empty())
                .map(|t| t.to_string_lossy().into_owned()),
        }
    }

    /// Classifies the launch from the current terminal's TTY name and a liveness probe for
    /// the inherited IPC socket.
    pub fn assess(
        &self,
        current_tty: Option<&str>,
        socket_alive: impl FnOnce(&Path) -> bool,
    ) -> Assessment {
        if !self.active {
            return Assessment::Fresh;
        }
        if let (Some(inherited), Some(current)) = (self.tty.as_deref(), current_tty)
            && inherited != current
        {
            return Assessment::OtherTerminal;
        }
        match &self.socket {
            Some(socket) if socket_alive(socket) => Assessment::Nested,
            dead => Assessment::Stale {
                dead_socket: dead.clone(),
            },
        }
    }
}

/// Removes `path` only if it is a Unix domain socket, so a mistyped path can never delete a
/// regular file.
pub(crate) fn remove_socket_file(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn inherited(active: &str, socket: Option<&str>, tty: Option<&str>) -> InheritedSession {
        InheritedSession::from_vars(
            Some(active.into()),
            socket.map(Into::into),
            tty.map(Into::into),
        )
    }

    #[test]
    fn assesses_inherited_sessions() {
        let alive = |_: &Path| true;
        let dead = |_: &Path| false;

        assert_eq!(
            InheritedSession::default().assess(Some("/dev/ttys001"), alive),
            Assessment::Fresh
        );
        assert_eq!(
            inherited("0", Some("/tmp/s.sock"), None).assess(None, alive),
            Assessment::Fresh
        );
        assert_eq!(
            inherited("1", Some("/tmp/s.sock"), Some("/dev/ttys001"))
                .assess(Some("/dev/ttys002"), alive),
            Assessment::OtherTerminal
        );
        assert_eq!(
            inherited("1", Some("/tmp/s.sock"), Some("/dev/ttys001"))
                .assess(Some("/dev/ttys001"), alive),
            Assessment::Nested
        );
        assert_eq!(
            inherited("1", Some("/tmp/s.sock"), None).assess(Some("/dev/ttys001"), alive),
            Assessment::Nested
        );
        assert_eq!(
            inherited("1", Some("/tmp/s.sock"), Some("/dev/ttys001"))
                .assess(Some("/dev/ttys001"), dead),
            Assessment::Stale {
                dead_socket: Some("/tmp/s.sock".into())
            }
        );
        assert_eq!(
            inherited("1", None, None).assess(None, alive),
            Assessment::Stale { dead_socket: None }
        );
    }

    #[test]
    fn removes_only_socket_files() {
        let dir = std::env::temp_dir().join(format!("thud-inherited-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let regular = dir.join("important.txt");
        std::fs::write(&regular, "keep me").unwrap();
        remove_socket_file(&regular).unwrap();
        assert!(regular.exists());

        let socket = dir.join("dead.sock");
        drop(UnixListener::bind(&socket).unwrap());
        remove_socket_file(&socket).unwrap();
        assert!(!socket.exists());

        remove_socket_file(&dir.join("missing.sock")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
