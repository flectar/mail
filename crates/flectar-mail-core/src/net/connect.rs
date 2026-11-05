//! TCP connection setup for the mail protocols.

use futures::stream::{FuturesUnordered, StreamExt};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use tokio::net::{TcpSocket, TcpStream};
use tokio::time::{Duration, Instant};

/// How long an attempt may go unanswered before the next address is tried
/// alongside it: RFC 8305's recommended Connection Attempt Delay.
const ATTEMPT_DELAY: Duration = Duration::from_millis(250);

/// Open a TCP connection for a mail protocol. Nagle's algorithm only delays
/// their short request/response exchanges, so it is disabled when the
/// platform allows.
///
/// The host's addresses are raced as RFC 8305 (Happy Eyeballs) describes,
/// so an address that doesn't answer, such as one behind a broken IPv6
/// route, delays the connection by a quarter of a second instead of holding
/// it until the system gives up on that address.
pub async fn connect_tcp(host: &str, port: u16) -> io::Result<TcpStream> {
    let addresses = interleave(tokio::net::lookup_host((host, port)).await?.collect());
    let stream = race(addresses, connect_address).await?;
    stream.set_nodelay(true).ok();
    Ok(stream)
}

async fn connect_address(address: SocketAddr) -> io::Result<TcpStream> {
    let socket = if address.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    #[cfg(windows)]
    windows::fail_fast_on_loopback(&socket, address);
    socket.connect(address).await
}

/// RFC 8305 section 4: alternate the address families, starting with the
/// family of the resolver's first address. The resolver already ranks
/// addresses by RFC 6724, and the order within each family is kept.
fn interleave(addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let first_is_ipv6 = addresses.first().is_some_and(SocketAddr::is_ipv6);
    let (preferred, other): (Vec<_>, Vec<_>) = addresses
        .into_iter()
        .partition(|address| address.is_ipv6() == first_is_ipv6);
    let mut preferred = preferred.into_iter();
    let mut other = other.into_iter();
    let mut ordered = Vec::with_capacity(preferred.len() + other.len());
    loop {
        match (preferred.next(), other.next()) {
            (None, None) => return ordered,
            (first, second) => ordered.extend(first.into_iter().chain(second)),
        }
    }
}

/// RFC 8305 section 5: start one attempt at a time, the next one once the
/// running attempts have gone unanswered for [`ATTEMPT_DELAY`], keep the
/// first connection that succeeds and drop the others.
///
/// When an attempt fails, the next one starts at once, as Go and hyper-util
/// do. The RFC's 10 ms minimum bounds the Connection Attempt Delay, the wait
/// while earlier attempts are still pending. If every attempt fails, the
/// first address's error is returned, as Go does: it belongs to the most
/// preferred address.
async fn race<T, Attempt>(
    addresses: Vec<SocketAddr>,
    connect: impl Fn(SocketAddr) -> Attempt,
) -> io::Result<T>
where
    Attempt: Future<Output = io::Result<T>>,
{
    let mut waiting = addresses.into_iter().enumerate().peekable();
    let mut attempts = FuturesUnordered::new();
    let mut first_error: Option<(usize, io::Error)> = None;
    let mut next_start = Instant::now();
    loop {
        // An attempt that is due starts here rather than after a sleep: even
        // a sleep that has already expired waits for the timer's next tick,
        // up to about 15 ms on Windows.
        if Instant::now() >= next_start
            && let Some((index, address)) = waiting.next()
        {
            let attempt = connect(address);
            attempts.push(async move { (index, attempt.await) });
            next_start = Instant::now() + ATTEMPT_DELAY;
        }
        let can_start = waiting.peek().is_some();
        if attempts.is_empty() && !can_start {
            return Err(first_error.map(|(_, error)| error).unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "could not resolve to any address",
                )
            }));
        }
        tokio::select! {
            biased;
            Some((index, result)) = attempts.next(), if !attempts.is_empty() => match result {
                Ok(connection) => return Ok(connection),
                Err(error) => {
                    if first_error.as_ref().is_none_or(|(first, _)| index < *first) {
                        first_error = Some((index, error));
                    }
                    next_start = Instant::now();
                }
            },
            () = tokio::time::sleep_until(next_start), if can_start => {}
        }
    }
}

