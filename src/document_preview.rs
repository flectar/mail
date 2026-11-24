//! Shared bounded file decoding used by Mail attachment dialogs and Files.
use std::sync::Arc;
// Cancelling spawn_blocking does not stop an active decoder. Keep admission
// inside the worker until decoding really ends, and never queue more images.
static IMAGE_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
static THUMBNAIL_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
type Result<T> = std::result::Result<T, String>;
pub(crate) enum Preview {
    Text(String),
    Image(Vec<u8>, u32, u32),
    Pdf(Arc<Vec<u8>>, crate::pdf_preview::Page),
}
/// Media types that say nothing about a file's content, so its name decides.
const GENERIC_TYPES: [&str; 4] = [
    "application/octet-stream",
    "binary/octet-stream",
    "application/x-download",
    "application/force-download",
];

/// Whether a file is declared as a PDF, by its media type or, when the type is
/// missing or generic, by a `.pdf` name. Content alone never counts, so a file
/// sent as an image or any other kind of file never reaches PDFium.
pub(crate) fn declared_pdf(media: &str, name: &str) -> bool {
    let media = media.split(';').next().unwrap_or("").trim();
    let is = |kind: &str| media.eq_ignore_ascii_case(kind);
    if is("application/pdf") || is("application/x-pdf") {
        return true;
    }
    (media.is_empty() || GENERIC_TYPES.into_iter().any(is))
        && name
            .as_bytes()
            .get(name.len().saturating_sub(4)..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(b".pdf"))
}

pub(crate) async fn preview(data: Vec<u8>, media: &str, name: &str) -> Result<Preview> {
    let normalized = media
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let media = normalized.as_str();
    if declared_pdf(media, name) {
        // PDFium only parses files that are declared as PDFs and start like one.
        if !data.starts_with(b"%PDF-") {
            return Err("This file does not look like a PDF. Download it to view locally.".into());
        }
        let data = Arc::new(data);
        return crate::pdf_preview::render(data.clone(), 0, 200)
            .await
            .map(|page| Preview::Pdf(data, page));
    }
    if media.starts_with("image/") && media != "image/svg+xml" {
        if data.len() > 16 * 1024 * 1024 {
            return Ok(Preview::Text(
                "This image is too large to preview. Download it to view locally.".into(),
            ));
        }
        return decode_image(move || {
            let mut reader = image::ImageReader::new(std::io::Cursor::new(data))
                .with_guessed_format()
                .map_err(|e| e.to_string())?;
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(8192);
            limits.max_image_height = Some(8192);
            limits.max_alloc = Some(64 * 1024 * 1024);
            reader.limits(limits);
            let image = reader
                .decode()
                .map_err(|_| {
                    "Could not decode this image safely. Download it to view locally.".to_string()
                })?
                .thumbnail(1800, 1400)
                .into_rgba8();
            let (w, h) = image.dimensions();
            Ok(Preview::Image(image.into_raw(), w, h))
        })
        .await;
    }
    if (media.starts_with("text/") || matches!(media, "application/json" | "application/xml"))
        && data.len() <= 1024 * 1024
    {
        return Ok(Preview::Text(String::from_utf8_lossy(&data).into_owned()));
    }
    Ok(Preview::Text("Preview is available for PDF, images and text files. Download this file to view it in a local application.".into()))
}

