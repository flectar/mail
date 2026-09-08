use image::{DynamicImage, ImageReader, Limits, Rgba, RgbaImage, imageops::FilterType};
use reqwest::{Client, Url, header::CONTENT_TYPE};
use sha2::{Digest, Sha256};
use std::{
    io::Cursor,
    net::IpAddr,
    path::PathBuf,
    time::{Duration, SystemTime},
};
use tokio::io::AsyncReadExt;

// Twenty serves a larger source than the largest normal avatar needs. The
// final raster is produced at the exact physical size Slint will paint (see
// `physical_pixel_side`), because Slint's software renderer otherwise uses
// nearest-neighbour sampling for the final scale operation.
const BRAND_CONTENT_NUMERATOR: u32 = 46;
const BRAND_CONTENT_DENOMINATOR: u32 = 64;
const SERVICE_ICON_SIZE: u32 = 128;
const MAX_ICON_BYTES: u64 = 256 * 1024;
const MAX_PROFILE_BYTES: u64 = 1024 * 1024;
const MAX_ICON_DIMENSION: u32 = 2_048;
const MAX_ICON_DECODE_BYTES: u64 = 16 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const PROFILE_CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MISSING_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// A small, UI-independent RGBA image. Keeping Slint out of this module lets
/// the network and image work stay on the Tokio worker threads.
#[derive(Clone, Debug)]
pub struct FaviconImage {
    pub width: u32,
    pub height: u32,
    pub pixels: std::sync::Arc<[u8]>,
}

#[derive(Clone, Debug)]
pub struct FaviconImages {
    pub small: FaviconImage,
    pub regular: FaviconImage,
}

fn trusted_service_url(url: &Url, service_domain: &str) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let is_service_host = host.eq_ignore_ascii_case(service_domain)
        || host
            .strip_suffix(service_domain)
            .is_some_and(|prefix| prefix.ends_with('.'));
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && is_service_host
}

fn service_redirect_policy(service_domain: &'static str) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > 3 || !trusted_service_url(attempt.url(), service_domain) {
            attempt.stop()
        } else {
            attempt.follow()
        }
    })
}

#[derive(Clone, Debug)]
pub struct ProfileAvatarImages {
    pub small: FaviconImage,
    pub regular: FaviconImage,
}

#[derive(Clone)]
pub struct FaviconLoader {
    client: Client,
    cache_dir: PathBuf,
}

impl FaviconLoader {
    pub fn new() -> Result<Self, String> {
        let client = Client::builder()
            .user_agent("Flectar Mail native sender icon/0.1")
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .redirect(service_redirect_policy("twenty-icons.com"))
            .build()
            .map_err(|error| format!("could not create favicon client: {error}"))?;

        let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
        let cache_dir = base.join("flectar-mail").join("sender-icons");
        std::fs::create_dir_all(&cache_dir)
            .map_err(|error| format!("could not create favicon cache: {error}"))?;

        Ok(Self { client, cache_dir })
    }

    /// Fetch a sender brand icon from the Twenty favicon service, using the
    /// local cache first. Brand domains are tried before exact mail hosts, so
    /// `mail.instagram.com` resolves through `instagram.com` and
    /// `support.facebook.com` through `facebook.com`.
    pub async fn load(
        &self,
        domain: &str,
        small_pixel_side: u32,
        regular_pixel_side: u32,
    ) -> Option<FaviconImages> {
        let cache_path = self.cache_path(domain);
        if let Some(bytes) = read_fresh(&cache_path, CACHE_TTL, MAX_ICON_BYTES).await {
            match decode_brand_icons(bytes, small_pixel_side, regular_pixel_side).await {
                Ok(icons) => return Some(icons),
                Err(_) => {
                    let _ = tokio::fs::remove_file(&cache_path).await;
                }
            }
        }

        let missing_path = self.missing_path(domain);
        if is_fresh(&missing_path, MISSING_TTL).await {
            return None;
        }

        for candidate in icon_domain_candidates(domain) {
            let Some(bytes) = self.fetch_icon_bytes(&candidate).await else {
                continue;
            };
            let Ok(icons) =
                decode_brand_icons(bytes.clone(), small_pixel_side, regular_pixel_side).await
            else {
                continue;
            };

            // Cache the resolved brand asset under the original sender host.
            // Later rows therefore avoid both the exact-host and parent lookup.
            let _ = tokio::fs::write(&cache_path, bytes).await;
            let _ = tokio::fs::remove_file(&missing_path).await;
            return Some(icons);
        }

        mark_missing(&missing_path).await;
        None
    }

