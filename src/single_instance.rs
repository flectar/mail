//! One desktop process per profile.
//!
//! The first process owns the profile and listens on a private local endpoint:
//! a Unix socket guarded by an advisory lock, or a named pipe whose first
//! instance is exclusive on Windows. A later launch asks that process to show
//! its window, passing on any mail links it was opened with, and exits. So
//! opening Flectar Mail from a launcher or a `mailto:` link while it runs in
//! the tray uses the existing window instead of starting a second sync engine
//! on the same database. A sign-in launch that finds the application already
//! running exits without disturbing it.
//!
//! Both the lock and the pipe are released by the kernel when the owner exits,
//! even after a crash, so a stale endpoint never blocks a later launch.

use crate::mailto_links::MAX_LAUNCH_LINKS;
use flectar_mail_core::mailto;
use sha2::{Digest, Sha256};
use std::{io, path::Path, sync::Arc, time::Duration};
use tokio::{io::AsyncReadExt, runtime::Runtime, task::JoinHandle};

/// The only request: show and focus the main window. The first line may add
/// the launcher's activation token so the compositor lets the window take
/// focus; each further line is a mail link to open.
const SHOW: &str = "show";
/// Activation tokens are short opaque strings; bound what we accept.
const MAX_TOKEN: usize = 512;
/// Longer than any valid request, so a misbehaving client cannot make the
/// owner buffer more.
const MAX_REQUEST: u64 =
    (SHOW.len() + 1 + MAX_TOKEN + 1 + MAX_LAUNCH_LINKS * (mailto::MAX_URI_BYTES + 1)) as u64;
/// How long the owner waits for a connected client to send its request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a later launch waits for a starting owner to accept requests.
const CONNECT_ATTEMPTS: u32 = 40;
const CONNECT_RETRY: Duration = Duration::from_millis(50);

pub(crate) enum Acquired {
    /// This process owns the profile. Keep the value alive until exit.
    Primary(Primary),
    /// Another process owns the profile and received the request.
    Forwarded,
    /// Another process owns the profile but could not be reached. Starting a
    /// second sync engine on the same database would be worse than exiting.
    Unresponsive(io::Error),
}

/// What a launch should ask of an already running instance.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Request {
    /// Show the window and open the mail links.
    Show(Activation),
    /// A sign-in launch: leave a running instance exactly as it is.
    Nothing,
}

/// A launch handed to the running instance.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Activation {
    /// The launcher's token, for the compositor to let the window take focus.
    pub(crate) activation_token: Option<String>,
    /// `mailto:` links the launch was opened with.
    pub(crate) links: Vec<String>,
}

/// Runs on a runtime worker thread with each forwarded launch.
type ShowCallback = Arc<dyn Fn(Activation) + Send + Sync>;

pub(crate) struct Primary {
    endpoint: imp::Endpoint,
    server: Option<(tokio::runtime::Handle, JoinHandle<()>)>,
}

impl Primary {
    /// Start answering later launches. `show` receives each forwarded
    /// launch on a runtime worker thread and must forward to the UI thread
    /// itself.
    pub(crate) fn serve(
        &mut self,
        runtime: &Runtime,
        show: impl Fn(Activation) + Send + Sync + 'static,
    ) {
        let show: ShowCallback = Arc::new(show);
        let _context = runtime.enter();
        if let Some(task) = self.endpoint.serve(show) {
            self.server = Some((runtime.handle().clone(), task));
        }
    }
}

impl Drop for Primary {
    fn drop(&mut self) {
        // Close the endpoint before returning, so a process started right
        // after this one exits (the CPU fallback restart) can take it over.
        if let Some((runtime, task)) = self.server.take() {
            task.abort();
            let _ = runtime.block_on(task);
        }
    }
}

/// Claim the profile in `data_dir`, or hand `request` to the process that
/// already owns it.
pub(crate) fn acquire(
    runtime: &Runtime,
    data_dir: &Path,
    request: Request,
) -> io::Result<Acquired> {
    std::fs::create_dir_all(data_dir)?;
    let _context = runtime.enter();
    let mut retried = false;
    loop {
        if let Some(endpoint) = imp::claim(data_dir)? {
            return Ok(Acquired::Primary(Primary {
                endpoint,
                server: None,
            }));
        }
        let Request::Show(activation) = &request else {
            return Ok(Acquired::Forwarded);
        };
        let message = encode_show(activation);
        match runtime.block_on(imp::forward(data_dir, &message)) {
            Ok(()) => return Ok(Acquired::Forwarded),
            // The owner may have exited after the claim failed; claim again.
            Err(_) if !retried => retried = true,
            Err(error) => return Ok(Acquired::Unresponsive(error)),
        }
    }
}

