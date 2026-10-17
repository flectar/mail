//! iOS integration. The Objective-C bridges in platform/ios are compiled by
//! build.rs; this module owns their Rust side.
mod documents;

use std::{path::PathBuf, sync::Arc};

unsafe extern "C" {
    fn install_pdf_lifecycle_observers();
}

/// Called by PdfPreview.m when the app is backgrounded or under memory
/// pressure.
#[unsafe(no_mangle)]
extern "C" fn flectar_suspend_pdf_preview() {
    crate::suspend_file_preview();
}

/// Build the platform context for the process launcher in src/desktop.rs.
pub fn launch_context() -> Result<crate::PlatformContext, flectar_mail_core::error::CoreError> {
    let root = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        flectar_mail_core::error::CoreError::Other(
            "iOS did not supply the application sandbox.".into(),
        )
    })?;
    Ok(crate::PlatformContext {
        paths: flectar_mail_core::config::Paths::new(
            root.join("Library/Application Support/Flectar"),
            root.join("Library/Caches/Flectar"),
        ),
        documents: Arc::new(documents::IosDocuments),
        // The Keychain bridge is not integrated yet; account setup reports
        // that instead of storing secrets anywhere else.
        credentials: Arc::new(
            flectar_mail_core::accounts::credentials::UnavailableCredentialStore,
        ),
        oauth_redirects: Arc::new(
            flectar_mail_core::oauth::redirect::LoopbackRedirectBroker::default(),
        ),
    })
}

/// Slint's documented iOS configuration: winit with Skia.
pub(crate) fn select_renderer() -> Result<&'static str, slint::PlatformError> {
    slint::BackendSelector::new()
        .backend_name("winit".to_owned())
        .renderer_name("skia".to_owned())
        .select()?;
    Ok("platform")
}

pub(crate) fn pdfium_library() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .map(|p| p.join("Frameworks/PDFium.framework/PDFium"))
        .ok_or_else(|| "Missing application bundle.".into())
}

pub(crate) fn lower_current_thread_priority() {
    // SAFETY: this only changes the calling thread's quality-of-service class.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

pub(crate) fn install_preview_lifecycle() {
    // SAFETY: PdfPreview.m registers NotificationCenter observers that call
    // flectar_suspend_pdf_preview; registration has no other preconditions.
    unsafe { install_pdf_lifecycle_observers() }
}

/// Android and iOS reach system fonts through the fontique system backend.
pub(crate) fn email_font_dir() -> Option<&'static std::path::Path> {
    None
}
