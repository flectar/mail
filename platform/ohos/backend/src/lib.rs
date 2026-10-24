//! Minimal Slint platform backend for OpenHarmony.
//!
//! Architecture (see `docs/plans/2026-10-11-ohos-m2-minimal-backend.md`):
//!
//! - The OHOS main thread hosts the ArkTS XComponent. Its native callbacks
//!   (surface lifecycle + touch) arrive on the main thread and are converted
//!   into [`event::BackendEvent`]s.
//! - [`start`] spawns the Slint application thread. It installs
//!   [`platform::OhosPlatform`] and runs the application closure. All Slint
//!   state lives on that thread.
//! - Rendering uses Slint's public software renderer
//!   (`slint::platform::software_renderer`) and presents through the
//!   `OHNativeWindow` buffer queue ([`present::Presenter`]).
//!
//! Only public Slint APIs are used.

pub mod asset;
pub mod clipboard;
pub mod display;
pub mod event;
pub mod ffi;
pub mod fonts;
pub mod gpu;
pub mod hold;
pub mod ime;
pub mod log;
pub mod perf;
pub mod platform;
pub mod present;
pub mod window;
pub mod xcomponent;

use core::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// The closure that builds and runs the Slint application, executed on the
/// dedicated Slint thread.
pub type AppFactory =
    Box<dyn FnOnce() -> Result<(), Box<dyn std::error::Error + Send + Sync>> + Send>;

/// Start the Slint application on a dedicated thread and return immediately.
///
/// Must be called from the OHOS main thread (the NAPI module `Init`), before
/// the first XComponent surface callback can arrive.
pub fn start(
    app_factory: impl FnOnce() -> Result<(), Box<dyn std::error::Error + Send + Sync>> + Send + 'static,
) -> Result<(), std::io::Error> {
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let gate = present::SurfaceGate::new();
    let visible = Arc::new(AtomicBool::new(true));
    xcomponent::install(event_tx.clone(), gate.clone(), visible.clone());
    let quit = Arc::new(AtomicBool::new(false));

    std::thread::Builder::new()
        .name("slint-ohos".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            log::info("slint thread started");
            let platform = platform::OhosPlatform::new(event_tx, event_rx, gate, quit, visible);
            if let Err(err) = slint::platform::set_platform(Box::new(platform)) {
                log::error(&format!("set_platform failed: {err}"));
                return;
            }
            log::info("slint platform installed; building UI");
            let han_families = fonts::register_system_fonts();
            log::info(&format!(
                "system fonts registered; Han fallback families: {han_families}"
            ));
            if let Err(err) = app_factory() {
                log::error(&format!("application terminated: {err}"));
            }
        })?;
    Ok(())
}

/// Register the XComponent callbacks on the raw component pointer obtained
/// from the NAPI `__NATIVE_XCOMPONENT_OBJ__` external.
///
/// # Safety
///
/// `component` must be the live pointer wrapped by the XComponent instance
/// passed to the NAPI module's init; the OHOS loader keeps it valid for the
/// process lifetime and the callback table is never unregistered.
pub unsafe fn register_xcomponent(component: *mut c_void) -> i32 {
    // SAFETY: the caller guarantees this is the live XComponent pointer.
    unsafe { xcomponent::register(component.cast()) }
}