/// A short, stable name for the profile; endpoints share a namespace across
/// profiles and, for named pipes, across users.
fn profile_key(data_dir: &Path) -> String {
    let digest = Sha256::digest(data_dir.as_os_str().as_encoded_bytes());
    digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn read_request(stream: impl tokio::io::AsyncRead + Unpin) -> Option<Vec<u8>> {
    let mut request = Vec::new();
    let mut stream = stream.take(MAX_REQUEST);
    tokio::time::timeout(REQUEST_TIMEOUT, stream.read_to_end(&mut request))
        .await
        .ok()?
        .ok()?;
    Some(request)
}

fn valid_token(token: &str) -> bool {
    (1..=MAX_TOKEN).contains(&token.len()) && token.bytes().all(|byte| byte.is_ascii_graphic())
}

fn encode_show(activation: &Activation) -> Vec<u8> {
    let mut request = match activation
        .activation_token
        .as_deref()
        .filter(|token| valid_token(token))
    {
        Some(token) => format!("{SHOW} {token}\n"),
        None => format!("{SHOW}\n"),
    };
    // Line breaks mean the same percent-encoded inside a mailto link, so a
    // link always fits on one line. Links the owner would refuse are dropped
    // here rather than failing the whole launch.
    let links = activation
        .links
        .iter()
        .map(|link| link.replace('\r', "%0D").replace('\n', "%0A"))
        .filter(|link| valid_link(link))
        .take(MAX_LAUNCH_LINKS);
    for link in links {
        request.push_str(&link);
        request.push('\n');
    }
    request.into_bytes()
}

/// The launch for a well-formed show request; anything else is ignored.
fn parse_show(request: &[u8]) -> Option<Activation> {
    let request = std::str::from_utf8(request).ok()?.strip_suffix('\n')?;
    let mut lines = request.split('\n');
    let first = lines.next()?;
    let activation_token = match first.split_once(' ') {
        None if first == SHOW => None,
        Some((SHOW, token)) if valid_token(token) => Some(token.to_owned()),
        _ => return None,
    };
    let links = lines.map(str::to_owned).collect::<Vec<_>>();
    if links.len() > MAX_LAUNCH_LINKS || !links.iter().all(|link| valid_link(link)) {
        return None;
    }
    Some(Activation {
        activation_token,
        links,
    })
}

fn valid_link(link: &str) -> bool {
    mailto::is_mailto(link) && link.len() <= mailto::MAX_URI_BYTES && !link.contains(['\r', '\n'])
}

fn handle_request(request: Option<Vec<u8>>, show: &(dyn Fn(Activation) + Send + Sync)) {
    if let Some(activation) = request.as_deref().and_then(parse_show) {
        show(activation);
    }
}

#[cfg(unix)]
mod imp {
    use super::{
        CONNECT_ATTEMPTS, CONNECT_RETRY, ShowCallback, handle_request, profile_key, read_request,
    };
    use std::{
        fs::File,
        io,
        os::unix::{fs::MetadataExt, io::AsRawFd, net::UnixListener},
        path::{Path, PathBuf},
        sync::Arc,
    };
    use tokio::{io::AsyncWriteExt, task::JoinHandle};

    /// `sockaddr_un` holds 104 bytes on macOS and 108 on Linux, including
    /// the terminator.
    const MAX_SOCKET_PATH: usize = 103;

    pub(super) struct Endpoint {
        /// Held for the life of the process; the kernel drops the lock when
        /// the process exits.
        _lock: File,
        listener: Option<UnixListener>,
        /// The bound socket file and its device and inode numbers.
        socket: Option<(PathBuf, (u64, u64))>,
    }

    impl Endpoint {
        pub(super) fn serve(&mut self, show: ShowCallback) -> Option<JoinHandle<()>> {
            let listener = match tokio::net::UnixListener::from_std(self.listener.take()?) {
                Ok(listener) => listener,
                Err(error) => {
                    eprintln!("could not accept launch requests: {error}");
                    return None;
                }
            };
            Some(tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        continue;
                    };
                    let show = Arc::clone(&show);
                    tokio::spawn(async move {
                        handle_request(read_request(stream).await, show.as_ref());
                    });
                }
            }))
        }
    }

    impl Drop for Endpoint {
        fn drop(&mut self) {
            // Remove only the socket this process bound. The lock is still
            // held here, so no other process can have replaced it yet.
            if let Some((path, identity)) = self.socket.take()
                && std::fs::symlink_metadata(&path)
                    .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == identity)
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// Prefer the per-user runtime directory: it is private, cleared at
    /// logout and short enough for socket paths.
    fn socket_path(data_dir: &Path) -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|directory| directory.is_absolute() && directory.is_dir())
            .map(|directory| {
                directory.join(format!("com.flectar.mail-{}.sock", profile_key(data_dir)))
            })
            .unwrap_or_else(|| data_dir.join("instance.sock"))
    }

    pub(super) fn claim(data_dir: &Path) -> io::Result<Option<Endpoint>> {
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data_dir.join("instance.lock"))?;
        // SAFETY: `lock` is an open descriptor owned by this function.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::EWOULDBLOCK) => Ok(None),
                _ => Err(error),
            };
        }

        let mut endpoint = Endpoint {
            _lock: lock,
            listener: None,
            socket: None,
        };
        let path = socket_path(data_dir);
        if path.as_os_str().len() > MAX_SOCKET_PATH {
            eprintln!(
                "{} is too long for a socket; later launches cannot show this window",
                path.display()
            );
            return Ok(Some(endpoint));
        }
        // Holding the lock proves any existing socket is stale.
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        endpoint.socket = Some((path, (metadata.dev(), metadata.ino())));
        endpoint.listener = Some(listener);
        Ok(Some(endpoint))
    }

    pub(super) async fn forward(data_dir: &Path, request: &[u8]) -> io::Result<()> {
        let path = socket_path(data_dir);
        let mut attempt = 0;
        // The owner takes the lock just before binding; give a starting
        // owner a moment to begin listening.
        let mut stream = loop {
            match tokio::net::UnixStream::connect(&path).await {
                Ok(stream) => break stream,
                Err(error) if attempt + 1 < CONNECT_ATTEMPTS => {
                    if !matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) {
                        return Err(error);
                    }
                    attempt += 1;
                    tokio::time::sleep(CONNECT_RETRY).await;
                }
                Err(error) => return Err(error),
            }
        };
        stream.write_all(request).await?;
        stream.shutdown().await
    }
}

