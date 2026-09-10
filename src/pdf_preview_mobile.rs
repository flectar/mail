//! Mobile cannot spawn the desktop helper. Admit one job at a time, without a
//! queue, and keep its permit until native code actually exits (not just until
//! its caller times out). Cancellation is cooperative: PDFium's document/page
//! parsing and individual render operations are not forcibly interruptible.
//! The engine stays loaded after first use; documents, bitmaps and input bytes
//! are dropped after each job. No form environment, JavaScript or XFA is started.
use super::{MAX_DOCUMENT_BYTES, Page, dimensions};
use pdfium_render::prelude::*;
use std::{
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const MOBILE_EDGE: u32 = 2048;
const DEADLINE: Duration = Duration::from_secs(15);
static EPOCH: AtomicU64 = AtomicU64::new(0);
static SLOT: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
// Bindings own the dynamic library. They are initialized once, exclusively by
// the admitted worker, and remain alive for the process lifetime. We use the
// public raw bindings because the high-level bitmap API has no pause callback.
static ENGINE: OnceLock<Result<Box<dyn PdfiumLibraryBindings>, String>> = OnceLock::new();
#[cfg(target_os = "android")]
static ANDROID_LIBRARY: OnceLock<PathBuf> = OnceLock::new();

/// Supply ApplicationInfo.nativeLibraryDir, never a document-controlled path.
/// The APK uses extracted native libraries so this is a private installed file.
#[cfg(target_os = "android")]
pub fn configure_android(directory: PathBuf) -> Result<(), String> {
    if !directory.is_absolute() {
        return Err("Android did not provide an absolute native library directory.".into());
    }
    let path = directory.join("libpdfium.so");
    if let Some(existing) = ANDROID_LIBRARY.get() {
        return if existing == &path {
            Ok(())
        } else {
            Err("PDF library directory changed.".into())
        };
    }
    ANDROID_LIBRARY
        .set(path)
        .map_err(|_| "PDF library already configured.".into())
}

/// Called on backgrounding and memory pressure, as well as by UI cancellation.
/// Does not kill a thread or pretend that a native parser has already stopped.
pub fn cancel_active() {
    EPOCH.fetch_add(1, Ordering::AcqRel);
}

fn library_path() -> Result<PathBuf, String> {
    #[cfg(target_os = "android")]
    return ANDROID_LIBRARY
        .get()
        .cloned()
        .ok_or_else(|| "PDF preview is not configured.".into());
    #[cfg(target_os = "ios")]
    return std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .map(|p| p.join("Frameworks/PDFium.framework/PDFium"))
        .ok_or_else(|| "Missing application bundle.".into());
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    std::env::var_os("FLECTAR_TEST_PDFIUM")
        .map(PathBuf::from)
        .ok_or_else(|| "Set FLECTAR_TEST_PDFIUM for the native integration test.".into())
}

struct Cancellation {
    cancelled: std::sync::atomic::AtomicBool,
    epoch: u64,
    deadline: Instant,
}
impl Cancellation {
    fn new() -> Self {
        Self {
            cancelled: false.into(),
            epoch: EPOCH.load(Ordering::Acquire),
            deadline: Instant::now() + DEADLINE,
        }
    }
    fn stopped(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
            || self.epoch != EPOCH.load(Ordering::Acquire)
            || Instant::now() >= self.deadline
    }
    fn check(&self) -> Result<(), String> {
        if self.stopped() {
            Err("PDF preview was cancelled or took too long. Download it to view locally.".into())
        } else {
            Ok(())
        }
    }
}
struct CancelOnDrop(Arc<Cancellation>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancelled.store(true, Ordering::Release);
    }
}

pub(crate) async fn render(data: Arc<Vec<u8>>, index: u32, zoom: u32) -> Result<Page, String> {
    if data.is_empty() || data.len() > MAX_DOCUMENT_BYTES || !(50..=200).contains(&zoom) {
        return Err("This PDF exceeds the preview limit. Download it to view locally.".into());
    }
    run_job(move |cancellation| render_page(&data, index, zoom, cancellation)).await
}

async fn run_job(
    work: impl FnOnce(&Cancellation) -> Result<Page, String> + Send + 'static,
) -> Result<Page, String> {
    let permit = SLOT
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            "The previous PDF preview is still stopping. Try again shortly.".to_string()
        })?;
    let cancellation = Arc::new(Cancellation::new());
    let guard = CancelOnDrop(cancellation.clone());
    let (sender, receiver) = tokio::sync::oneshot::channel();
    // A dedicated, low-priority thread avoids blocking either Slint or Tokio's
    // pool. There is never a second parser or a backlog of retained documents.
    std::thread::Builder::new()
        .name("pdf-preview".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            #[cfg(target_os = "android")]
            unsafe {
                libc::nice(10);
            }
            #[cfg(target_os = "ios")]
            unsafe {
                libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
            }
            let result = work(&cancellation);
            drop(permit);
            let _ = sender.send(result);
        })
        .map_err(|e| format!("Could not start PDF preview: {e}"))?;
    let result = tokio::time::timeout(DEADLINE, receiver)
        .await
        .map_err(|_| {
            "This PDF page took too long to render. Download it to view locally.".to_string()
        })?
        .map_err(|_| "The PDF renderer could not finish this page.".to_string())?;
    drop(guard);
    result
}

