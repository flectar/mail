//! Pictures a signature links from the web, downloaded once when it's pasted or imported and kept
//! in it from then on (#549). Signature generators link their logos from their own servers, and a
//! signature here holds only pictures of its own, so they show at once wherever the message is
//! read, and nothing loads from the web when the signature is shown or sent.

use crate::compose_paste::{linked_pictures, with_picture_sources};
use std::{collections::HashMap, future::Future, io::Cursor};

/// The most pictures one signature downloads.
const MAX_PICTURES: usize = 16;
/// How many times wider than it shows a picture is kept, for sharp pictures on dense screens.
const SHARPNESS: u32 = 2;
/// The widest a picture is kept, in pixels, also when its HTML doesn't say how wide it shows.
const MAX_WIDTH: u32 = 1_200;

/// What downloading a signature's pictures came to.
#[derive(Debug)]
pub(crate) struct Embedded {
    /// The signature's HTML, with the pictures that were downloaded held in it.
    pub(crate) html: String,
    /// How many of the pictures it links couldn't be downloaded. It goes without them.
    pub(crate) missing: usize,
}

/// `html` with each picture it links from the web held in it, as `fetch` loads them, at most
/// twice as wide as it shows. Pictures `fetch` can't load, or that aren't PNG, JPEG, GIF or WebP
/// pictures, are counted as missing and keep their address, which cleaning the signature then
/// takes out.
pub(crate) async fn embed<F, Fut>(html: String, fetch: F) -> Embedded
where
    F: Fn(url::Url) -> Fut,
    Fut: Future<Output = Option<Vec<u8>>> + Send + 'static,
{
    let pictures = linked_pictures(&html);
    if pictures.is_empty() {
        return Embedded { html, missing: 0 };
    }
    let mut missing = pictures.len().saturating_sub(MAX_PICTURES);
    let mut downloads = tokio::task::JoinSet::new();
    for picture in pictures.into_iter().take(MAX_PICTURES) {
        let Ok(url) = url::Url::parse(&picture.url) else {
            missing += 1;
            continue;
        };
        let loaded = fetch(url);
        downloads.spawn(async move {
            let kept = loaded
                .await
                .and_then(|bytes| fitted_data_uri(&bytes, picture.shown_width));
            (picture.src, kept)
        });
    }
    let mut kept = HashMap::new();
    while let Some(done) = downloads.join_next().await {
        match done {
            Ok((src, Some(data_uri))) => {
                kept.insert(src, data_uri);
            }
            _ => missing += 1,
        }
    }
    let html = with_picture_sources(&html, |src| kept.get(src).cloned());
    Embedded { html, missing }
}

