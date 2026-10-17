//! OpenHarmony integration used by platform/ohos. The ArkTS shell and the
//! XComponent Slint backend live in that host tree; this module only holds
//! what the shared application needs at runtime.

use std::{path::PathBuf, sync::OnceLock};

static PDFIUM_LIBRARY: OnceLock<PathBuf> = OnceLock::new();

/// Supply the application's native library directory (the hap's `libs/<abi>`,
/// which the host derives from its own loaded module path). Never a
/// document-controlled path.
pub fn configure_pdfium(directory: PathBuf) -> Result<(), String> {
    if !directory.is_absolute() {
        return Err("OpenHarmony did not provide an absolute native library directory.".into());
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

/// The slint-ohos backend installs the platform (and selects the renderer
/// itself, with an automatic software fallback) before entering the shared
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

/// Preview lifecycle is owned by the ArkTS shell.
pub(crate) fn install_preview_lifecycle() {}