/// A loopback port nothing listens on, for tests of refused connections.
/// Until the returned value is dropped it keeps a connection on the port, so
/// the system can't choose the port as the source of the connection under
/// test and connect that connection to itself. Go's
/// TestDialClosedPortFailFast reserves its port the same way.
#[cfg(test)]
pub(crate) async fn closed_loopback_port() -> (u16, impl Sized) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let client = TcpStream::connect(address).await.unwrap();
    let (server, _) = listener.accept().await.unwrap();
    (address.port(), (client, server))
}

#[cfg(windows)]
mod windows {
    use std::net::SocketAddr;
    use std::os::windows::io::AsRawSocket;
    use std::sync::OnceLock;
    use tokio::net::TcpSocket;
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::Networking::WinSock::{
        SIO_TCP_INITIAL_RTO, TCP_INITIAL_RTO_NO_SYN_RETRANSMISSIONS, TCP_INITIAL_RTO_PARAMETERS,
        WSAIoctl,
    };
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

    /// `TCP_INITIAL_RTO_UNSPECIFIED_RTT` from mstcpip.h, `((USHORT) -1)`,
    /// which windows-sys doesn't carry: keep the system's or the
    /// administrator's round-trip estimate.
    const UNSPECIFIED_RTT: u16 = u16::MAX;

    /// Windows answers a refused connection by sending the SYN again, so a
    /// closed port on this machine takes about two seconds to fail where
    /// other systems fail at once. A loopback SYN can't get lost, so the
    /// resends are turned off there, as Go, libuv, curl, OpenJDK and
    /// Chromium do. This is best effort: if the call fails, connecting still
    /// works, only slower.
    pub(super) fn fail_fast_on_loopback(socket: &TcpSocket, address: SocketAddr) {
        if !address.ip().is_loopback() {
            return;
        }
        let parameters = TCP_INITIAL_RTO_PARAMETERS {
            Rtt: UNSPECIFIED_RTT,
            MaxSynRetransmissions: if accepts_no_syn_retransmissions() {
                // mstcpip.h defines it as `((UCHAR) -2)`; windows-sys widens
                // it to u16.
                TCP_INITIAL_RTO_NO_SYN_RETRANSMISSIONS as u8
            } else {
                1
            },
        };
        let mut returned = 0;
        // SAFETY: the socket is open, the input buffer is a live
        // TCP_INITIAL_RTO_PARAMETERS of the size passed, and the call is
        // synchronous: no output buffer, no OVERLAPPED, no completion routine.
        unsafe {
            WSAIoctl(
                socket.as_raw_socket() as usize,
                SIO_TCP_INITIAL_RTO,
                (&raw const parameters).cast(),
                size_of::<TCP_INITIAL_RTO_PARAMETERS>() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
                None,
            );
        }
    }

    /// Windows 10 1709 (build 16299) is the first to accept "no SYN
    /// retransmissions". Older builds read that value as 254
    /// retransmissions, so they get the smallest count they accept, 1.
    fn accepts_no_syn_retransmissions() -> bool {
        static ACCEPTS: OnceLock<bool> = OnceLock::new();
        *ACCEPTS.get_or_init(|| {
            // SAFETY: OSVERSIONINFOW is plain data, so all zeroes is valid.
            let mut version: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
            version.dwOSVersionInfoSize = size_of::<OSVERSIONINFOW>() as u32;
            // RtlGetVersion reports the real version whatever the
            // application manifest declares.
            // SAFETY: `version` is a live OSVERSIONINFOW with its size set.
            let status = unsafe { RtlGetVersion(&mut version) };
            status == 0 && (version.dwMajorVersion, version.dwBuildNumber) >= (10, 16299)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    fn v4(last: u8) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::new(192, 0, 2, last), 993))
    }

