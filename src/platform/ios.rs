//! iOS integration. The Objective-C bridges in platform/ios are compiled by
//! build.rs; this module owns their Rust side.
mod documents;
mod oauth;
mod security;

use flectar_mail_core::models::AppIcon;
use std::{
    ffi::{CStr, c_void},
    path::PathBuf,
    sync::Arc,
};

unsafe extern "C" {
    fn install_pdf_lifecycle_observers();
    fn install_memory_pressure_observers();
    fn flectar_set_app_icon(name: *const std::ffi::c_char);
    fn flectar_register_background_checks();
    fn flectar_check_finished(context: *mut c_void, checked: bool);
    fn flectar_app_is_active() -> bool;
    fn flectar_background_refresh_state() -> std::ffi::c_int;
    fn flectar_open_notification_settings();
    fn flectar_open_app_settings();
    fn flectar_stay_awake_begin() -> *mut c_void;
    fn flectar_stay_awake_end(awake: *mut c_void);
}

/// Names this platform to the settings that describe it.
pub(crate) const NAME: &str = "ios";

/// Called by PdfPreview.m when the app is backgrounded or under memory
/// pressure.
#[unsafe(no_mangle)]
extern "C" fn flectar_suspend_pdf_preview() {
    crate::suspend_file_preview();
}

/// Called by MemoryPressure.m: `critical` for a memory warning, otherwise the
/// app moved to the background.
#[unsafe(no_mangle)]
extern "C" fn flectar_memory_pressure(critical: bool) {
    crate::memory::memory_pressure(if critical {
        crate::memory::MemoryPressure::Critical
    } else {
        crate::memory::MemoryPressure::Moderate
    });
}

pub(crate) fn watch_memory_pressure() {
    // SAFETY: installs NSNotificationCenter observers that live for the
    // process; the Objective-C side takes no arguments.
    unsafe { install_memory_pressure_observers() };
}

/// libmalloc on iOS returns freed pages to the system on its own, and its
/// pressure-relief call does nothing there.
pub(crate) fn purge_allocator(_purge: crate::memory::Purge) {}

/// Called by BackgroundChecks.m when iOS starts a check for new mail.
/// `context` goes back to flectar_check_finished once the check is done; the
/// returned handle cancels it until flectar_release_check.
#[unsafe(no_mangle)]
extern "C" fn flectar_check_for_mail(context: *mut c_void) -> *mut c_void {
    struct Context(*mut c_void);
    // SAFETY: the Objective-C side's reference to its check object, which it
    // takes back on any thread.
    unsafe impl Send for Context {}
    impl Context {
        fn finish(self, checked: bool) {
            // SAFETY: the reference flectar_check_for_mail was given, handed
            // back once.
            unsafe { flectar_check_finished(self.0, checked) };
        }
    }
    let context = Context(context);
    let check = crate::notifications::check_in_background(move |checked| context.finish(checked));
    Box::into_raw(Box::new(check)).cast()
}

/// Called by BackgroundChecks.m when iOS wants the check's time back.
#[unsafe(no_mangle)]
extern "C" fn flectar_cancel_check(check: *mut c_void) {
    // SAFETY: a handle flectar_check_for_mail returned and not yet released.
    unsafe { &*check.cast::<crate::notifications::BackgroundCheck>() }.cancel();
}

/// Called by BackgroundChecks.m once nothing can cancel the check any more.
#[unsafe(no_mangle)]
extern "C" fn flectar_release_check(check: *mut c_void) {
    // SAFETY: a handle flectar_check_for_mail returned, released once.
    drop(unsafe { Box::from_raw(check.cast::<crate::notifications::BackgroundCheck>()) });
}

/// Called by BackgroundChecks.m to decide whether to ask iOS for the next
/// check.
#[unsafe(no_mangle)]
extern "C" fn flectar_wants_background_checks() -> bool {
    crate::notifications::background_checks_wanted()
}

/// Called by BackgroundChecks.m on the main thread when the app comes to the
/// front.
#[unsafe(no_mangle)]
extern "C" fn flectar_app_became_active() {
    crate::notifications::app_became_active();
    let _ = slint::invoke_from_event_loop(crate::notifications::ui::came_to_front);
}

