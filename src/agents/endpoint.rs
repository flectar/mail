//! The local endpoint the bridge reaches the running app on. A Unix socket in
//! a directory only this user can enter, or on Windows a named pipe only this
//! user can open. Each side also checks that the other runs as this user, so
//! neither talks to another user's process.

use std::path::Path;

/// Named after the profile like the single-instance endpoint, so two
/// profiles never share one.
fn endpoint_name(data_dir: &Path) -> String {
    format!(
        "com.flectar.mail-{}-agents",
        crate::single_instance::profile_key(data_dir)
    )
}

#[cfg(unix)]
pub(crate) use unix::{Listener, connect};
#[cfg(windows)]
pub(crate) use windows::{Listener, connect};

#[cfg(unix)]
mod unix {
    use super::endpoint_name;
    use std::{
        io,
        os::unix::fs::{MetadataExt, PermissionsExt},
        path::{Path, PathBuf},
    };
    use tokio::net::{UnixListener, UnixStream};

    /// Socket paths longer than this don't fit `sockaddr_un` on every system.
    const MAX_SOCKET_PATH: usize = 103;

    /// The private runtime directory the session gives this user.
    fn runtime_dir(from_environment: bool) -> Option<PathBuf> {
        let from_environment = from_environment
            .then(|| std::env::var_os("XDG_RUNTIME_DIR"))
            .flatten()
            .map(PathBuf::from);
        // Agents' apps often start the bridge with a cleaned environment, so
        // it also looks where logind puts the directory, if it's ours alone.
        let fallback = || {
            let directory = PathBuf::from(format!("/run/user/{}", effective_uid()));
            let metadata = std::fs::metadata(&directory).ok()?;
            (metadata.is_dir()
                && metadata.uid() == effective_uid()
                && metadata.permissions().mode() & 0o077 == 0)
                .then_some(directory)
        };
        from_environment
            .filter(|directory| directory.is_absolute() && directory.is_dir())
            .or_else(fallback)
    }

    fn effective_uid() -> u32 {
        // SAFETY: geteuid cannot fail.
        unsafe { libc::geteuid() }
    }

    fn socket_in(directory: &Path, data_dir: &Path) -> PathBuf {
        directory.join(format!("{}.sock", endpoint_name(data_dir)))
    }