    async fn fetch_icon_bytes(&self, domain: &str) -> Option<Vec<u8>> {
        let url = format!("https://twenty-icons.com/{domain}/{SERVICE_ICON_SIZE}");
        let response = self.client.get(url).send().await.ok()?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > MAX_ICON_BYTES)
        {
            return None;
        }
        if let Some(content_type) = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            && !content_type.to_ascii_lowercase().starts_with("image/")
        {
            return None;
        }
        response_bytes_bounded(response, MAX_ICON_BYTES).await
    }

    fn cache_path(&self, domain: &str) -> PathBuf {
        self.cache_dir
            .join(format!("{}.brand-v4.icon", cache_key(domain)))
    }

    fn missing_path(&self, domain: &str) -> PathBuf {
        self.cache_dir
            .join(format!("{}.brand-v4.missing", cache_key(domain)))
    }
}

#[derive(Clone)]
pub struct ProfileAvatarLoader {
    client: Client,
    cache_dir: PathBuf,
}

impl ProfileAvatarLoader {
    pub fn new() -> Result<Self, String> {
        let client = Client::builder()
            .user_agent("Flectar Mail native account avatar/0.1")
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .redirect(service_redirect_policy("googleusercontent.com"))
            .build()
            .map_err(|error| format!("could not create account-avatar client: {error}"))?;
        let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
        let cache_dir = base.join("flectar-mail").join("account-avatars");
        std::fs::create_dir_all(&cache_dir)
            .map_err(|error| format!("could not create account-avatar cache: {error}"))?;
        Ok(Self { client, cache_dir })
    }

    pub async fn load(
        &self,
        source_url: &str,
        small_pixel_side: u32,
        regular_pixel_side: u32,
    ) -> Option<ProfileAvatarImages> {
        let source_url = trusted_profile_avatar_url(source_url)?;
        let cache_path = self.cache_dir.join(format!(
            "{}.profile-v1.icon",
            cache_key(source_url.as_str())
        ));
        if let Some(bytes) = read_fresh(&cache_path, PROFILE_CACHE_TTL, MAX_PROFILE_BYTES).await {
            match decode_profile_avatars(bytes, small_pixel_side, regular_pixel_side).await {
                Ok(images) => return Some(images),
                Err(_) => {
                    let _ = tokio::fs::remove_file(&cache_path).await;
                }
            }
        }

        let response = self.client.get(source_url).send().await.ok()?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > MAX_PROFILE_BYTES)
        {
            return None;
        }
        if let Some(content_type) = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            && !content_type.to_ascii_lowercase().starts_with("image/")
        {
            return None;
        }
        let bytes = response_bytes_bounded(response, MAX_PROFILE_BYTES).await?;
        let images = decode_profile_avatars(bytes.clone(), small_pixel_side, regular_pixel_side)
            .await
            .ok()?;
        let _ = tokio::fs::write(cache_path, bytes).await;
        Some(images)
    }
}

async fn response_bytes_bounded(
    mut response: reqwest::Response,
    max_bytes: u64,
) -> Option<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        return None;
    }
    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(8 * 1024)
        .min(usize::try_from(max_bytes).unwrap_or(usize::MAX));
    let mut bytes = Vec::with_capacity(capacity);
    while let Some(chunk) = response.chunk().await.ok()? {
        if !append_response_chunk(&mut bytes, &chunk, max_bytes) {
            return None;
        }
    }
    Some(bytes)
}

fn append_response_chunk(bytes: &mut Vec<u8>, chunk: &[u8], max_bytes: u64) -> bool {
    if chunk.len() as u64 > max_bytes.saturating_sub(bytes.len() as u64) {
        return false;
    }
    bytes.extend_from_slice(chunk);
    true
}

