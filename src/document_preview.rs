//! Shared bounded file decoding used by Mail attachment dialogs and Files.
use std::sync::Arc;
// Cancelling spawn_blocking does not stop an active decoder. Keep admission
// inside the worker until decoding really ends, and never queue more images.
static IMAGE_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
type Result<T> = std::result::Result<T, String>;
pub(crate) enum Preview {
    Text(String),
    Image(Vec<u8>, u32, u32),
    Pdf(Arc<Vec<u8>>, crate::pdf_preview::Page),
}
pub(crate) async fn preview(data: Vec<u8>, media: &str) -> Result<Preview> {
    let normalized = media
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let media = normalized.as_str();
    if media
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("application/pdf")
        || data.starts_with(b"%PDF-")
    {
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
    let permit = IMAGE_SLOT.try_acquire().map_err(|_| {
        "Another image is still being decoded. Try again shortly.".to_string()
    })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    }).await.map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelling_a_preview_does_not_admit_another_decoder_until_the_worker_exits() {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        runtime.block_on(async {
            let (started, wait_started) = tokio::sync::oneshot::channel();
            let (release, wait_release) = std::sync::mpsc::channel();
            let job = tokio::spawn(decode_image(move || {
                started.send(()).unwrap();
                wait_release.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
                Ok(Preview::Text("finished".into()))
            }));
            wait_started.await.unwrap();
            job.abort();
            assert!(job.await.err().unwrap().is_cancelled());
            assert!(decode_image(|| panic!("second decoder started")).await.is_err());
            release.send(()).unwrap();
            let permit = tokio::time::timeout(std::time::Duration::from_secs(5), IMAGE_SLOT.acquire()).await.unwrap().unwrap();
            drop(permit);
            assert!(decode_image(|| Ok(Preview::Text("next".into()))).await.is_ok());
        });
    }
}