async fn decode_image(work: impl FnOnce() -> Result<Preview> + Send + 'static) -> Result<Preview> {
    let permit = IMAGE_SLOT
        .try_acquire()
        .map_err(|_| "Another image is still being decoded. Try again shortly.".to_string())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Decode a small attachment thumbnail on a separate, serialized lane so
/// background list decoration cannot make an explicitly requested preview
/// report that its decoder is busy.
pub(crate) async fn image_thumbnail(data: Vec<u8>) -> Result<(Vec<u8>, u32, u32)> {
    if data.len() > 4 * 1024 * 1024 {
        return Err("Image is too large for an attachment thumbnail".into());
    }
    let permit = THUMBNAIL_SLOT.acquire().await.map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut reader = image::ImageReader::new(std::io::Cursor::new(data))
            .with_guessed_format()
            .map_err(|e| e.to_string())?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let image = reader
            .decode()
            .map_err(|_| "Could not decode attachment thumbnail".to_string())?
            .thumbnail(96, 96)
            .into_rgba8();
        let (width, height) = image.dimensions();
        Ok((image.into_raw(), width, height))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    // Tests that decode images share IMAGE_SLOT, so they take turns.
    static IMAGE_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn only_files_declared_as_pdfs_count_as_pdfs() {
        for (media, name) in [
            ("application/pdf", "scan"),
            ("Application/PDF; name=\"scan.pdf\"", "scan.pdf"),
            ("application/x-pdf", "scan"),
            ("application/octet-stream", "Report.PDF"),
            ("binary/octet-stream", "report.pdf"),
            ("", "report.pdf"),
        ] {
            assert!(declared_pdf(media, name), "{media} {name}");
        }
        for (media, name) in [
            ("image/png", "photo.png"),
            ("image/png", "photo.pdf"),
            ("text/plain", "notes.pdf"),
            ("application/zip", "report.pdf"),
            ("application/octet-stream", "report.bin"),
            ("application/octet-stream", "pdf"),
            ("", "report.pdf.exe"),
            ("", ""),
        ] {
            assert!(!declared_pdf(media, name), "{media} {name}");
        }
    }

    #[test]
    fn pdf_content_in_a_file_declared_as_something_else_never_reaches_pdfium() {
        let _turn = IMAGE_TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let pdf = b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\n%%EOF\n".to_vec();
        let text = |result: Result<Preview>| match result {
            Ok(Preview::Text(text)) => text,
            _ => String::new(),
        };
        runtime.block_on(async {
            // An image decoder, not PDFium, rejects it.
            let image = preview(pdf.clone(), "image/png", "photo.png").await;
            assert_eq!(
                image.err().as_deref(),
                Some("Could not decode this image safely. Download it to view locally.")
            );
            // Text is shown as text.
            let shown = text(preview(pdf.clone(), "text/plain", "notes.txt").await);
            assert!(shown.starts_with("%PDF-1.7"));
            // Other types, and generic types without a PDF name, are not previewed.
            for (media, name) in [
                ("application/zip", "report.pdf"),
                ("application/octet-stream", "report.bin"),
                ("application/octet-stream", ""),
            ] {
                let shown = text(preview(pdf.clone(), media, name).await);
                assert!(
                    shown.starts_with("Preview is available for PDF"),
                    "{media} {name}"
                );
            }
            // A declared PDF must also start like one before PDFium sees it.
            let html = b"<html>not a PDF</html>".to_vec();
            let disguised = preview(html, "application/pdf", "report.pdf").await;
            assert_eq!(
                disguised.err().as_deref(),
                Some("This file does not look like a PDF. Download it to view locally.")
            );
        });
    }

    #[test]
    fn attachment_thumbnail_is_bounded_and_preserves_aspect_ratio() {
        let source = image::RgbaImage::from_pixel(200, 100, image::Rgba([24, 120, 176, 255]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (pixels, width, height) = runtime
            .block_on(image_thumbnail(encoded.into_inner()))
            .unwrap();
        assert_eq!((width, height), (96, 48));
        assert_eq!(pixels.len(), (width * height * 4) as usize);
    }

    #[test]
    fn cancelling_a_preview_does_not_admit_another_decoder_until_the_worker_exits() {
        let _turn = IMAGE_TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (started, wait_started) = tokio::sync::oneshot::channel();
            let (release, wait_release) = std::sync::mpsc::channel();
            let job = tokio::spawn(decode_image(move || {
                started.send(()).unwrap();
                wait_release
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                Ok(Preview::Text("finished".into()))
            }));
            wait_started.await.unwrap();
            job.abort();
            assert!(job.await.err().unwrap().is_cancelled());
            assert!(
                decode_image(|| panic!("second decoder started"))
                    .await
                    .is_err()
            );
            release.send(()).unwrap();
            let permit =
                tokio::time::timeout(std::time::Duration::from_secs(5), IMAGE_SLOT.acquire())
                    .await
                    .unwrap()
                    .unwrap();
            drop(permit);
            assert!(
                decode_image(|| Ok(Preview::Text("next".into())))
                    .await
                    .is_ok()
            );
        });
    }
}
