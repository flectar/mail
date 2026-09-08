//! Bounded network provider for images embedded in email HTML.
//!
//! Embedded `data:` images are always available because they are already part
//! of the message. HTTP(S) loading is a separate, feature- and consent-gated
//! capability with strict byte/pixel budgets and public-address pinning.

use blitz_traits::net::{Bytes, NetHandler, NetProvider, NetWaker, Request};
use image::{ImageReader, Limits};
#[cfg(feature = "remote-content")]
use reqwest::redirect::Policy;
use std::{
    collections::HashMap,
    io::Cursor,
    sync::{Arc, Mutex},
};
#[cfg(feature = "remote-content")]
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::sync::Semaphore;

const MAX_RESOURCE_BYTES: usize = 5 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 4_096;
const MAX_IMAGE_PIXELS: u64 = 8 * 1024 * 1024;
const MAX_DOCUMENT_BYTES: u64 = 20 * 1024 * 1024;
const MAX_DOCUMENT_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_DOCUMENT_REQUESTS: u8 = 32;
const MAX_CONCURRENT_REQUESTS: usize = 4;
const MAX_TRACKED_DOCUMENTS: usize = 128;

#[derive(Default)]
struct DocumentBudget {
    requests: u8,
    embedded_requests: u8,
    bytes: u64,
    pixels: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceState {
    Loading,
    Ready,
    Blocked,
    Failed,
    Limited,
}
pub type ResourceLedger = Arc<Mutex<HashMap<(usize, u64), ResourceState>>>;
pub fn resource_key(id: usize, url: &str) -> (usize, u64) {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hash);
    (id, hash.finish())
}
fn record(ledger: &ResourceLedger, id: usize, url: &str, state: ResourceState) {
    let mut map = ledger
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if map.len() >= 4096 {
        map.clear();
    }
    map.insert(resource_key(id, url), state);
}

struct LimitedImageProvider {
    ledger: ResourceLedger,
    #[cfg(feature = "remote-content")]
    allow_remote: bool,
    waker: Arc<dyn NetWaker>,
    permits: Arc<Semaphore>,
    budgets: Arc<Mutex<HashMap<usize, DocumentBudget>>>,
}

pub fn email_image_provider_tracked(
    waker: Arc<dyn NetWaker>,
    allow_remote: bool,
    ledger: ResourceLedger,
) -> Result<Arc<dyn NetProvider>, String> {
    #[cfg(not(feature = "remote-content"))]
    let _ = allow_remote;
    static IMAGE_WORK: std::sync::OnceLock<Arc<Semaphore>> = std::sync::OnceLock::new();
    Ok(Arc::new(LimitedImageProvider {
        ledger,
        #[cfg(feature = "remote-content")]
        allow_remote: allow_remote && cfg!(feature = "remote-content"),
        waker,
        permits: IMAGE_WORK
            .get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS)))
            .clone(),
        budgets: Arc::new(Mutex::new(HashMap::new())),
    }))
}