pub fn physical_pixel_side(logical_side: f32, scale_factor: f32) -> u32 {
    (logical_side * scale_factor.max(1.0))
        .round()
        .clamp(1.0, 512.0) as u32
}

fn trusted_profile_avatar_url(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    trusted_service_url(&url, "googleusercontent.com").then_some(url)
}

fn icon_domain_candidates(domain: &str) -> Vec<String> {
    let labels: Vec<&str> = domain
        .trim_matches('.')
        .split('.')
        .filter(|label| !label.is_empty())
        .collect();
    if labels.len() <= 2 {
        return (!labels.is_empty())
            .then(|| labels.join("."))
            .into_iter()
            .collect();
    }

    // Preserve the registrant label for common country-code public suffixes
    // such as co.uk/com.au without pulling a PSL into this tiny UI helper.
    let second_level_suffix = matches!(
        labels[labels.len() - 2],
        "ac" | "co" | "com" | "edu" | "gov" | "net" | "org"
    ) && labels.last().is_some_and(|tld| tld.len() == 2);
    let brand_label_count = if second_level_suffix { 3 } else { 2 };
    let brand = labels[labels.len() - brand_label_count..].join(".");
    let exact = labels.join(".");
    if brand == exact {
        vec![exact]
    } else {
        vec![brand, exact]
    }
}

/// Return a safe, lower-case DNS host from an RFC-style email address.
/// Numeric hosts are deliberately ignored: they do not represent useful
/// sender-brand domains and should not cause arbitrary local network fetches.
pub fn domain_from_address(address: &str) -> Option<String> {
    let (_, raw_domain) = address.rsplit_once('@')?;
    let raw_domain = raw_domain
        .trim()
        .trim_end_matches('>')
        .trim()
        .trim_end_matches('.');
    if raw_domain.is_empty() {
        return None;
    }

    let url = Url::parse(&format!("https://{raw_domain}/")).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    if host.parse::<IpAddr>().is_ok() {
        return None;
    }
    Some(host)
}