fn page_dimensions(width: f32, height: f32, zoom: u32) -> Result<(u32, u32), String> {
    if !width.is_finite()
        || !height.is_finite()
        || width <= 0.0
        || height <= 0.0
        || !(50..=200).contains(&zoom)
    {
        return Err("Invalid PDF page dimensions.".into());
    }
    let width = f64::from(width);
    let height = f64::from(height);
    let scale = (f64::from(1024 * zoom / 100) / width)
        .min(f64::from(MOBILE_EDGE) / height)
        .min(f64::from(MOBILE_EDGE) / width);
    let w = (width * scale).round().clamp(1.0, f64::from(MOBILE_EDGE)) as u32;
    let h = (height * scale).round().clamp(1.0, f64::from(MOBILE_EDGE)) as u32;
    Ok((w, h))
}

// The callback borrows Cancellation for the synchronous Start/Continue call.
// It never calls PDFium again (the bindings hold a mutex), allocates, or panics.
unsafe extern "C" fn pause_now(pause: *mut IFSDK_PAUSE) -> FPDF_BOOL {
    unsafe { i32::from((&*((*pause).user.cast::<Cancellation>())).stopped()) }
}

struct Document<'a>(FPDF_DOCUMENT, &'a dyn PdfiumLibraryBindings);
impl Drop for Document<'_> {
    fn drop(&mut self) {
        unsafe {
            self.1.FPDF_CloseDocument(self.0);
        }
    }
}
struct NativePage<'a>(FPDF_PAGE, &'a dyn PdfiumLibraryBindings);
impl Drop for NativePage<'_> {
    fn drop(&mut self) {
        unsafe {
            self.1.FPDF_ClosePage(self.0);
        }
    }
}
struct Bitmap<'a>(FPDF_BITMAP, &'a dyn PdfiumLibraryBindings);
impl Drop for Bitmap<'_> {
    fn drop(&mut self) {
        unsafe {
            self.1.FPDFBitmap_Destroy(self.0);
        }
    }
}
struct Progress<'a>(FPDF_PAGE, &'a dyn PdfiumLibraryBindings);
impl Drop for Progress<'_> {
    fn drop(&mut self) {
        unsafe {
            self.1.FPDF_RenderPage_Close(self.0);
        }
    }
}