impl NetProvider for LimitedImageProvider {
    fn fetch(&self, document_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let embedded = request.url.scheme() == "data";
        let permitted = (embedded
            && request
                .url
                .as_str()
                .get(..11)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:image/")))
            || {
                #[cfg(feature = "remote-content")]
                {
                    self.allow_remote && safe_remote_url_shape(&request.url)
                }
                #[cfg(not(feature = "remote-content"))]
                {
                    false
                }
            };
        // Rejected trackers must not spend the embedded-image allowance.
        if request.method != blitz_traits::net::http::Method::GET
            || !permitted
            || request.signal.as_ref().is_some_and(|s| s.aborted())
            || !reserve_request(&self.budgets, document_id, embedded)
        {
            record(
                &self.ledger,
                document_id,
                request.url.as_str(),
                if permitted {
                    ResourceState::Limited
                } else {
                    ResourceState::Blocked
                },
            );
            self.waker.wake(document_id);
            return;
        }
        record(
            &self.ledger,
            document_id,
            request.url.as_str(),
            ResourceState::Loading,
        );
        let ledger = self.ledger.clone();
        let waker = Arc::clone(&self.waker);
        let permits = Arc::clone(&self.permits);
        let budgets = Arc::clone(&self.budgets);
        tokio::spawn(async move {
            let work = async {
                let permit = permits.acquire_owned().await.ok()?;
                let bytes = match request.url.scheme() {
                    "data" => load_data_image(request.url.as_str()),
                    #[cfg(feature = "remote-content")]
                    "http" | "https" => load_http_image(&request).await,
                    _ => None,
                }?;
                let signal = request.signal.clone();
                let url = request.url.to_string();
                let ledger = ledger.clone();
                // Decode and the bounded Blitz response queue may block. Keep
                // both off Tokio's async executor and hold the work permit.
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        return;
                    }
                    if let Some(pixels) = validated_pixel_count(&bytes)
                        && reserve_image(&budgets, document_id, bytes.len() as u64, pixels)
                    {
                        let decoded =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                handler.bytes(url.clone(), Bytes::from(bytes));
                            }));
                        record(
                            &ledger,
                            document_id,
                            &url,
                            if decoded.is_ok() {
                                ResourceState::Ready
                            } else {
                                ResourceState::Failed
                            },
                        );
                    } else {
                        record(&ledger, document_id, &url, ResourceState::Limited);
                    }
                })
                .await
                .ok()
            };
            let cancelled = async {
                loop {
                    if request.signal.as_ref().is_some_and(|s| s.aborted()) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            };
            tokio::select! {
                _ = cancelled => {},
                _ = tokio::time::timeout(std::time::Duration::from_secs(16), work) => {},
            }
            let mut states = ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if states.get(&resource_key(document_id, request.url.as_str()))
                == Some(&ResourceState::Loading)
            {
                states.insert(
                    resource_key(document_id, request.url.as_str()),
                    ResourceState::Failed,
                );
            }
            drop(states);
            waker.wake(document_id);
        });
    }
}

#[cfg(feature = "remote-content")]
async fn load_http_image(request: &Request) -> Option<Vec<u8>> {
    if request
        .signal
        .as_ref()
        .is_some_and(|signal| signal.aborted())
    {
        return None;
    }
    let mut url = request.url.clone();
    let mut redirects = 0_u8;
    let response = loop {
        let client = pinned_public_client(&url).await?;
        let response = client.get(url.clone()).send().await.ok()?;
        if !response.status().is_redirection() {
            break response;
        }
        if redirects >= 3 {
            return None;
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())?;
        url = url.join(location).ok()?;
        redirects += 1;
    };
    consume_http_image(response, request.signal.as_ref()).await
}

#[cfg(feature = "remote-content")]
async fn consume_http_image(
    mut response: reqwest::Response,
    signal: Option<&blitz_traits::net::AbortSignal>,
) -> Option<Vec<u8>> {
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|length| length > MAX_RESOURCE_BYTES as u64)
    {
        return None;
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !content_type.starts_with("image/") || content_type.starts_with("image/svg") {
        return None;
    }

    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or(0)
            .min(MAX_RESOURCE_BYTES as u64) as usize,
    );
    while let Some(chunk) = response.chunk().await.ok()? {
        if signal.is_some_and(|signal| signal.aborted())
            || bytes.len().saturating_add(chunk.len()) > MAX_RESOURCE_BYTES
        {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    Some(bytes)
}

/// Resolve and pin one public address before opening the connection. This
/// prevents a hostname or redirect from reaching loopback, LAN, link-local, or
/// metadata services through DNS rebinding.
#[cfg(feature = "remote-content")]
async fn pinned_public_client(url: &reqwest::Url) -> Option<reqwest::Client> {
    if !safe_remote_url_shape(url) {
        return None;
    }
    let port = url.port_or_known_default()?;
    let (addresses, resolve_host): (Vec<SocketAddr>, Option<String>) = match url.host()? {
        url::Host::Domain(host) => (
            tokio::net::lookup_host((host, port)).await.ok()?.collect(),
            Some(host.to_owned()),
        ),
        url::Host::Ipv4(address) => (vec![SocketAddr::new(address.into(), port)], None),
        url::Host::Ipv6(address) => (vec![SocketAddr::new(address.into(), port)], None),
    };
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| !safe_remote_ip(address.ip()))
    {
        return None;
    }
    let pinned = addresses[0];
    static CLIENTS: std::sync::OnceLock<Mutex<HashMap<String, reqwest::Client>>> =
        std::sync::OnceLock::new();
    let cache = CLIENTS.get_or_init(Default::default);
    let key = format!("{}|{}|{}", url.scheme(), url.host_str()?, pinned);
    if let Some(client) = cache.lock().ok()?.get(&key) {
        return Some(client.clone());
    }
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .user_agent("Flectar Mail bounded remote image loader/0.1")
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(12))
        .redirect(Policy::none());
    if let Some(host) = resolve_host {
        builder = builder.resolve(&host, pinned);
    }
    let client = builder.build().ok()?;
    let mut clients = cache.lock().ok()?;
    if clients.len() >= 32 {
        clients.clear();
    }
    clients.insert(key, client.clone());
    Some(client)
}

