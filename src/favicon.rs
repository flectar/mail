use flectar_mail_core::net::proxy::{Owner, SharedClient};
use image::{DynamicImage, ImageReader, Limits, Rgba, RgbaImage, imageops::FilterType};
use reqwest::{StatusCode, Url, header::CONTENT_TYPE};
use sha2::{Digest, Sha256};
use std::{
    io::Cursor,
    net::IpAddr,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use tokio::io::AsyncReadExt;

// Twenty serves a larger source than the largest normal avatar needs. The
// final raster is produced at the exact physical size Slint will paint (see
// `physical_pixel_side`), because Slint's software renderer otherwise uses
// nearest-neighbour sampling for the final scale operation.
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
    cache_dir: PathBuf,
}

/// Icons come through the app's proxy, on clients built again when the proxy
/// settings change.
static FAVICON_HTTP: SharedClient = SharedClient::new(|builder| {
    builder
        .user_agent("Flectar Mail native sender icon/0.1")
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .redirect(service_redirect_policy("twenty-icons.com"))
});

static AVATAR_HTTP: SharedClient = SharedClient::new(|builder| {
    builder
        .user_agent("Flectar Mail native account avatar/0.1")
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .redirect(service_redirect_policy("googleusercontent.com"))
});

impl FaviconLoader {
    pub fn new(cache_root: &Path) -> Result<Self, String> {
        let cache_dir = cache_root.join("sender-icons");
        std::fs::create_dir_all(&cache_dir)
            .map_err(|error| format!("could not create favicon cache: {error}"))?;

        Ok(Self { cache_dir })
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
    ) -> Result<Option<FaviconImages>, String> {
        let cache_path = self.cache_path(domain);
        if let Some(bytes) = read_fresh(&cache_path, CACHE_TTL, MAX_ICON_BYTES).await {
            match decode_brand_icons(bytes, small_pixel_side, regular_pixel_side).await {
                Ok(icons) => return Ok(Some(icons)),
                Err(_) => {
                    let _ = tokio::fs::remove_file(&cache_path).await;
                }
            }
        }

        let missing_path = self.missing_path(domain);
        if is_fresh(&missing_path, MISSING_TTL).await {
            return Ok(None);
        }

        let mut retryable_error = None;
        for candidate in icon_domain_candidates(domain) {
            let bytes = match self.fetch_icon_bytes(&candidate).await {
                Ok(Some(bytes)) => bytes,
                Ok(None) => continue,
                Err(error) => {
                    retryable_error.get_or_insert(error);
                    continue;
                }
            };
            let icons = match decode_brand_icons(bytes.clone(), small_pixel_side, regular_pixel_side)
                .await
            {
                Ok(icons) => icons,
                Err(error) => {
                    retryable_error.get_or_insert(error);
                    continue;
                }
            };

            // Cache the resolved brand asset under the original sender host.
            // Later rows therefore avoid both the exact-host and parent lookup.
            let _ = tokio::fs::write(&cache_path, bytes).await;
            let _ = tokio::fs::remove_file(&missing_path).await;
            return Ok(Some(icons));
        }

        if let Some(error) = retryable_error {
            return Err(error);
        }
        mark_missing(&missing_path).await;
        Ok(None)
    }

    async fn fetch_icon_bytes(&self, domain: &str) -> Result<Option<Vec<u8>>, String> {
        let url = format!("https://twenty-icons.com/{domain}/{SERVICE_ICON_SIZE}");
        let client = FAVICON_HTTP
            .get(Owner::App)
            .map_err(|error| format!("could not create favicon client: {error}"))?;
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        if !classify_icon_status(response.status())? {
            return Ok(None);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_ICON_BYTES)
        {
            return Err("sender icon response is too large".into());
        }
        if let Some(content_type) = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            && !content_type.to_ascii_lowercase().starts_with("image/")
        {
            return Err("sender icon response is not an image".into());
        }
        response_bytes_bounded(response, MAX_ICON_BYTES)
            .await
            .map(Some)
            .ok_or_else(|| "sender icon response could not be read within its size limit".into())
    }