/// The picture `bytes` hold as a `data:` URI, no wider than `SHARPNESS` times `shown_width`, or
/// than `MAX_WIDTH` when that isn't known. A picture that's already narrow enough keeps its file
/// as it is; a wider one is scaled down and kept as a JPEG if it was one, else as a PNG. `None`
/// for anything but a PNG, JPEG, GIF or WebP picture.
fn fitted_data_uri(bytes: &[u8], shown_width: Option<u32>) -> Option<String> {
    use base64::Engine;
    use image::ImageFormat;
    let format = image::guess_format(bytes).ok()?;
    let mime_type = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Gif => "image/gif",
        ImageFormat::WebP => "image/webp",
        _ => return None,
    };
    let (width, height) = image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .ok()?;
    let max_width = shown_width
        .map_or(MAX_WIDTH, |shown| shown.saturating_mul(SHARPNESS))
        .clamp(1, MAX_WIDTH);
    let encode = |mime_type: &str, file: &[u8]| {
        let mut uri = format!("data:{mime_type};base64,");
        base64::engine::general_purpose::STANDARD.encode_string(file, &mut uri);
        uri
    };
    if width <= max_width {
        // Its file is checked to be a picture of its kind before it's kept as it is.
        image::load_from_memory_with_format(bytes, format).ok()?;
        return Some(encode(mime_type, bytes));
    }
    let picture = image::load_from_memory_with_format(bytes, format).ok()?;
    let fitted_height = (u64::from(height) * u64::from(max_width) / u64::from(width.max(1)))
        .clamp(1, u64::from(u32::MAX)) as u32;
    let fitted = picture.resize_exact(
        max_width,
        fitted_height,
        image::imageops::FilterType::Lanczos3,
    );
    let mut file = Vec::new();
    if format == ImageFormat::Jpeg {
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 90);
        fitted.into_rgb8().write_with_encoder(encoder).ok()?;
        Some(encode("image/jpeg", &file))
    } else {
        fitted
            .write_to(&mut Cursor::new(&mut file), ImageFormat::Png)
            .ok()?;
        Some(encode("image/png", &file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([26, 92, 196, 255]),
        ))
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
        bytes
    }

    fn picture_size(data_uri: &str) -> (u32, u32) {
        use base64::Engine;
        let (_, encoded) = data_uri.split_once(";base64,").unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        image::load_from_memory(&bytes)
            .unwrap()
            .into_rgba8()
            .dimensions()
    }

    /// The `src` of each picture in `html`, in order.
    fn sources(html: &str) -> Vec<String> {
        let mut found = Vec::new();
        with_picture_sources(html, |src| {
            found.push(src.to_owned());
            None
        });
        found
    }

    #[tokio::test]
    async fn linked_pictures_are_downloaded_into_the_signature() {
        let requested = Arc::new(Mutex::new(Vec::new()));
        let log = requested.clone();
        let fetch = move |url: url::Url| {
            log.lock().unwrap().push(url.to_string());
            let found = match url.path() {
                "/logo.png" => Some(png(600, 200)),
                "/small.png" => Some(png(40, 40)),
                "/page.html" => Some(b"<html></html>".to_vec()),
                _ => None,
            };
            async move { found }
        };
        let html = "<!--StartFragment--><table><tr><td><img src=\"https://cdn.example/logo.png\" \
                    width=\"150\" alt=\"Logo\"></td><td><img src=\"//cdn.example/small.png\">\
                    <img src=\"https://cdn.example/logo.png\" width=\"150\">\
                    <img src=\"https://cdn.example/page.html\"><img src=\"https://cdn.example/gone.png\">\
                    <img src=\"data:image/png;base64,iVBORw0KGgo=\"></td></tr></table><!--EndFragment-->";
        let embedded = embed(html.to_owned(), fetch).await;
        // The same picture once; a page and a missing picture can't be kept.
        let mut asked = requested.lock().unwrap().clone();
        asked.sort();
        assert_eq!(
            asked,
            [
                "https://cdn.example/gone.png",
                "https://cdn.example/logo.png",
                "https://cdn.example/page.html",
                "https://cdn.example/small.png",
            ]
        );
        assert_eq!(embedded.missing, 2);
        let sources = sources(&embedded.html);
        assert_eq!(sources.len(), 6, "{}", embedded.html);
        // Twice as wide as it shows, and a small one as it was.
        assert!(sources[0].starts_with("data:image/png;base64,"));
        assert_eq!(picture_size(&sources[0]), (300, 100));
        assert_eq!(picture_size(&sources[1]), (40, 40));
        assert_eq!(sources[2], sources[0]);
        assert_eq!(sources[3], "https://cdn.example/page.html");
        assert_eq!(sources[4], "https://cdn.example/gone.png");
        assert_eq!(sources[5], "data:image/png;base64,iVBORw0KGgo=");
        // What browsers mark as copied stays marked.
        assert!(embedded.html.starts_with("<!--StartFragment-->"));
        assert!(embedded.html.ends_with("<!--EndFragment-->"));
    }

    #[tokio::test]
    async fn html_without_linked_pictures_is_left_as_it_is() {
        let html = "<p>Pat <img src=\"data:image/png;base64,iVBORw0KGgo=\"></p>".to_owned();
        let embedded = embed(html.clone(), |_| async { panic!("nothing to download") }).await;
        assert_eq!((embedded.html, embedded.missing), (html, 0));
    }

    #[test]
    fn pictures_are_kept_no_wider_than_they_need() {
        // Without a width of its own, a picture is kept at most MAX_WIDTH wide.
        let wide = fitted_data_uri(&png(2_400, 600), None).unwrap();
        assert_eq!(picture_size(&wide), (MAX_WIDTH, 300));
        // A JPEG stays a JPEG.
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::new(800, 400))
            .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        let fitted = fitted_data_uri(&jpeg, Some(100)).unwrap();
        assert!(fitted.starts_with("data:image/jpeg;base64,"));
        assert_eq!(picture_size(&fitted), (200, 100));
        // Other files, and broken pictures, aren't kept.
        assert_eq!(fitted_data_uri(b"GIF89a broken", None), None);
        assert_eq!(fitted_data_uri(b"<svg></svg>", None), None);
    }
}
