//! Android integration used by platform/android. The Kotlin/Java activity and
//! JNI services live in that shell; this module only holds what the shared
//! application needs at runtime.
use std::{path::PathBuf, sync::OnceLock};

static PDFIUM_LIBRARY: OnceLock<PathBuf> = OnceLock::new();

/// Supply ApplicationInfo.nativeLibraryDir, never a document-controlled path.
/// The APK uses extracted native libraries so this is a private installed file.
pub fn configure_pdfium(directory: PathBuf) -> Result<(), String> {
    if !directory.is_absolute() {
        return Err("Android did not provide an absolute native library directory.".into());
    }
    let path = directory.join("libpdfium.so");
    if let Some(existing) = PDFIUM_LIBRARY.get() {
        return if existing == &path {
            Ok(())
        } else {
            Err("PDF library directory changed.".into())
        };
    }
    PDFIUM_LIBRARY
        .set(path)
        .map_err(|_| "PDF library already configured.".into())
}

pub(crate) fn pdfium_library() -> Result<PathBuf, String> {
    PDFIUM_LIBRARY
        .get()
        .cloned()
        .ok_or_else(|| "PDF preview is not configured.".into())
}

/// The shell installs Slint's activity backend before entering the shared
/// application, so only the renderer name is reported here.
pub(crate) fn select_renderer() -> Result<&'static str, slint::PlatformError> {
    Ok("software")
}

pub(crate) fn lower_current_thread_priority() {
    // SAFETY: nice only adjusts the calling thread's scheduling priority.
    unsafe {
        libc::nice(10);
    }
}

/// Lifecycle callbacks arrive through FlectarActivity's JNI bridge.
pub(crate) fn install_preview_lifecycle() {}

/// Android and iOS reach system fonts through the fontique system backend.
pub(crate) fn email_font_dir() -> Option<&'static std::path::Path> {
    None
}
