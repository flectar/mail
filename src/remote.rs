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
mod scheduler;
pub type ResourcePriorities = Arc<Mutex<std::collections::HashSet<(usize, u64)>>>;
pub fn reprioritize_images() {
    scheduler::shared().reprioritize();
}

#[cfg(feature = "remote-content")]
const MAX_REMOTE_RESOURCE_BYTES: usize = 5 * 1024 * 1024;
// CID images are already bounded to 8 MiB by the mail core. Keep the renderer
// in agreement so a valid embedded attachment is not rejected after it has
// been resolved to a data URI.
const MAX_EMBEDDED_RESOURCE_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 4_096;
const MAX_IMAGE_PIXELS: u64 = 8 * 1024 * 1024;
// Modern phone JPEGs commonly exceed the generic raster limits. JPEG IDCT
// scaling reduces these before allocating pixels, so larger sources remain
// safe while PNG/GIF/WebP keep the stricter limits above.
const MAX_JPEG_SOURCE_DIMENSION: u32 = 16_384;
const MAX_JPEG_SOURCE_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_DECODED_DIMENSION: u32 = 2048;
const MAX_DOCUMENT_BYTES: u64 = 20 * 1024 * 1024;
const MAX_DOCUMENT_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_DOCUMENT_REQUESTS: u8 = 64;
const MAX_TRACKED_DOCUMENTS: usize = 128;

pub(crate) fn image_decode_limits() -> blitz_dom::net::ImageDecodeLimits {
    blitz_dom::net::ImageDecodeLimits {
        max_source_dimension: MAX_IMAGE_DIMENSION,
        max_source_pixels: MAX_IMAGE_PIXELS,
        max_jpeg_source_dimension: MAX_JPEG_SOURCE_DIMENSION,
        max_jpeg_source_pixels: MAX_JPEG_SOURCE_PIXELS,
        max_alloc: MAX_IMAGE_PIXELS * 4,
        target_dimension: MAX_DECODED_DIMENSION,
    }
}

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
    scheduler: Arc<scheduler::Scheduler>,
    priorities: ResourcePriorities,
    budgets: Arc<Mutex<HashMap<usize, DocumentBudget>>>,
}

#[cfg(test)]
pub fn email_image_provider_tracked(
    waker: Arc<dyn NetWaker>,
    allow_remote: bool,
    ledger: ResourceLedger,
) -> Result<Arc<dyn NetProvider>, String> {
    email_image_provider_prioritized(waker, allow_remote, ledger, ResourcePriorities::default())
}