/// Called by BackgroundChecks.m on the main thread when Background App
/// Refresh or Low Power Mode changes.
#[unsafe(no_mangle)]
extern "C" fn flectar_background_refresh_changed() {
    let _ = slint::invoke_from_event_loop(crate::notifications::ui::refresh_availability);
}

/// Whether the app is in front of the user. Main thread only.
pub(crate) fn app_is_active() -> bool {
    // SAFETY: called on the main thread, as the Objective-C side requires.
    unsafe { flectar_app_is_active() }
}

/// Whether iOS checks for new mail in the background now.
pub(crate) fn background_refresh() -> super::BackgroundRefresh {
    // SAFETY: called on the main thread, as the Objective-C side requires.
    match unsafe { flectar_background_refresh_state() } {
        0 => super::BackgroundRefresh::On,
        2 => super::BackgroundRefresh::LowPower,
        _ => super::BackgroundRefresh::Off,
    }
}

/// Keeps the app running in the background while it lives, for as long as
/// iOS allows: about half a minute.
pub(crate) struct Awake(*mut c_void);

// SAFETY: a handle the Objective-C side takes back on any thread.
unsafe impl Send for Awake {}

impl Drop for Awake {
    fn drop(&mut self) {
        // SAFETY: the handle flectar_stay_awake_begin returned, ended once.
        unsafe { flectar_stay_awake_end(self.0) }
    }
}

/// Keep the app running in the background until the returned guard drops.
pub(crate) fn stay_awake() -> Awake {
    // SAFETY: UIKit takes background tasks on any thread.
    Awake(unsafe { flectar_stay_awake_begin() })
}

/// Open the app's notification settings.
pub(crate) fn open_notification_settings() {
    // SAFETY: opens a system URL on the main queue; no arguments.
    unsafe { flectar_open_notification_settings() }
}

/// Open the app's page in Settings, where Background App Refresh is.
pub(crate) fn open_app_settings() {
    // SAFETY: opens a system URL on the main queue; no arguments.
    unsafe { flectar_open_app_settings() }
}

/// Build the platform context for the process launcher in src/desktop.rs.
pub fn launch_context() -> Result<crate::PlatformContext, flectar_mail_core::error::CoreError> {
    let root = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        flectar_mail_core::error::CoreError::Other(
            "iOS did not supply the application sandbox.".into(),
        )
    })?;
    let sign_in = oauth::broker();
    let authorize = sign_in.clone();
    // SAFETY: on the main thread before the app finishes launching, where iOS
    // takes background task handlers.
    unsafe { flectar_register_background_checks() };
    Ok(crate::PlatformContext {
        // Mail links open in Safari; sign-in pages in a web authentication
        // session that returns the provider's redirect to the app.
        external_urls: crate::ExternalUrlOpener::default()
            .with_authorization(move |url| authorize.authorize(url)),
        // Set up before the app finishes launching, so the tap that launched
        // it reaches the app.
        notifications: crate::notifications::host(),
        privacy_consent: crate::PrivacyConsent::default(),
        paths: flectar_mail_core::config::Paths::new(
            root.join("Library/Application Support/Flectar"),
            root.join("Library/Caches/Flectar"),
        ),
        documents: Arc::new(documents::IosDocuments),
        credentials: Arc::new(super::keychain::KeychainCredentialStore::new(
            security::SecurityKeychain,
        )),
        oauth_redirects: Arc::new(sign_in),
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

/// iOS apps may switch to alternate icons, so Settings offers the choice.
pub(crate) const SWITCHES_APP_ICON: bool = true;

/// Show the alternate icon set declared in project.yml, or the primary icon.
pub(crate) fn show_app_icon(icon: AppIcon) {
    let name: Option<&CStr> = match icon {
        AppIcon::Orange => None,
        AppIcon::Graphite => Some(c"AppIconGraphite"),
        AppIcon::Light => Some(c"AppIconLight"),
        AppIcon::Blue => Some(c"AppIconBlue"),
        AppIcon::Green => Some(c"AppIconGreen"),
        AppIcon::Purple => Some(c"AppIconPurple"),
    };
    // SAFETY: AppIcon.m copies the NUL-terminated name, or takes NULL for the
    // primary icon, before returning.
    unsafe { flectar_set_app_icon(name.map_or(std::ptr::null(), CStr::as_ptr)) }
}

/// iOS reaches system fonts through the fontique system backend.
pub(crate) fn email_fonts() -> Option<&'static super::EmailFonts> {
    None
}