#[cfg(windows)]
mod imp {
    use super::{
        CONNECT_ATTEMPTS, CONNECT_RETRY, ShowCallback, handle_request, profile_key, read_request,
    };
    use std::{io, path::Path, sync::Arc};
    use tokio::{
        io::AsyncWriteExt,
        net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions},
        task::JoinHandle,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY},
        UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow},
    };

    pub(super) struct Endpoint {
        name: String,
        server: Option<NamedPipeServer>,
    }

    impl Endpoint {
        pub(super) fn serve(&mut self, show: ShowCallback) -> Option<JoinHandle<()>> {
            let mut server = self.server.take()?;
            let name = self.name.clone();
            Some(tokio::spawn(async move {
                loop {
                    if server.connect().await.is_err() {
                        // The client went away before it was accepted.
                        server = match pipe(&name, false) {
                            Ok(server) => server,
                            Err(_) => return,
                        };
                        continue;
                    }
                    // Create the next instance before handing this one off, so
                    // the pipe name never disappears while this process runs.
                    let connected = match pipe(&name, false) {
                        Ok(next) => std::mem::replace(&mut server, next),
                        Err(_) => return,
                    };
                    let show = Arc::clone(&show);
                    tokio::spawn(async move {
                        handle_request(read_request(connected).await, show.as_ref());
                    });
                }
            }))
        }
    }

    fn pipe_name(data_dir: &Path) -> String {
        format!(r"\\.\pipe\com.flectar.mail-{}", profile_key(data_dir))
    }

    fn pipe(name: &str, first: bool) -> io::Result<NamedPipeServer> {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .access_outbound(false)
            .create(name)
    }

    pub(super) fn claim(data_dir: &Path) -> io::Result<Option<Endpoint>> {
        let name = pipe_name(data_dir);
        match pipe(&name, true) {
            Ok(server) => Ok(Some(Endpoint {
                name,
                server: Some(server),
            })),
            // Another process already created the first instance.
            Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(super) async fn forward(data_dir: &Path, request: &[u8]) -> io::Result<()> {
        let name = pipe_name(data_dir);
        let mut attempt = 0;
        let mut client = loop {
            match ClientOptions::new().read(false).open(&name) {
                Ok(client) => break client,
                Err(error)
                    if attempt + 1 < CONNECT_ATTEMPTS
                        && matches!(
                            error.raw_os_error(),
                            Some(code) if code == ERROR_PIPE_BUSY as i32
                                || code == ERROR_FILE_NOT_FOUND as i32
                        ) =>
                {
                    attempt += 1;
                    tokio::time::sleep(CONNECT_RETRY).await;
                }
                Err(error) => return Err(error),
            }
        };
        // This launch was started by the user and may bring a window to the
        // foreground; pass that right on to the running instance.
        // SAFETY: plain Win32 call without pointers.
        unsafe { AllowSetForegroundWindow(ASFW_ANY) };
        client.write_all(request).await?;
        client.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap()
    }

    fn activation(activation_token: Option<&str>, links: &[&str]) -> Activation {
        Activation {
            activation_token: activation_token.map(str::to_owned),
            links: links.iter().map(|link| (*link).to_owned()).collect(),
        }
    }

    fn show(activation_token: Option<&str>) -> Request {
        Request::Show(activation(activation_token, &[]))
    }

    #[test]
    fn later_launches_reach_the_first_process_and_leave_it_running() {
        let runtime = runtime();
        let directory = tempfile::tempdir().unwrap();
        let Acquired::Primary(mut primary) =
            acquire(&runtime, directory.path(), show(None)).unwrap()
        else {
            panic!("the first launch must own the profile");
        };
        let (shown_tx, shown_rx) = std::sync::mpsc::channel();
        primary.serve(&runtime, move |activation| {
            shown_tx.send(activation).unwrap()
        });

        assert!(matches!(
            acquire(&runtime, directory.path(), show(Some("launcher-token_1"))).unwrap(),
            Acquired::Forwarded
        ));
        assert_eq!(
            shown_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            activation(Some("launcher-token_1"), &[])
        );

        // A mail link opened while the application runs reaches it whole.
        let link = format!(
            "mailto:a+b@example.com?subject=Hi&body={}%0D%0Aend",
            "x".repeat(100_000)
        );
        let request = Request::Show(activation(None, &[&link, "mailto:"]));
        assert!(matches!(
            acquire(&runtime, directory.path(), request).unwrap(),
            Acquired::Forwarded
        ));
        assert_eq!(
            shown_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            activation(None, &[&link, "mailto:"])
        );

        assert!(matches!(
            acquire(&runtime, directory.path(), Request::Nothing).unwrap(),
            Acquired::Forwarded
        ));
        assert!(
            shown_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "a sign-in launch must not show the running window"
        );

        drop(primary);
        assert!(matches!(
            acquire(&runtime, directory.path(), show(None)).unwrap(),
            Acquired::Primary(_)
        ));
    }

    #[test]
    fn profiles_are_independent() {
        let runtime = runtime();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        assert_ne!(profile_key(first.path()), profile_key(second.path()));
        let Acquired::Primary(_first) = acquire(&runtime, first.path(), show(None)).unwrap()
        else {
            panic!("first profile");
        };
        assert!(matches!(
            acquire(&runtime, second.path(), show(None)).unwrap(),
            Acquired::Primary(_)
        ));
    }

    #[test]
    fn only_well_formed_show_requests_are_accepted() {
        let round_trip = |activation: Activation| parse_show(&encode_show(&activation));
        assert_eq!(
            round_trip(activation(None, &[])),
            Some(activation(None, &[]))
        );
        assert_eq!(
            round_trip(activation(Some("abc:123"), &["mailto:a@example.com"])),
            Some(activation(Some("abc:123"), &["mailto:a@example.com"]))
        );
        // Invalid tokens are dropped by the sender rather than the request.
        assert_eq!(
            round_trip(activation(Some("has space"), &[])),
            Some(activation(None, &[]))
        );
        assert_eq!(
            round_trip(activation(Some(&"x".repeat(MAX_TOKEN + 1)), &[])),
            Some(activation(None, &[]))
        );
        // So are links the owner would refuse, and raw line breaks are
        // encoded the way the link means them.
        let too_long = format!("mailto:?body={}", "x".repeat(mailto::MAX_URI_BYTES));
        assert_eq!(
            round_trip(activation(
                None,
                &["https://example.com", &too_long, "mailto:?body=a\r\nb"]
            )),
            Some(activation(None, &["mailto:?body=a%0D%0Ab"]))
        );
        let many = vec!["mailto:a@example.com"; MAX_LAUNCH_LINKS + 3];
        assert_eq!(
            round_trip(activation(None, &many)).unwrap().links.len(),
            MAX_LAUNCH_LINKS
        );

        for request in [
            &b"quit\n"[..],
            b"show",
            b"show\nshow\n",
            b"show \n",
            b"show a b\n",
            b"shows\n",
            b"show \xff\n",
            b"show\nhttps://example.com\n",
            b"show\nmailto:a@example.com",
            b"show\nmailto:a@example.com\n\n",
            b"mailto:a@example.com\n",
        ] {
            assert_eq!(parse_show(request), None, "{request:?}");
        }
        let largest = activation(
            Some(&"x".repeat(MAX_TOKEN)),
            &vec![
                &*format!("mailto:?body={}", "x".repeat(mailto::MAX_URI_BYTES - 13));
                MAX_LAUNCH_LINKS
            ],
        );
        assert_eq!(largest.links[0].len(), mailto::MAX_URI_BYTES);
        assert!(MAX_REQUEST as usize >= encode_show(&largest).len());
        assert_eq!(round_trip(largest.clone()), Some(largest));
    }
}