    fn v6(last: u16) -> SocketAddr {
        SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, last), 993))
    }

    #[test]
    fn interleave_alternates_families_starting_with_the_first() {
        assert_eq!(
            interleave(vec![v6(1), v6(2), v4(1), v4(2), v4(3)]),
            [v6(1), v4(1), v6(2), v4(2), v4(3)]
        );
        assert_eq!(interleave(vec![v4(1), v4(2), v6(1)]), [v4(1), v6(1), v4(2)]);
        assert_eq!(interleave(vec![v4(1), v4(2)]), [v4(1), v4(2)]);
        assert!(interleave(Vec::new()).is_empty());
    }

    #[derive(Clone, Copy)]
    enum Outcome {
        Connect(u64),
        Fail(u64, io::ErrorKind),
        FailNow(io::ErrorKind),
        Hang,
    }

    type Attempt = Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>>;

    /// Connection attempts that finish after the given number of
    /// milliseconds, and a log of when each one started and ended.
    #[derive(Clone, Default)]
    struct Script {
        outcomes: Vec<(SocketAddr, Outcome)>,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Script {
        fn new(outcomes: &[(SocketAddr, Outcome)]) -> Self {
            Self {
                outcomes: outcomes.to_vec(),
                ..Self::default()
            }
        }

        fn addresses(&self) -> Vec<SocketAddr> {
            self.outcomes.iter().map(|(address, _)| *address).collect()
        }

        fn attempt(&self, start: Instant, address: SocketAddr) -> Attempt {
            let outcome = self
                .outcomes
                .iter()
                .find(|(candidate, _)| *candidate == address)
                .unwrap()
                .1;
            let log = self.log.clone();
            let note = move |event: &str| {
                let elapsed = start.elapsed().as_millis();
                log.lock()
                    .unwrap()
                    .push(format!("{elapsed} {event} {address}"));
            };
            note("start");
            Box::pin(async move {
                // Notes an attempt that is dropped before it finishes.
                struct LogIfDropped<F: Fn(&str)>(Option<F>);
                impl<F: Fn(&str)> Drop for LogIfDropped<F> {
                    fn drop(&mut self) {
                        if let Some(note) = self.0.take() {
                            note("dropped");
                        }
                    }
                }
                let mut guard = LogIfDropped(Some(note));
                let result = match outcome {
                    Outcome::Connect(after) => {
                        tokio::time::sleep(Duration::from_millis(after)).await;
                        Ok(address)
                    }
                    Outcome::Fail(after, kind) => {
                        tokio::time::sleep(Duration::from_millis(after)).await;
                        Err(io::Error::from(kind))
                    }
                    Outcome::FailNow(kind) => Err(io::Error::from(kind)),
                    Outcome::Hang => std::future::pending().await,
                };
                let note = guard.0.take().unwrap();
                note(match result {
                    Ok(_) => "connected",
                    Err(_) => "failed",
                });
                result
            })
        }

        async fn run(&self) -> (io::Result<SocketAddr>, u128, Vec<String>) {
            let start = Instant::now();
            let result = race(self.addresses(), |address| self.attempt(start, address)).await;
            let elapsed = start.elapsed().as_millis();
            (result, elapsed, self.log.lock().unwrap().clone())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_unanswered_attempt_starts_the_next_address_after_the_delay() {
        let script = Script::new(&[(v6(1), Outcome::Hang), (v4(1), Outcome::Connect(5))]);
        let (result, elapsed, log) = script.run().await;
        assert_eq!(result.unwrap(), v4(1));
        assert_eq!(elapsed, 255);
        assert_eq!(
            log,
            [
                format!("0 start {}", v6(1)),
                format!("250 start {}", v4(1)),
                format!("255 connected {}", v4(1)),
                format!("255 dropped {}", v6(1)),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_attempt_starts_the_next_address_at_once() {
        let script = Script::new(&[
            (v6(1), Outcome::Fail(30, io::ErrorKind::ConnectionRefused)),
            (v4(1), Outcome::Connect(5)),
        ]);
        let (result, elapsed, _) = script.run().await;
        assert_eq!(result.unwrap(), v4(1));
        assert_eq!(elapsed, 35);
    }

    #[tokio::test(start_paused = true)]
    async fn instant_failures_start_the_next_address_at_once() {
        let script = Script::new(&[
            (v6(1), Outcome::Fail(0, io::ErrorKind::NetworkUnreachable)),
            (v6(2), Outcome::Fail(0, io::ErrorKind::NetworkUnreachable)),
            (v4(1), Outcome::Connect(0)),
        ]);
        let (result, elapsed, log) = script.run().await;
        assert_eq!(result.unwrap(), v4(1));
        assert_eq!(elapsed, 0);
        let starts: Vec<_> = log.iter().filter(|line| line.contains(" start ")).collect();
        assert_eq!(starts.len(), 3, "{log:?}");
        assert!(starts.iter().all(|line| line.starts_with("0 ")), "{log:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_connection_wins_and_later_addresses_are_never_tried() {
        let script = Script::new(&[
            (v6(1), Outcome::Connect(400)),
            (v4(1), Outcome::Connect(1000)),
            (v6(2), Outcome::Connect(0)),
        ]);
        let (result, elapsed, log) = script.run().await;
        assert_eq!(result.unwrap(), v6(1));
        assert_eq!(elapsed, 400);
        // The third address was due at 500 ms, after the winner connected.
        assert_eq!(
            log.iter().filter(|line| line.contains(" start ")).count(),
            2
        );
        assert!(
            log.iter()
                .any(|line| line == &format!("400 dropped {}", v4(1))),
            "{log:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn when_every_attempt_fails_the_first_address_error_is_returned() {
        // The first address fails last.
        let script = Script::new(&[
            (v6(1), Outcome::Fail(300, io::ErrorKind::TimedOut)),
            (v4(1), Outcome::Fail(10, io::ErrorKind::ConnectionRefused)),
        ]);
        let (result, elapsed, _) = script.run().await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(elapsed, 300);

        // The first address fails first.
        let script = Script::new(&[
            (v6(1), Outcome::Fail(30, io::ErrorKind::TimedOut)),
            (v4(1), Outcome::Fail(10, io::ErrorKind::ConnectionRefused)),
        ]);
        let (result, elapsed, _) = script.run().await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(elapsed, 40);
    }

    #[tokio::test(start_paused = true)]
    async fn no_addresses_is_an_error() {
        let (result, _, _) = Script::new(&[]).run().await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    /// The first attempt starts on the first poll. Waiting on even an expired
    /// timer first could cost up to a timer tick, about 15 ms on Windows.
    #[tokio::test]
    async fn the_first_attempt_starts_without_waiting_on_a_timer() {
        let script = Script::new(&[(v6(1), Outcome::Hang), (v4(1), Outcome::Hang)]);
        let start = Instant::now();
        let race = race(script.addresses(), |address| script.attempt(start, address));
        let mut race = std::pin::pin!(race);
        assert!(futures::poll!(race.as_mut()).is_pending());
        let log = script.log.lock().unwrap().clone();
        assert_eq!(log.len(), 1, "{log:?}");
        assert!(log[0].ends_with(&format!(" start {}", v6(1))), "{log:?}");
    }

    /// A failed attempt is replaced on the same poll, so the `127.0.0.1`
    /// attempt after a `::1` refusal doesn't wait on a timer either.
    #[tokio::test]
    async fn a_failed_attempt_is_replaced_without_waiting_on_a_timer() {
        let script = Script::new(&[
            (v6(1), Outcome::FailNow(io::ErrorKind::ConnectionRefused)),
            (v4(1), Outcome::Hang),
        ]);
        let start = Instant::now();
        let race = race(script.addresses(), |address| script.attempt(start, address));
        let mut race = std::pin::pin!(race);
        assert!(futures::poll!(race.as_mut()).is_pending());
        let log = script.log.lock().unwrap().clone();
        let starts = log.iter().filter(|line| line.contains(" start ")).count();
        assert_eq!(starts, 2, "{log:?}");
    }

    #[tokio::test]
    async fn connects_to_a_server_that_only_listens_on_ipv4_through_localhost() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let stream = connect_tcp("localhost", port).await.unwrap();
        assert_eq!(stream.peer_addr().unwrap(), listener.local_addr().unwrap());
        assert!(stream.nodelay().unwrap());
    }

    #[tokio::test]
    async fn a_closed_port_is_reported_as_refused() {
        let (port, _port_in_use) = closed_loopback_port().await;
        let error = connect_tcp("127.0.0.1", port).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
    }

    /// Go's TestDialClosedPortFailFast: without the loopback setting,
    /// Windows takes about two seconds to report the refusal.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_closed_loopback_port_fails_fast_on_windows() {
        let (port, _port_in_use) = closed_loopback_port().await;
        // A busy machine can be slow once; Go's test retries too.
        let mut fastest = Duration::MAX;
        for _ in 0..3 {
            let started = std::time::Instant::now();
            let error = connect_tcp("127.0.0.1", port).await.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
            fastest = fastest.min(started.elapsed());
            if fastest < Duration::from_secs(1) {
                return;
            }
        }
        panic!("refusals took {fastest:?} at best");
    }
}