async fn decode_brand_icons(
    bytes: Vec<u8>,
    small_pixel_side: u32,
    regular_pixel_side: u32,
) -> Result<FaviconImages, String> {
    tokio::task::spawn_blocking(move || {
        let decoded = decode_bounded(bytes)?;
        let flattened = flatten_on_white(decoded);
        Ok(FaviconImages {
            small: render_brand_icon(&flattened, small_pixel_side),
            regular: render_brand_icon(&flattened, regular_pixel_side),
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

fn render_brand_icon(flattened: &DynamicImage, pixel_side: u32) -> FaviconImage {
    let content_side = centered_brand_content_side(pixel_side);
    let fitted = flattened.resize(content_side, content_side, FilterType::Lanczos3);

    // Brand marks are authored for light surfaces. Flattening their alpha
    // into white before the one high-quality resize avoids dark fringes.
    // Bake the circular clip into the raster as well: the software renderer
    // does not reliably clip an Image to its parent's rounded corners.
    let mut canvas = RgbaImage::from_pixel(pixel_side, pixel_side, Rgba([255, 255, 255, 255]));
    let x = i64::from((pixel_side - fitted.width()) / 2);
    let y = i64::from((pixel_side - fitted.height()) / 2);
    image::imageops::overlay(&mut canvas, &fitted, x, y);
    apply_circular_alpha_mask(&mut canvas);

    FaviconImage {
        width: pixel_side,
        height: pixel_side,
        pixels: canvas.into_raw().into(),
    }
}

/// Pick the nearest brand-mark size that leaves equal whole-pixel margins.
///
/// The software renderer needs an exact-size raster, so its final composition
/// also happens in physical pixels. An odd mark inside an even avatar (or the
/// reverse) has a half-pixel center; integer overlay used to resolve that
/// toward the top-left. Matching the parity keeps every brand geometrically
/// centered without brand-specific offsets or another resampling pass.
fn centered_brand_content_side(pixel_side: u32) -> u32 {
    let scaled_side = pixel_side * BRAND_CONTENT_NUMERATOR;
    let rounded = ((scaled_side + BRAND_CONTENT_DENOMINATOR / 2) / BRAND_CONTENT_DENOMINATOR)
        .clamp(1, pixel_side);
    if (pixel_side - rounded).is_multiple_of(2) {
        return rounded;
    }

    [
        rounded.saturating_sub(1).max(1),
        (rounded + 1).min(pixel_side),
    ]
    .into_iter()
    .filter(|candidate| (pixel_side - candidate).is_multiple_of(2))
    .min_by_key(|candidate| (candidate * BRAND_CONTENT_DENOMINATOR).abs_diff(scaled_side))
    .unwrap_or(rounded)
}

async fn decode_profile_avatars(
    bytes: Vec<u8>,
    small_pixel_side: u32,
    regular_pixel_side: u32,
) -> Result<ProfileAvatarImages, String> {
    tokio::task::spawn_blocking(move || {
        let decoded = decode_bounded(bytes)?;
        Ok(ProfileAvatarImages {
            small: resize_profile(&decoded, small_pixel_side),
            regular: resize_profile(&decoded, regular_pixel_side),
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

fn decode_bounded(bytes: Vec<u8>) -> Result<DynamicImage, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_ICON_DIMENSION);
    limits.max_image_height = Some(MAX_ICON_DIMENSION);
    limits.max_alloc = Some(MAX_ICON_DECODE_BYTES);
    reader.limits(limits);
    reader.decode().map_err(|error| error.to_string())
}

fn flatten_on_white(image: DynamicImage) -> DynamicImage {
    let mut image = image.to_rgba8();
    for pixel in image.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = ((u32::from(*channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
        }
        pixel[3] = 255;
    }
    DynamicImage::ImageRgba8(image)
}

/// Apply a one-pixel antialiased circle to an exact-size avatar raster.
///
/// The half-pixel inset keeps the outermost edge blended even at very small
/// avatar sizes, while leaving the center fully opaque.
fn apply_circular_alpha_mask(image: &mut RgbaImage) {
    let center_x = (image.width() as f32 - 1.0) / 2.0;
    let center_y = (image.height() as f32 - 1.0) / 2.0;
    let radius = (image.width().min(image.height()) as f32 - 1.0) / 2.0;

    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let dx = x as f32 - center_x;
        let dy = y as f32 - center_y;
        let distance = (dx * dx + dy * dy).sqrt();
        let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0);
        pixel[3] = (f32::from(pixel[3]) * coverage).round() as u8;
    }
}

fn resize_profile(image: &DynamicImage, pixel_side: u32) -> FaviconImage {
    let mut image = image
        .resize_to_fill(pixel_side, pixel_side, FilterType::Lanczos3)
        .into_rgba8();
    // Account photos use the same pixel-level circle as sender brands. This
    // avoids square corners in places where the software renderer does not
    // clip an Image to the Avatar component's rounded bounds.
    apply_circular_alpha_mask(&mut image);
    FaviconImage {
        width: pixel_side,
        height: pixel_side,
        pixels: image.into_raw().into(),
    }
}

fn cache_key(domain: &str) -> String {
    let digest = Sha256::digest(domain.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn read_fresh(path: &PathBuf, ttl: Duration, max_bytes: u64) -> Option<Vec<u8>> {
    if !is_fresh(path, ttl).await {
        return None;
    }
    let file = tokio::fs::File::open(path).await.ok()?;
    let mut bytes = Vec::with_capacity(usize::try_from(max_bytes).ok()?);
    if file
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .await
        .is_err()
        || bytes.len() as u64 > max_bytes
    {
        return None;
    }
    Some(bytes)
}

async fn is_fresh(path: &PathBuf, ttl: Duration) -> bool {
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age <= ttl)
}

async fn mark_missing(path: &PathBuf) {
    let _ = tokio::fs::write(path, b"").await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_brand_png() -> Vec<u8> {
        let mut image = RgbaImage::from_pixel(128, 128, Rgba([0, 0, 0, 0]));
        for y in 0_i32..128 {
            for x in 0_i32..128 {
                if (x - 64).pow(2) + (y - 64).pow(2) <= 50_i32.pow(2) {
                    image.put_pixel(x as u32, y as u32, Rgba([255, 0, 0, 255]));
                }
            }
        }
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn extracts_normalized_domain() {
        assert_eq!(
            domain_from_address("Sender <News@Example.COM>").as_deref(),
            Some("example.com")
        );
        assert_eq!(domain_from_address("local@127.0.0.1"), None);
        assert_eq!(domain_from_address("not-an-address"), None);
    }

    #[test]
    fn chunked_icon_body_never_grows_past_its_limit() {
        let mut body = vec![1, 2, 3];
        assert!(append_response_chunk(&mut body, &[4, 5], 5));
        assert!(!append_response_chunk(&mut body, &[6], 5));
        assert_eq!(body, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn sender_icon_lookup_prefers_the_brand_domain() {
        assert_eq!(
            icon_domain_candidates("mail.instagram.com"),
            ["instagram.com", "mail.instagram.com"]
        );
        assert_eq!(
            icon_domain_candidates("advertise.support.facebook.com"),
            ["facebook.com", "advertise.support.facebook.com"]
        );
        assert_eq!(
            icon_domain_candidates("mailer.example.co.uk"),
            ["example.co.uk", "mailer.example.co.uk"]
        );
    }

    #[test]
    fn avatar_size_tracks_physical_pixels() {
        assert_eq!(physical_pixel_side(38.0, 1.0), 38);
        assert_eq!(physical_pixel_side(38.0, 1.5), 57);
        assert_eq!(physical_pixel_side(38.0, 2.0), 76);
    }

    #[test]
    fn brand_content_has_symmetric_pixel_margins() {
        assert_eq!(centered_brand_content_side(28), 20);
        assert_eq!(centered_brand_content_side(38), 28);
        assert_eq!(centered_brand_content_side(76), 54);
        for pixel_side in 1..=512 {
            let content_side = centered_brand_content_side(pixel_side);
            assert_eq!((pixel_side - content_side) % 2, 0);
        }
    }

    #[tokio::test]
    async fn brand_icon_variants_match_their_render_sizes() {
        let icons = decode_brand_icons(test_brand_png(), 28, 38).await.unwrap();
        assert_eq!((icons.small.width, icons.small.height), (28, 28));
        assert_eq!((icons.regular.width, icons.regular.height), (38, 38));
        assert_eq!(&icons.small.pixels[..4], &[255, 255, 255, 0]);
        assert_eq!(&icons.regular.pixels[..4], &[255, 255, 255, 0]);
        let small_center = ((14 * icons.small.width + 14) * 4) as usize;
        let regular_center = ((19 * icons.regular.width + 19) * 4) as usize;
        assert_eq!(icons.small.pixels[small_center + 3], 255);
        assert_eq!(icons.regular.pixels[regular_center + 3], 255);
        assert!(
            icons
                .small
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| (1..=254).contains(&pixel[3]))
        );
        assert!(
            icons
                .regular
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[0] == 255 && (1..=254).contains(&pixel[1]))
        );
    }

    #[tokio::test]
    async fn profile_avatar_variants_match_their_render_sizes() {
        let mut source = RgbaImage::from_pixel(128, 128, Rgba([40, 90, 180, 255]));
        source.put_pixel(0, 0, Rgba([10, 20, 30, 255]));
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(source)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();

        let images = decode_profile_avatars(bytes.into_inner(), 22, 38)
            .await
            .unwrap();
        assert_eq!((images.small.width, images.small.height), (22, 22));
        assert_eq!((images.regular.width, images.regular.height), (38, 38));
        assert_eq!(images.small.pixels[3], 0);
        assert_eq!(images.regular.pixels[3], 0);
        let small_center = ((11 * images.small.width + 11) * 4) as usize;
        let regular_center = ((19 * images.regular.width + 19) * 4) as usize;
        assert_eq!(images.small.pixels[small_center + 3], 255);
        assert_eq!(images.regular.pixels[regular_center + 3], 255);
        assert!(
            images
                .regular
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| (1..=254).contains(&pixel[3]))
        );
    }

    #[test]
    fn account_avatar_urls_are_provider_scoped() {
        assert!(
            trusted_profile_avatar_url("https://lh3.googleusercontent.com/a/profile").is_some()
        );
        assert!(trusted_profile_avatar_url("https://example.com/avatar.png").is_none());
        assert!(trusted_profile_avatar_url("http://lh3.googleusercontent.com/a/profile").is_none());
    }
}