    /// Where the app listens: the runtime directory, as for the single-instance
    /// endpoint, or the private data directory when there is none.
    fn listen_path(data_dir: &Path) -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|directory| directory.is_absolute() && directory.is_dir())
            .map(|directory| socket_in(&directory, data_dir))
            .unwrap_or_else(|| data_dir.join("agents.sock"))
    }

    /// Where the bridge looks, best first.
    fn connect_paths(data_dir: &Path) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for path in [
            runtime_dir(true).map(|directory| socket_in(&directory, data_dir)),
            runtime_dir(false).map(|directory| socket_in(&directory, data_dir)),
            Some(data_dir.join("agents.sock")),
        ]
        .into_iter()
        .flatten()
        {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        paths
    }

    fn same_user(stream: &UnixStream) -> io::Result<()> {
        if stream.peer_cred()?.uid() == effective_uid() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the other end runs as another user",
            ))
        }
    }

    pub(crate) struct Listener {
        listener: UnixListener,
        /// The socket file and its identity, removed when the app stops
        /// listening unless something else replaced it.
        socket: (PathBuf, (u64, u64)),
    }

    impl Listener {
        pub(crate) fn bind(data_dir: &Path) -> io::Result<Self> {
            let path = listen_path(data_dir);
            if path.as_os_str().len() > MAX_SOCKET_PATH {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is too long for a socket", path.display()),
                ));
            }
            // This process owns the profile, so a socket left at this path
            // belonged to an earlier run of it.
            match std::fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
            let listener = UnixListener::bind(&path)?;
            // The directory already keeps other users out. Some systems ignore
            // a socket's own mode, but none are hurt by it.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            let metadata = std::fs::symlink_metadata(&path)?;
            Ok(Self {
                listener,
                socket: (path, (metadata.dev(), metadata.ino())),
            })
        }

        /// The next connection from a process of this user.
        pub(crate) async fn accept(&self) -> io::Result<UnixStream> {
            loop {
                let (stream, _) = self.listener.accept().await?;
                if same_user(&stream).is_ok() {
                    return Ok(stream);
                }
            }
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let (path, identity) = &self.socket;
            if std::fs::symlink_metadata(path)
                .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == *identity)
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// Reaches the running app's endpoint. `NotFound` when nothing listens.
    pub(crate) async fn connect(data_dir: &Path) -> io::Result<UnixStream> {
        let mut last_error = None;
        for path in connect_paths(data_dir) {
            match UnixStream::connect(&path).await {
                Ok(stream) => {
                    same_user(&stream)?;
                    return Ok(stream);
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(match last_error {
            Some(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                io::Error::new(io::ErrorKind::NotFound, error)
            }
            Some(error) => error,
            None => io::ErrorKind::NotFound.into(),
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn the_bridge_finds_the_app_and_the_socket_goes_with_it() {
            let directory = tempfile::tempdir().unwrap();
            let data_dir = directory.path().join("data");
            std::fs::create_dir(&data_dir).unwrap();
            assert_eq!(
                connect(&data_dir).await.unwrap_err().kind(),
                io::ErrorKind::NotFound
            );

            let listener = Listener::bind(&data_dir).unwrap();
            let (path, _) = listener.socket.clone();
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let (accepted, connected) = tokio::join!(listener.accept(), connect(&data_dir));
            accepted.unwrap();
            connected.unwrap();

            drop(listener);
            assert!(!path.exists());
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::endpoint_name;
    use std::{io, os::windows::io::AsRawHandle, path::Path, time::Duration};
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_PIPE_BUSY, HANDLE},
        System::Pipes::GetNamedPipeServerProcessId,
    };

    /// How long the bridge waits while every pipe instance is busy.
    const BUSY_ATTEMPTS: u32 = 40;
    const BUSY_RETRY: Duration = Duration::from_millis(50);

    fn pipe_name(data_dir: &Path) -> String {
        format!(r"\\.\pipe\{}", endpoint_name(data_dir))
    }

    pub(crate) struct Listener {
        name: String,
        security: crate::platform::windows_security::CurrentUserOnly,
        next: tokio::sync::Mutex<NamedPipeServer>,
    }

    impl Listener {
        pub(crate) fn bind(data_dir: &Path) -> io::Result<Self> {
            let name = pipe_name(data_dir);
            let security = crate::platform::windows_security::CurrentUserOnly::new()?;
            let first = create(&name, &security, true)?;
            Ok(Self {
                name,
                security,
                next: tokio::sync::Mutex::new(first),
            })
        }

        /// The next connection. Only this user can open the pipe.
        pub(crate) async fn accept(&self) -> io::Result<NamedPipeServer> {
            let mut next = self.next.lock().await;
            next.connect().await?;
            // Create the next instance before handing this one off, so the
            // pipe name never disappears while the app listens.
            let following = create(&self.name, &self.security, false)?;
            Ok(std::mem::replace(&mut *next, following))
        }
    }

    fn create(
        name: &str,
        security: &crate::platform::windows_security::CurrentUserOnly,
        first: bool,
    ) -> io::Result<NamedPipeServer> {
        let mut attributes = security.attributes();
        // SAFETY: `attributes` points at a security descriptor that `security`
        // keeps alive for this call.
        unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(name, (&raw mut attributes).cast())
        }
    }

    /// Reaches the running app's pipe and checks that the app runs as this
    /// user. `NotFound` when nothing listens.
    pub(crate) async fn connect(data_dir: &Path) -> io::Result<NamedPipeClient> {
        let name = pipe_name(data_dir);
        let mut attempt = 0;
        let client = loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => break client,
                Err(error)
                    if attempt + 1 < BUSY_ATTEMPTS
                        && error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) =>
                {
                    attempt += 1;
                    tokio::time::sleep(BUSY_RETRY).await;
                }
                Err(error) => return Err(error),
            }
        };
        let mut server_process = 0;
        // SAFETY: `client` is an open pipe handle and `server_process` a
        // valid output location.
        if unsafe {
            GetNamedPipeServerProcessId(client.as_raw_handle() as HANDLE, &mut server_process)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if !crate::platform::windows_security::process_is_current_user(server_process)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the pipe belongs to another user",
            ));
        }
        Ok(client)
    }
}