fn render_page(
    data: &[u8],
    index: u32,
    zoom: u32,
    cancellation: &Cancellation,
) -> Result<Page, String> {
    cancellation.check()?;
    let bindings = ENGINE
        .get_or_init(|| {
            let bindings = Pdfium::bind_to_library(library_path()?).map_err(|_| {
                "The bundled PDF preview component could not be loaded.".to_string()
            })?;
            // SAFETY: only this OnceLock initializer initializes this engine. The
            // owning bindings and loaded library outlive every native handle.
            unsafe {
                bindings.FPDF_InitLibrary();
            }
            Ok(bindings)
        })
        .as_ref()
        .map_err(Clone::clone)?
        .as_ref();
    cancellation.check()?;
    // SAFETY: all handles are checked before use, exclusively accessed by the
    // single admitted worker, and dropped in reverse dependency order. Input
    // bytes and caller-owned bitmap storage outlive their native handles.
    unsafe {
        let raw = bindings.FPDF_LoadMemDocument64(data, None);
        if raw.is_null() {
            return Err("Unable to open this PDF. It may be damaged or password protected.".into());
        }
        let document = Document(raw, bindings);
        cancellation.check()?;
        let count = bindings.FPDF_GetPageCount(document.0);
        if count <= 0 || index >= count as u32 {
            return Err("PDF page out of range.".into());
        }
        let raw = bindings.FPDF_LoadPage(document.0, index as i32);
        if raw.is_null() {
            return Err("Unable to read this PDF page.".into());
        }
        let page = NativePage(raw, bindings);
        cancellation.check()?;
        let (width, height) = page_dimensions(
            bindings.FPDF_GetPageWidthF(page.0),
            bindings.FPDF_GetPageHeightF(page.0),
            zoom,
        )?;
        let mut pixels = vec![255u8; dimensions(width, height)?];
        // FPDFBitmap_BGRA = 4; stride is explicit and storage is at most 16 MiB.
        let raw = bindings.FPDFBitmap_CreateEx(
            width as i32,
            height as i32,
            4,
            pixels.as_mut_ptr().cast(),
            (width * 4) as i32,
        );
        if raw.is_null() {
            return Err("Unable to allocate a PDF preview bitmap.".into());
        }
        let bitmap = Bitmap(raw, bindings);
        let mut pause = IFSDK_PAUSE {
            version: 1,
            NeedToPauseNow: Some(pause_now),
            user: std::ptr::from_ref(cancellation).cast_mut().cast(),
        };
        // FPDF_ANNOT | FPDF_RENDER_LIMITEDIMAGECACHE; no form fill environment
        // or document actions. Close progressive state even on cancellation.
        let progress = Progress(page.0, bindings);
        let mut status = bindings.FPDF_RenderPageBitmap_Start(
            bitmap.0,
            page.0,
            0,
            0,
            width as i32,
            height as i32,
            0,
            1 | 0x200,
            &mut pause,
        );
        while status == 1 {
            // FPDF_RENDER_TOBECONTINUED
            cancellation.check()?;
            status = bindings.FPDF_RenderPage_Continue(page.0, &mut pause);
        }
        cancellation.check()?;
        if status != 2 {
            return Err("Unable to finish rendering this PDF page.".into());
        }
        // Native bitmap no longer borrows pixels before returning their owner.
        drop(progress);
        drop(bitmap);
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        Ok(Page {
            pixels,
            width,
            height,
            count: count as u32,
            index,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static TEST_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[test]
    fn mobile_pixels_are_bounded_for_extreme_aspect_ratios() {
        for (w, h) in [
            (612.0, 792.0),
            (f32::MAX, 1.0),
            (1.0, f32::MAX),
            (f32::MIN_POSITIVE, 1.0),
        ] {
            for zoom in [50, 100, 150, 200] {
                let (w, h) = page_dimensions(w, h, zoom).unwrap();
                assert!(w > 0 && h > 0 && w <= MOBILE_EDGE && h <= MOBILE_EDGE);
                assert!(dimensions(w, h).unwrap() <= 16 * 1024 * 1024);
            }
        }
        for invalid in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(page_dimensions(invalid, 1.0, 100).is_err());
        }
    }
    #[test]
    fn cancellation_and_background_epoch_stop_native_callback() {
        let _test = TEST_GATE.lock().unwrap();
        let cancellation = Arc::new(Cancellation::new());
        assert!(!cancellation.stopped());
        drop(CancelOnDrop(cancellation.clone()));
        assert!(cancellation.stopped());
        let next = Cancellation::new();
        cancel_active();
        assert!(next.stopped());
    }
    #[test]
    fn cancelled_caller_cannot_start_a_second_native_worker() {
        let _test = TEST_GATE.lock().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (started, ready) = tokio::sync::oneshot::channel();
            let (release, blocked) = std::sync::mpsc::channel();
            let (finished, stopped) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(run_job(move |cancellation| {
                started.send(()).unwrap();
                // Model a non-interruptible native parse without hanging a test.
                blocked.recv_timeout(Duration::from_secs(5)).unwrap();
                finished.send(cancellation.stopped()).unwrap();
                Err("native parser finished".into())
            }));
            ready.await.unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            let refused = run_job(|_| panic!("a second worker must not be started")).await;
            assert!(refused.unwrap_err().contains("still stopping"));
            release.send(()).unwrap();
            assert!(stopped.await.unwrap());
            tokio::time::timeout(Duration::from_secs(2), async {
                while SLOT.get().unwrap().available_permits() == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                run_job(|_| Err("recovered".into())).await.unwrap_err(),
                "recovered"
            );
        });
    }
    #[test]
    #[ignore = "requires FLECTAR_TEST_PDFIUM and FLECTAR_TEST_PDF; runs the actual mobile renderer on the host"]
    fn mobile_native_pages_and_rejection() {
        let _test = TEST_GATE.lock().unwrap();
        let data = Arc::new(std::fs::read(std::env::var_os("FLECTAR_TEST_PDF").unwrap()).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            for (index, zoom) in [(0, 100), (1, 100), (0, 200)] {
                let page = render(data.clone(), index, zoom).await.unwrap();
                assert_eq!(page.count, 2);
                assert_eq!(page.index, index);
                let center =
                    (page.height as usize / 2 * page.width as usize + page.width as usize / 2) * 4;
                let expected = if index == 0 {
                    [26, 102, 204, 255]
                } else {
                    [204, 51, 26, 255]
                };
                for (a, b) in page.pixels[center..center + 4].iter().zip(expected) {
                    assert!(a.abs_diff(b) <= 1);
                }
                assert_eq!(page.width, 1024 * zoom / 100);
                assert!(
                    page.pixels[..(page.width * page.height / 3 * 4) as usize]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .filter(|pixel| pixel[..3].iter().any(|v| *v < 230))
                        .count()
                        > 100,
                    "standard PDF font should render visible text"
                );
                if index == 0
                    && zoom == 100
                    && let Some(path) = std::env::var_os("FLECTAR_TEST_PDF_RGBA")
                {
                    std::fs::write(path, &page.pixels).unwrap();
                }
            }
            assert!(render(data.clone(), 9, 100).await.is_err());
            assert!(render(data, 0, 999).await.is_err());
            assert!(render(Arc::new(b"bad PDF".to_vec()), 0, 100).await.is_err());
            let cancelled = Cancellation::new();
            cancelled.cancelled.store(true, Ordering::Release);
            assert!(render_page(b"bad PDF", 0, 100, &cancelled).is_err());
        });
    }
}