    fn cache_path(&self, domain: &str) -> PathBuf {
        self.cache_dir
            .join(format!("{}.brand-v4.icon", cache_key(domain)))
    }

    fn missing_path(&self, domain: &str) -> PathBuf {
        self.cache_dir
            .join(format!("{}.brand-v5.missing", cache_key(domain)))
    }
}

fn classify_icon_status(status: StatusCode) -> Result<bool, String> {
    if status.is_success() {
        Ok(true)
    } else if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
        Ok(false)
    } else {
        Err(format!("sender icon service returned {status}"))
    }
}

#[derive(Clone)]
pub struct ProfileAvatarLoader {
    cache_dir: PathBuf,
}

impl ProfileAvatarLoader {
    pub fn new(cache_root: &Path) -> Result<Self, String> {
        let cache_dir = cache_root.join("account-avatars");
        std::fs::create_dir_all(&cache_dir)
            .map_err(|error| format!("could not create account-avatar cache: {error}"))?;
        Ok(Self { cache_dir })
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

        let response = AVATAR_HTTP
            .get(Owner::App)
            .ok()?
            .get(source_url)
            .send()
            .await
            .ok()?;
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
        let decoded = decode_bounded(bytes)?.into_rgba8();
        let layout = brand_layout(&decoded);
        let premultiplied = premultiply_alpha(decoded);
        Ok(FaviconImages {
            small: render_brand_icon(&premultiplied, &layout, small_pixel_side),
            regular: render_brand_icon(&premultiplied, &layout, regular_pixel_side),
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Where a brand icon sits inside the circular avatar and what fills the
/// circle around it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BrandLayout {
    backdrop: Rgba<u8>,
    /// Source point placed at the avatar centre.
    center: (f32, f32),
    /// Source length that spans the avatar diameter.
    extent: f32,
}

fn brand_layout(image: &RgbaImage) -> BrandLayout {
    let (width, height) = image.dimensions();
    let canvas_center = (width as f32 / 2.0, height as f32 / 2.0);
    let Some(bounds) = visible_bounds(image) else {
        return BrandLayout {
            backdrop: BRAND_PLATE,
            center: canvas_center,
            extent: width.max(height) as f32,
        };
    };
    let bounds_center = bounds.center();

    // Round logos already have the avatar's shape, so they fill it.
    if bounds.is_filled_disc(image) {
        return BrandLayout {
            backdrop: plate_for(image),
            center: bounds_center,
            extent: bounds.side(),
        };
    }

    // Icons whose outline is mostly opaque are tiles (squares, rounded
    // squares, discs with a background). They fill the circle instead of
    // showing as a smaller tile inside it.
    let edge = bounds.edge_pixels(image);
    let opaque_edge: Vec<Rgba<u8>> = edge
        .iter()
        .copied()
        .filter(|pixel| pixel[3] >= OPAQUE_ALPHA)
        .collect();
    if opaque_edge.len() * 2 >= edge.len() {
        if let Some(fill) = uniform_color(&opaque_edge) {
            let is_mark = |pixel: Rgba<u8>| {
                pixel[3] >= VISIBLE_ALPHA && color_distance(pixel, fill) > MARK_COLOR_DISTANCE
            };
            // A flat tile colour continues across the whole circle, so the
            // mark inside can be sized like any other mark without clipping.
            // A shaped tile whose mark runs off its edge (a letter cut by a
            // rounded square) keeps that cut at the circle edge instead of
            // floating inside it.
            let cut_mark =
                bounds.has_cut_corners(image) && bounds.ring_pixels(image).into_iter().any(is_mark);
            let extent = if cut_mark {
                bounds.width().min(bounds.height())
            } else {
                mark_radius(image, bounds_center, is_mark).map_or(bounds.side(), |radius| {
                    (2.0 * radius / BRAND_MARK_RADIUS).max(bounds.side() / 2.0)
                })
            };
            return BrandLayout {
                backdrop: fill,
                center: bounds_center,
                extent,
            };
        }
        // Gradients and pictures cover the circle edge to edge.
        return BrandLayout {
            backdrop: BRAND_PLATE,
            center: bounds_center,
            extent: bounds.width().min(bounds.height()),
        };
    }

    let backdrop = plate_for(image);
    // A mark with transparent surroundings sits on a plate. It keeps the
    // centre its designer gave it, and its visible pixels set its size, so a
    // favicon's own padding does not make one brand look smaller than another.
    let radius = mark_radius(image, canvas_center, |pixel| pixel[3] >= VISIBLE_ALPHA)
        .unwrap_or(width.max(height) as f32 / 2.0);
    BrandLayout {
        backdrop,
        center: canvas_center,
        extent: 2.0 * radius / BRAND_MARK_RADIUS,
    }
}

fn render_brand_icon(
    premultiplied: &RgbaImage,
    layout: &BrandLayout,
    pixel_side: u32,
) -> FaviconImage {
    let (width, height) = premultiplied.dimensions();
    let scale = pixel_side as f32 / layout.extent.max(1.0);
    let scaled_width = matching_parity_side(width as f32 * scale, pixel_side);
    let scaled_height = matching_parity_side(height as f32 * scale, pixel_side);
    let mut fitted = image::imageops::resize(
        premultiplied,
        scaled_width,
        scaled_height,
        FilterType::Lanczos3,
    );
    unpremultiply_alpha(&mut fitted);

    // The circle is opaque in every theme. Sender favicons are drawn for light
    // browser tabs, so a theme-coloured backdrop would hide dark marks such as
    // GitHub's in dark mode. Tiles carry their own colour to the circle edge.
    // Bake the circular clip into the raster as well: the software renderer
    // does not reliably clip an Image to its parent's rounded corners.
    let mut canvas = RgbaImage::from_pixel(pixel_side, pixel_side, layout.backdrop);
    let scale_x = scaled_width as f32 / width as f32;
    let scale_y = scaled_height as f32 / height as f32;
    let x = (pixel_side as f32 / 2.0 - layout.center.0 * scale_x).round() as i64;
    let y = (pixel_side as f32 / 2.0 - layout.center.1 * scale_y).round() as i64;
    image::imageops::overlay(&mut canvas, &fitted, x, y);
    apply_circular_alpha_mask(&mut canvas);

    FaviconImage {
        width: pixel_side,
        height: pixel_side,
        pixels: canvas.into_raw().into(),
    }
}

/// Round a scaled length to the nearest size with the avatar's parity.
///
/// The software renderer needs an exact-size raster, so the composition
/// happens in physical pixels. An odd mark inside an even avatar (or the
/// reverse) has a half-pixel centre. Matching the parity keeps centred
/// brands geometrically centred without another resampling pass.
fn matching_parity_side(length: f32, pixel_side: u32) -> u32 {
    let length = length.clamp(1.0, (pixel_side * MAX_BRAND_UPSCALE) as f32);
    let rounded = length.round() as u32;
    if rounded.abs_diff(pixel_side).is_multiple_of(2) {
        return rounded.max(1);
    }
    if length >= rounded as f32 || rounded == 1 {
        rounded + 1
    } else {
        rounded - 1
    }
}

/// The light plate behind marks with transparent surroundings. Favicons are
/// authored for light browser tabs, which is why mail clients, launchers and
/// browser start pages keep a light surface behind them in dark themes too.
const BRAND_PLATE: Rgba<u8> = Rgba([255, 255, 255, 255]);
/// The plate for marks drawn almost entirely in white, which would vanish on
/// the light plate.
const BRAND_DARK_PLATE: Rgba<u8> = Rgba([32, 33, 36, 255]);
/// Radius of the furthest visible mark pixel, as a share of the avatar radius.
const BRAND_MARK_RADIUS: f32 = 0.8;
/// Upper bound for the scaled source side, in avatar diameters.
const MAX_BRAND_UPSCALE: u32 = 4;
const VISIBLE_ALPHA: u8 = 128;
const OPAQUE_ALPHA: u8 = 250;
/// Largest per-channel difference still treated as the tile colour.
const TILE_COLOR_TOLERANCE: u8 = 16;
/// Smallest per-channel difference that marks a pixel as part of the mark.
const MARK_COLOR_DISTANCE: u8 = 48;

#[derive(Clone, Copy, Debug)]
struct PixelBounds {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

impl PixelBounds {
    fn width(self) -> f32 {
        (self.right - self.left + 1) as f32
    }

    fn height(self) -> f32 {
        (self.bottom - self.top + 1) as f32
    }

    fn side(self) -> f32 {
        self.width().max(self.height())
    }

    fn center(self) -> (f32, f32) {
        (
            (self.left + self.right + 1) as f32 / 2.0,
            (self.top + self.bottom + 1) as f32 / 2.0,
        )
    }

    /// Whether at least two corners are transparent, as on a rounded square.
    fn has_cut_corners(self, image: &RgbaImage) -> bool {
        [
            (self.left, self.top),
            (self.right, self.top),
            (self.left, self.bottom),
            (self.right, self.bottom),
        ]
        .into_iter()
        .filter(|&(x, y)| image.get_pixel(x, y)[3] < VISIBLE_ALPHA)
        .count()
            >= 2
    }

    fn ring_pixels(self, image: &RgbaImage) -> Vec<Rgba<u8>> {
        let mut pixels = Vec::new();
        for x in self.left..=self.right {
            pixels.push(*image.get_pixel(x, self.top));
            pixels.push(*image.get_pixel(x, self.bottom));
        }
        for y in self.top..=self.bottom {
            pixels.push(*image.get_pixel(self.left, y));
            pixels.push(*image.get_pixel(self.right, y));
        }
        pixels
    }

    /// Pixels along the middle half of each side. Rounded corners stay out
    /// of the sample, so a rounded-square tile reads as opaque while a disc,
    /// which only touches the middle of each side, does not.
    fn edge_pixels(self, image: &RgbaImage) -> Vec<Rgba<u8>> {
        let middle = |start: u32, end: u32| {
            let length = end - start + 1;
            let skip = length / 4;
            start + skip..=end - skip
        };
        let mut pixels = Vec::new();
        for x in middle(self.left, self.right) {
            pixels.push(*image.get_pixel(x, self.top));
            pixels.push(*image.get_pixel(x, self.bottom));
        }
        for y in middle(self.top, self.bottom) {
            pixels.push(*image.get_pixel(self.left, y));
            pixels.push(*image.get_pixel(self.right, y));
        }
        pixels
    }

    /// Whether the visible pixels form a solid disc that fills these bounds,
    /// the way round logos and pre-cropped avatars do.
    fn is_filled_disc(self, image: &RgbaImage) -> bool {
        let (width, height) = (self.width(), self.height());
        if (width - height).abs() > width.max(height) * 0.04 || width < 8.0 {
            return false;
        }
        let (center_x, center_y) = self.center();
        let radius = (width + height) / 4.0;
        let mut mismatched = 0_u32;
        let mut total = 0_u32;
        for y in self.top..=self.bottom {
            for x in self.left..=self.right {
                let dx = x as f32 + 0.5 - center_x;
                let dy = y as f32 + 0.5 - center_y;
                let distance = (dx * dx + dy * dy).sqrt();
                // Skip the one-pixel antialiased rim.
                if (distance - radius).abs() < 1.0 {
                    continue;
                }
                total += 1;
                let inside = distance < radius;
                let visible = image.get_pixel(x, y)[3] >= VISIBLE_ALPHA;
                if inside != visible {
                    mismatched += 1;
                }
            }
        }
        total > 0 && mismatched * 100 <= total * 2
    }
}

fn visible_bounds(image: &RgbaImage) -> Option<PixelBounds> {
    let mut bounds: Option<PixelBounds> = None;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] < VISIBLE_ALPHA {
            continue;
        }
        let current = bounds.get_or_insert(PixelBounds {
            left: x,
            top: y,
            right: x,
            bottom: y,
        });
        current.left = current.left.min(x);
        current.top = current.top.min(y);
        current.right = current.right.max(x);
        current.bottom = current.bottom.max(y);
    }
    bounds
}

/// The tile colour when most opaque edge pixels share one colour. Marks that
/// run off the tile edge (wordmarks cut by the square) still leave most of
/// the edge in the tile colour.
fn uniform_color(pixels: &[Rgba<u8>]) -> Option<Rgba<u8>> {
    if pixels.is_empty() {
        return None;
    }
    let median = |channel: usize| {
        let mut values: Vec<u8> = pixels.iter().map(|pixel| pixel[channel]).collect();
        values.sort_unstable();
        values[values.len() / 2]
    };
    let color = Rgba([median(0), median(1), median(2), 255]);
    let matching = pixels
        .iter()
        .filter(|pixel| color_distance(**pixel, color) <= TILE_COLOR_TOLERANCE)
        .count();
    (matching * 4 >= pixels.len() * 3).then_some(color)
}

fn color_distance(a: Rgba<u8>, b: Rgba<u8>) -> u8 {
    (0..3)
        .map(|channel| a[channel].abs_diff(b[channel]))
        .max()
        .unwrap_or(0)
}

/// Distance from `center` to the outer corner of the furthest matching pixel.
fn mark_radius(
    image: &RgbaImage,
    center: (f32, f32),
    is_mark: impl Fn(Rgba<u8>) -> bool,
) -> Option<f32> {
    let mut radius: Option<f32> = None;
    for (x, y, pixel) in image.enumerate_pixels() {
        if !is_mark(*pixel) {
            continue;
        }
        let dx = (x as f32 + 0.5 - center.0).abs() + 0.5;
        let dy = (y as f32 + 0.5 - center.1).abs() + 0.5;
        let distance = (dx * dx + dy * dy).sqrt();
        radius = Some(radius.map_or(distance, |radius: f32| radius.max(distance)));
    }
    radius
}

/// The light plate, unless the mark is drawn almost entirely in white.
fn plate_for(image: &RgbaImage) -> Rgba<u8> {
    let mut ink = 0.0_f32;
    let mut light_ink = 0.0_f32;
    for pixel in image.pixels() {
        let alpha = f32::from(pixel[3]) / 255.0;
        ink += alpha;
        if contrast_ratio(relative_luminance(*pixel), 1.0) < LIGHT_INK_CONTRAST {
            light_ink += alpha;
        }
    }
    if ink > 0.0 && light_ink >= ink * 0.9 {
        BRAND_DARK_PLATE
    } else {
        BRAND_PLATE
    }
}

/// Marks below this WCAG contrast ratio against white count as light ink.
const LIGHT_INK_CONTRAST: f32 = 1.5;

fn relative_luminance(pixel: Rgba<u8>) -> f32 {
    let linear = |channel: u8| {
        let value = f32::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(pixel[0]) + 0.7152 * linear(pixel[1]) + 0.0722 * linear(pixel[2])
}

fn contrast_ratio(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

pub(crate) async fn decode_profile_avatars(
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

/// Convert straight alpha to premultiplied alpha so that resampling does not
/// blend the (often black) colour of fully transparent pixels into the edges.
fn premultiply_alpha(mut image: RgbaImage) -> RgbaImage {
    for pixel in image.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
    image
}

/// Inverse of [`premultiply_alpha`], applied after resizing. Lanczos ringing
/// can push a premultiplied channel above its alpha, so the result is clamped.
fn unpremultiply_alpha(image: &mut RgbaImage) {
    for pixel in image.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            pixel.0 = [0, 0, 0, 0];
            continue;
        }
        for channel in &mut pixel.0[..3] {
            *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
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

    #[test]
    fn image_loaders_use_the_platform_cache_directory() {
        let root = tempfile::tempdir().unwrap();
        let cache_root = root.path().join("app-private-cache");
        let sender_icons = FaviconLoader::new(&cache_root).unwrap();
        let account_avatars = ProfileAvatarLoader::new(&cache_root).unwrap();

        assert_eq!(sender_icons.cache_dir, cache_root.join("sender-icons"));
        assert_eq!(account_avatars.cache_dir, cache_root.join("account-avatars"));
        assert!(sender_icons.cache_dir.is_dir());
        assert!(account_avatars.cache_dir.is_dir());
    }

    #[test]
    fn only_confirmed_absence_is_negative_cached() {
        assert!(classify_icon_status(StatusCode::OK).unwrap());
        assert!(!classify_icon_status(StatusCode::NOT_FOUND).unwrap());
        assert!(!classify_icon_status(StatusCode::GONE).unwrap());
        assert!(classify_icon_status(StatusCode::TOO_MANY_REQUESTS).is_err());
        assert!(classify_icon_status(StatusCode::SERVICE_UNAVAILABLE).is_err());
    }

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
    fn scaled_brand_sides_keep_symmetric_pixel_margins() {
        assert_eq!(matching_parity_side(20.4, 28), 20);
        assert_eq!(matching_parity_side(27.6, 38), 28);
        assert_eq!(matching_parity_side(27.4, 37), 27);
        for pixel_side in 1..=512 {
            for length in [0.2, 1.0, 13.5, 27.3, 64.9, 900.0, 5000.0] {
                let side = matching_parity_side(length, pixel_side);
                assert!(side >= 1);
                assert_eq!(side.abs_diff(pixel_side) % 2, 0, "{length} in {pixel_side}");
            }
        }
    }

    fn png_bytes(image: RgbaImage) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn pixel_at(icon: &FaviconImage, x: u32, y: u32) -> [u8; 4] {
        let index = ((y * icon.width + x) * 4) as usize;
        icon.pixels[index..index + 4].try_into().unwrap()
    }

    /// A glyph that is neither a disc nor a tile: a ring with a bar through it.
    fn draw_glyph(image: &mut RgbaImage, center: i32, radius: i32, color: Rgba<u8>) {
        for y in 0..image.height() as i32 {
            for x in 0..image.width() as i32 {
                let distance = (x - center).pow(2) + (y - center).pow(2);
                let ring = distance <= radius.pow(2) && distance >= (radius * 2 / 3).pow(2);
                let bar = (y - center).abs() <= radius / 6 && (x - center).abs() <= radius;
                if ring || bar {
                    image.put_pixel(x as u32, y as u32, color);
                }
            }
        }
    }

    #[tokio::test]
    async fn brand_icon_variants_match_their_render_sizes() {
        let icons = decode_brand_icons(test_brand_png(), 28, 38).await.unwrap();
        assert_eq!((icons.small.width, icons.small.height), (28, 28));
        assert_eq!((icons.regular.width, icons.regular.height), (38, 38));
        for icon in [&icons.small, &icons.regular] {
            assert_eq!(pixel_at(icon, 0, 0)[3], 0);
            let center = pixel_at(icon, icon.width / 2, icon.height / 2);
            assert_eq!(center, [255, 0, 0, 255]);
            assert!(
                icon.pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| (1..=254).contains(&pixel[3]))
            );
            // Resizing on premultiplied pixels leaves no dark fringe.
            assert!(
                icon.pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel[3] == 0 || pixel[0] >= 250)
            );
        }
    }

    #[test]
    fn round_logos_fill_the_avatar() {
        let image = image::load_from_memory(&test_brand_png())
            .unwrap()
            .into_rgba8();
        let layout = brand_layout(&image);
        assert_eq!(layout.center, (64.5, 64.5));
        assert_eq!(layout.extent, 101.0);
    }

    #[tokio::test]
    async fn dark_marks_stay_on_a_light_plate() {
        let mut image = RgbaImage::from_pixel(128, 128, Rgba([0, 0, 0, 0]));
        draw_glyph(&mut image, 64, 30, Rgba([24, 23, 23, 255]));
        let layout = brand_layout(&image);
        assert_eq!(layout.backdrop, BRAND_PLATE);
        assert_eq!(layout.center, (64.0, 64.0));

        let icons = decode_brand_icons(png_bytes(image), 28, 38).await.unwrap();
        let icon = &icons.regular;
        // The plate is opaque inside the circle, between the mark and the rim.
        assert_eq!(pixel_at(icon, 19, 3), [255, 255, 255, 255]);
        assert!(pixel_at(icon, 19, 19)[0] < 40);
        // The visible mark reaches the same share of every avatar radius,
        // whatever padding the favicon itself carries.
        let reach = |icon: &FaviconImage| {
            (0..icon.width)
                .filter(|&x| pixel_at(icon, x, icon.height / 2)[0] < 128)
                .map(|x| (x as f32 + 0.5 - icon.width as f32 / 2.0).abs() + 0.5)
                .fold(0.0, f32::max)
                / (icon.width as f32 / 2.0)
        };
        assert!((reach(&icons.small) - BRAND_MARK_RADIUS).abs() <= 0.08);
        assert!((reach(&icons.regular) - BRAND_MARK_RADIUS).abs() <= 0.06);
    }

    #[test]
    fn white_marks_get_a_dark_plate() {
        let mut image = RgbaImage::from_pixel(64, 64, Rgba([0, 0, 0, 0]));
        draw_glyph(&mut image, 32, 20, Rgba([250, 250, 250, 255]));
        assert_eq!(brand_layout(&image).backdrop, BRAND_DARK_PLATE);
    }

    #[test]
    fn flat_tiles_extend_their_colour_around_the_mark() {
        let tile = Rgba([0, 0, 0, 255]);
        let mut image = RgbaImage::from_pixel(128, 128, tile);
        draw_glyph(&mut image, 64, 20, Rgba([255, 255, 255, 255]));
        let layout = brand_layout(&image);
        assert_eq!(layout.backdrop, tile);
        assert_eq!(layout.center, (64.0, 64.0));
        // The mark grows to the shared size; the square tile edge falls
        // outside the circle, where the tile colour continues.
        assert!(layout.extent < 128.0);
    }

    #[test]
    fn marks_cut_by_a_rounded_tile_keep_the_cut_at_the_rim() {
        let tile = Rgba([8, 102, 255, 255]);
        let mut image = RgbaImage::from_pixel(128, 128, tile);
        for y in 0..128_i32 {
            for x in 0..128_i32 {
                let corner_x = x.clamp(20, 107);
                let corner_y = y.clamp(20, 107);
                if (x - corner_x).pow(2) + (y - corner_y).pow(2) > 20_i32.pow(2) {
                    image.put_pixel(x as u32, y as u32, Rgba([0, 0, 0, 0]));
                }
                // A white stem that runs off the bottom edge.
                if (56..72).contains(&x) && y >= 70 {
                    image.put_pixel(x as u32, y as u32, Rgba([255, 255, 255, 255]));
                }
            }
        }
        let layout = brand_layout(&image);
        assert_eq!(layout.backdrop, tile);
        assert_eq!(layout.extent, 128.0);
    }

    #[test]
    fn pictures_and_gradients_cover_the_avatar() {
        let image = RgbaImage::from_fn(96, 96, |x, y| {
            Rgba([(x * 2) as u8, (y * 2) as u8, 160, 255])
        });
        let layout = brand_layout(&image);
        assert_eq!(layout.backdrop, BRAND_PLATE);
        assert_eq!(layout.extent, 96.0);
    }

    #[test]
    fn empty_icons_fall_back_to_the_plain_plate() {
        let image = RgbaImage::from_pixel(32, 32, Rgba([0, 0, 0, 0]));
        let layout = brand_layout(&image);
        assert_eq!(layout.backdrop, BRAND_PLATE);
        assert_eq!(layout.extent, 32.0);
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