pub fn email_image_provider_prioritized(
    waker: Arc<dyn NetWaker>,
    allow_remote: bool,
    ledger: ResourceLedger,
    priorities: ResourcePriorities,
) -> Result<Arc<dyn NetProvider>, String> {
    #[cfg(not(feature = "remote-content"))]
    let _ = allow_remote;
    Ok(Arc::new(LimitedImageProvider {
        ledger,
        #[cfg(feature = "remote-content")]
        allow_remote: allow_remote && cfg!(feature = "remote-content"),
        waker,
        scheduler: scheduler::shared(),
        priorities,
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
        let scheduler = self.scheduler.clone();
        let priorities = self.priorities.clone();
        let budgets = Arc::clone(&self.budgets);
        tokio::spawn(async move {
            let started = crate::renderer::render_timings_enabled().then(std::time::Instant::now);
            let work = async {
                let permit = scheduler
                    .acquire(document_id, request.url.as_str(), embedded, priorities)
                    .await;
                let queued = started.map(|start| start.elapsed());
                // Queueing behind other images must not consume the download
                // deadline. A newsletter can use all 64 request slots.
                let bytes = tokio::time::timeout(std::time::Duration::from_secs(16), async {
                    match request.url.scheme() {
                        "data" => load_data_image(request.url.as_str()),
                        #[cfg(feature = "remote-content")]
                        "http" | "https" => load_http_image(&request).await,
                        _ => None,
                    }
                })
                .await
                .ok()??;
                if let (Some(start), Some(queued)) = (started, queued) {
                    eprintln!(
                        "email image: queue={:.2}ms transfer={:.2}ms bytes={}",
                        queued.as_secs_f64() * 1000.0,
                        start.elapsed().saturating_sub(queued).as_secs_f64() * 1000.0,
                        bytes.len()
                    );
                }
                let signal = request.signal.clone();
                let url = request.url.to_string();
                let ledger = ledger.clone();
                let decode_waker = waker.clone();
                // Decode and the bounded Blitz response queue may block. Keep
                // both off Tokio's async executor and hold the work permit.
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    let decode_start = started.map(|_| std::time::Instant::now());
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
                    // Blocking work is not aborted when its JoinHandle is
                    // dropped. Always wake after delivery, even if the async
                    // caller was cancelled while a decode was finishing.
                    decode_waker.wake(document_id);
                    if let Some(start) = decode_start {
                        eprintln!(
                            "email image decode + delivery: {:.2}ms",
                            start.elapsed().as_secs_f64() * 1000.0
                        );
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
                _ = work => {},
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
            .is_some_and(|length| length > MAX_REMOTE_RESOURCE_BYTES as u64)
    {
        return None;
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let media_type = content_type.split(';').next().unwrap_or("").trim();
    // Some image CDNs return generic or absent MIME types. Accept those only
    // after the raster decoder recognizes and validates the payload below.
    if !(media_type.starts_with("image/")
        || media_type.is_empty()
        || media_type == "application/octet-stream")
        || media_type.starts_with("image/svg")
    {
        return None;
    }

    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or(0)
            .min(MAX_REMOTE_RESOURCE_BYTES as u64) as usize,
    );
    while let Some(chunk) = response.chunk().await.ok()? {
        if signal.is_some_and(|signal| signal.aborted())
            || bytes.len().saturating_add(chunk.len()) > MAX_REMOTE_RESOURCE_BYTES
        {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    validated_pixel_count(&bytes).map(|_| bytes)
}

/// Resolve and pin public addresses before opening the connection. This
/// prevents a hostname or redirect from reaching loopback, LAN, link-local, or
/// metadata services through DNS rebinding.
#[cfg(feature = "remote-content")]
async fn pinned_public_client(url: &reqwest::Url) -> Option<reqwest::Client> {
    if !safe_remote_url_shape(url) {
        return None;
    }
    let port = url.port_or_known_default()?;
    let (mut addresses, resolve_host): (Vec<SocketAddr>, Option<String>) = match url.host()? {
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
    // Keep both address families so the HTTP connector can race/fall back
    // when IPv6 or one CDN endpoint is unreachable. Every address is vetted.
    addresses.sort_unstable();
    addresses.dedup();
    addresses.truncate(32);
    static CLIENTS: std::sync::OnceLock<Mutex<HashMap<String, reqwest::Client>>> =
        std::sync::OnceLock::new();
    let cache = CLIENTS.get_or_init(Default::default);
    let key = format!("{}|{}|{:?}", url.scheme(), url.host_str()?, addresses);
    if let Some(client) = cache.lock().ok()?.get(&key) {
        return Some(client.clone());
    }
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .user_agent("Flectar Mail bounded remote image loader/0.1")
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(12))
        .pool_max_idle_per_host(2)
        .pool_idle_timeout(Duration::from_secs(30))
        .redirect(Policy::none());
    if let Some(host) = resolve_host {
        builder = builder.resolve_to_addrs(&host, &addresses);
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
    if url.len() > MAX_EMBEDDED_RESOURCE_BYTES.saturating_mul(2) {
        return None;
    }
    let data = data_url::DataUrl::process(url).ok()?;
    if data.mime_type().type_ != "image" || data.mime_type().subtype == "svg+xml" {
        return None;
    }
    let (bytes, _) = data.decode_to_vec().ok()?;
    (bytes.len() <= MAX_EMBEDDED_RESOURCE_BYTES).then_some(bytes)
}

fn validated_pixel_count(bytes: &[u8]) -> Option<u64> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let format = reader.format();
    let (max_dimension, max_pixels) = if format == Some(image::ImageFormat::Jpeg) {
        (MAX_JPEG_SOURCE_DIMENSION, MAX_JPEG_SOURCE_PIXELS)
    } else {
        (MAX_IMAGE_DIMENSION, MAX_IMAGE_PIXELS)
    };
    let mut limits = Limits::default();
    limits.max_image_width = Some(max_dimension);
    limits.max_image_height = Some(max_dimension);
    limits.max_alloc = Some(MAX_IMAGE_PIXELS * 4);
    reader.limits(limits);
    let (width, height) = reader.into_dimensions().ok()?;
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    (width > 0
        && height > 0
        && width <= max_dimension
        && height <= max_dimension
        && pixels <= max_pixels)
        .then(|| {
            // Charge a conservative upper bound on retained decoded pixels,
            // rather than the source pixels discarded by downsampling.
            let longest = width.max(height).max(MAX_DECODED_DIMENSION) as u64;
            (u64::from(width) * u64::from(MAX_DECODED_DIMENSION)).div_ceil(longest)
                * (u64::from(height) * u64::from(MAX_DECODED_DIMENSION)).div_ceil(longest)
        })
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
    fn queued_images_keep_their_download_deadline() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let scheduler = scheduler::Scheduler::new(1, 1);
            let held = scheduler
                .clone()
                .acquire(9, "held", true, ResourcePriorities::default())
                .await;
            let ledger = ResourceLedger::default();
            let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (done, mut completions) = tokio::sync::mpsc::unbounded_channel();
            let provider = LimitedImageProvider {
                ledger: ledger.clone(),
                #[cfg(feature = "remote-content")]
                allow_remote: false,
                waker: Arc::new(move |_| {
                    let _ = done.send(());
                }),
                scheduler,
                priorities: ResourcePriorities::default(),
                budgets: Arc::default(),
            };
            provider.fetch(
                9,
                Request::get(url::Url::parse(PIXEL).unwrap()),
                Box::new(Counter(count.clone())),
            );
            // Deliberately longer than the production transfer deadline.
            tokio::time::sleep(std::time::Duration::from_secs(17)).await;
            assert_eq!(
                ledger.lock().unwrap().get(&resource_key(9, PIXEL)),
                Some(&ResourceState::Loading)
            );
            drop(held);
            tokio::time::timeout(std::time::Duration::from_secs(3), completions.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(
                ledger.lock().unwrap().get(&resource_key(9, PIXEL)),
                Some(&ResourceState::Ready)
            );
        });
    }

    #[test]
    fn pixel_budget_covers_retained_downsampled_images() {
        for (width, height) in [(3000, 301), (301, 3000), (2049, 3), (17, 29)] {
            for format in [image::ImageFormat::Png, image::ImageFormat::Jpeg] {
                let mut bytes = Cursor::new(Vec::new());
                image::RgbImage::new(width, height)
                    .write_to(&mut bytes, format)
                    .unwrap();
                let budget = validated_pixel_count(bytes.get_ref()).unwrap();
                let decoded = image_decode_limits().decode(bytes.get_ref()).unwrap();
                let retained = u64::from(decoded.pixel_width) * u64::from(decoded.pixel_height);
                assert!(
                    budget >= retained,
                    "{width}x{height} {format:?}: {budget} < {retained}"
                );
                assert!(budget <= u64::from(width) * u64::from(height));
            }
        }
    }

    #[test]
    fn full_resolution_phone_jpeg_is_admitted_and_downsampled() {
        let mut bytes = Cursor::new(Vec::new());
        image::RgbImage::new(4032, 3024)
            .write_to(&mut bytes, image::ImageFormat::Jpeg)
            .unwrap();

        let budget = validated_pixel_count(bytes.get_ref())
            .expect("a common 12 MP phone JPEG should fit the bounded JPEG path");
        let decoded = image_decode_limits().decode(bytes.get_ref()).unwrap();

        assert!(decoded.pixel_width <= MAX_DECODED_DIMENSION);
        assert!(decoded.pixel_height <= MAX_DECODED_DIMENSION);
        assert!(
            budget >= u64::from(decoded.pixel_width) * u64::from(decoded.pixel_height)
        );
    }

    #[test]
    fn raster_limits_reject_oversized_dimensions_and_non_images() {
        let mut bytes = Cursor::new(Vec::new());
        image::RgbaImage::new(1, MAX_IMAGE_DIMENSION + 1)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        assert!(validated_pixel_count(bytes.get_ref()).is_none());
        assert!(validated_pixel_count(b"<html>error</html>").is_none());
        assert_eq!(
            validated_pixel_count(&load_data_image(PIXEL).unwrap()),
            Some(1)
        );
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
            (
                "200 OK\r\nContent-Type: image/png",
                load_data_image(PIXEL).unwrap(),
                true,
            ),
            (
                "200 OK\r\nContent-Type: application/octet-stream",
                load_data_image(PIXEL).unwrap(),
                true,
            ),
            ("200 OK", load_data_image(PIXEL).unwrap(), true),
            ("200 OK\r\nContent-Type: image/png", vec![1, 2, 3], false),
            (
                "200 OK\r\nContent-Type: application/octet-stream",
                b"<svg/>".to_vec(),
                false,
            ),
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