fn load_data_image(url: &str) -> Option<Vec<u8>> {
    // Base64 overhead is 4/3; this pre-check bounds allocation even before
    // the streaming decoder sees the data.
    if url.len() > MAX_RESOURCE_BYTES.saturating_mul(2) {
        return None;
    }
    let data = data_url::DataUrl::process(url).ok()?;
    if data.mime_type().type_ != "image" || data.mime_type().subtype == "svg+xml" {
        return None;
    }
    let (bytes, _) = data.decode_to_vec().ok()?;
    (bytes.len() <= MAX_RESOURCE_BYTES).then_some(bytes)
}

fn validated_pixel_count(bytes: &[u8]) -> Option<u64> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_PIXELS * 4);
    reader.limits(limits);
    let (width, height) = reader.into_dimensions().ok()?;
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    (width > 0
        && height > 0
        && width <= MAX_IMAGE_DIMENSION
        && height <= MAX_IMAGE_DIMENSION
        && pixels <= MAX_IMAGE_PIXELS)
        .then_some(pixels)
}

fn reserve_request(
    budgets: &Mutex<HashMap<usize, DocumentBudget>>,
    document_id: usize,
    embedded: bool,
) -> bool {
    // A panic in unrelated remote-image work must not turn every later image
    // request into a second panic. The counters remain conservative if a
    // poisoned lock is recovered.
    let mut budgets = budgets
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if budgets.len() >= MAX_TRACKED_DOCUMENTS && !budgets.contains_key(&document_id) {
        budgets.clear();
    }
    let budget = budgets.entry(document_id).or_default();
    let requests = if embedded {
        &mut budget.embedded_requests
    } else {
        &mut budget.requests
    };
    if *requests >= MAX_DOCUMENT_REQUESTS {
        return false;
    }
    *requests += 1;
    true
}

fn reserve_image(
    budgets: &Mutex<HashMap<usize, DocumentBudget>>,
    document_id: usize,
    bytes: u64,
    pixels: u64,
) -> bool {
    let mut budgets = budgets
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let budget = budgets.entry(document_id).or_default();
    let Some(total_bytes) = budget.bytes.checked_add(bytes) else {
        return false;
    };
    let Some(total_pixels) = budget.pixels.checked_add(pixels) else {
        return false;
    };
    if total_bytes > MAX_DOCUMENT_BYTES || total_pixels > MAX_DOCUMENT_PIXELS {
        return false;
    }
    budget.bytes = total_bytes;
    budget.pixels = total_pixels;
    true
}

#[cfg(feature = "remote-content")]
fn safe_remote_url_shape(url: &reqwest::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return false;
    }
    {
        let Some(port) = url.port_or_known_default() else {
            return false;
        };
        if !matches!((url.scheme(), port), ("http", 80) | ("https", 443)) {
            return false;
        }
    }
    match url.host() {
        Some(url::Host::Ipv4(address)) => safe_remote_ip(address.into()),
        Some(url::Host::Ipv6(address)) => safe_remote_ip(address.into()),
        Some(url::Host::Domain(_)) => true,
        None => false,
    }
}

#[cfg(feature = "remote-content")]
fn safe_remote_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_documentation()
                || octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 198 && matches!(octets[1], 18 | 19))
                || octets[0] >= 240)
        }
        IpAddr::V6(ip) => {
            !(ip.is_loopback()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.segments()[..2] == [0x2001, 0x0db8])
                && ip
                    .to_ipv4_mapped()
                    .is_none_or(|mapped| safe_remote_ip(mapped.into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_budget_is_bounded() {
        let budgets = Mutex::new(HashMap::new());
        assert!(reserve_image(
            &budgets,
            7,
            MAX_DOCUMENT_BYTES,
            MAX_DOCUMENT_PIXELS
        ));
        assert!(!reserve_image(&budgets, 7, 1, 1));
        for _ in 0..MAX_DOCUMENT_REQUESTS {
            assert!(reserve_request(&budgets, 8, false));
        }
        assert!(!reserve_request(&budgets, 8, false));
        assert!(reserve_request(&budgets, 8, true));
    }

    #[test]
    #[cfg(feature = "remote-content")]
    fn remote_url_shape_rejects_credentials() {
        let url = reqwest::Url::parse("https://user:secret@example.com/pixel.png").unwrap();
        assert!(!safe_remote_url_shape(&url));
    }
    const PIXEL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    struct Counter(Arc<std::sync::atomic::AtomicUsize>);
    impl NetHandler for Counter {
        fn bytes(self: Box<Self>, _: String, _: Bytes) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    #[test]
    fn blocked_trackers_leave_embedded_budget_available_and_cancelled_requests_do_not_decode() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let ledger = ResourceLedger::default();
            let provider =
                email_image_provider_tracked(Arc::new(|_| {}), false, ledger.clone()).unwrap();
            for i in 0..64 {
                provider.fetch(
                    1,
                    Request::get(url::Url::parse(&format!("https://example.com/{i}.png")).unwrap()),
                    Box::new(Counter(count.clone())),
                );
            }
            provider.fetch(
                1,
                Request::get(url::Url::parse(PIXEL).unwrap()),
                Box::new(Counter(count.clone())),
            );
            let abort = blitz_traits::net::AbortController::default();
            let signal = abort.signal.clone();
            abort.abort();
            provider.fetch(
                2,
                Request::get(url::Url::parse(PIXEL).unwrap()).signal(signal),
                Box::new(Counter(count.clone())),
            );
            for _ in 0..100 {
                if count.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(
                ledger.lock().unwrap().get(&resource_key(1, PIXEL)),
                Some(&ResourceState::Ready)
            );
        });
    }
    #[test]
    #[cfg(feature = "remote-content")]
    fn production_url_and_ip_policy_cannot_be_disabled_in_tests() {
        for value in [
            "http://127.0.0.1/x",
            "http://10.0.0.1/x",
            "http://169.254.169.254/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "https://example.com:8443/a",
            "file:///tmp/pixel.png",
        ] {
            assert!(
                !safe_remote_url_shape(&url::Url::parse(value).unwrap()),
                "{value}"
            );
        }
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!safe_remote_ip(value.parse().unwrap()), "{value}");
        }
        assert!(safe_remote_url_shape(
            &url::Url::parse("https://example.com/pixel.png").unwrap()
        ));
    }
    #[test]
    #[cfg(feature = "remote-content")]
    fn http_body_checks_content_type_status_bytes_and_abort() {
        use std::io::{Read, Write};
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Only this transport fixture uses localhost; the provider and its
        // production resolver are tested separately with their policy intact.
        for (header, body, accepted) in [
            ("200 OK\r\nContent-Type: image/png", vec![1, 2, 3], true),
            ("404 Not Found\r\nContent-Type: image/png", vec![1], false),
            ("200 OK\r\nContent-Type: text/html", vec![1], false),
            ("200 OK\r\nContent-Type: image/svg+xml", vec![1], false),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut request = [0; 1024];
                let _ = stream.read(&mut request);
                write!(
                    stream,
                    "HTTP/1.1 {header}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            });
            let actual = rt.block_on(async {
                let response = reqwest::Client::builder()
                    .no_proxy()
                    .timeout(std::time::Duration::from_secs(2))
                    .build()
                    .unwrap()
                    .get(format!("http://{addr}/pixel"))
                    .send()
                    .await
                    .unwrap();
                consume_http_image(response, None).await.is_some()
            });
            server.join().unwrap();
            assert_eq!(actual, accepted);
        }
    }
}
