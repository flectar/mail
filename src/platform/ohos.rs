//! OpenHarmony integration used by platform/ohos. The ArkTS shell and the
//! XComponent Slint backend live in that host tree; this module only holds
//! what the shared application needs at runtime.

use flectar_mail_core::models::AppIcon;
use std::{
    path::PathBuf,
    sync::{
        Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

static PDFIUM_LIBRARY: OnceLock<PathBuf> = OnceLock::new();

/// Names this platform to the settings that describe it.
pub(crate) const NAME: &str = "ohos";

/// The native window is the app while the page is showing; account changes
/// that request notification permission come from that foreground session.
pub(crate) fn app_is_active() -> bool {
    true
}

/// OpenHarmony has no spell checker for apps.
pub(crate) const CHECKS_SPELLING: bool = false;

pub(crate) fn spell_checker() -> Option<std::sync::Arc<dyn crate::spelling::Checker>> {
    None
}

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

/// `onMemoryLevel` from the ability, forwarded by the page.
pub fn memory_level(level: i32) {
    if let Some(pressure) = super::memory_levels::ohos_memory_level(level) {
        crate::memory::memory_pressure(pressure);
    }
}

/// The ability forwards memory levels as they arrive; there is nothing to
/// subscribe to.
pub(crate) fn watch_memory_pressure() {}

/// OpenHarmony's C library stubs `mallopt` and has no `malloc_trim`, so only
/// the app's own caches can be released.
pub(crate) fn purge_allocator(_purge: crate::memory::Purge) {}

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

/// Switching the home screen icon is a system API on OpenHarmony
/// (`bundleManager.enableDynamicIcon`), so Settings does not offer the choice.
pub(crate) const SWITCHES_APP_ICON: bool = false;

/// Only the default icon reaches here; see [`SWITCHES_APP_ICON`].
pub(crate) fn show_app_icon(_icon: AppIcon) {}

static EMAIL_FONTS: OnceLock<super::EmailFonts> = OnceLock::new();

/// Supply the platform fonts and the fallback policy the backend applies to
/// the Slint UI, before the application starts. OpenHarmony ships no
/// Fontconfig, so the email renderer and the composer load them explicitly.
pub fn configure_email_fonts(fonts: super::EmailFonts) {
    let _ = EMAIL_FONTS.set(fonts);
}

pub(crate) fn email_fonts() -> Option<&'static super::EmailFonts> {
    EMAIL_FONTS.get()
}

/// Whether this build may send people to the web checkout for Flectar Mail Sync.
/// HarmonyOS apps ship only through AppGallery, which requires Huawei's
/// in-app purchases, so this build only shows what the group pays.
pub(crate) fn sells_sync() -> bool {
    false
}

/// The marketing name the page read from `deviceInfo`; empty until it reports.
struct DeviceName {
    name: Mutex<String>,
    reported: Condvar,
}

static DEVICE_NAME: OnceLock<DeviceName> = OnceLock::new();

fn device_name() -> &'static DeviceName {
    DEVICE_NAME.get_or_init(|| DeviceName {
        name: Mutex::new(String::new()),
        reported: Condvar::new(),
    })
}

/// How long Sync waits for the page's name at startup. The page reports it
/// right after the native module starts, before the window reaches Sync.
const DEVICE_NAME_WAIT: Duration = Duration::from_millis(500);

/// The page reports the marketing name or, without one, the product model,
/// before the window reaches Sync. An empty report keeps the previous name.
pub fn set_device_name(name: &str) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    let slot = device_name();
    *slot
        .name
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = name.to_owned();
    slot.reported.notify_all();
}

/// How this device appears in Flectar Mail Sync's device list: by its
/// marketing name, such as "HUAWEI MateBook Pro", as Android and iOS use
/// their model names. The page reports it through [`set_device_name`]; if the
/// page is late, wait briefly rather than sending an unnamed device.
pub(crate) fn device_description() -> flectar_mail_core::device_sync::DeviceDescription {
    let slot = device_name();
    let guard = slot
        .name
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (guard, _) = slot
        .reported
        .wait_timeout_while(guard, DEVICE_NAME_WAIT, |name| name.is_empty())
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let reported = guard.clone();
    drop(guard);
    let name = if reported.is_empty() {
        // The page has not reported; keep the shared host-name fallback.
        flectar_mail_core::device_sync::DeviceDescription::default().name
    } else {
        reported
    };
    flectar_mail_core::device_sync::DeviceDescription {
        name,
        platform: "harmonyos".to_owned(),
    }
}

/// The animation reduction setting, as the page last reported it.
static REDUCE_MOTION: AtomicBool = AtomicBool::new(false);

/// The page reports animation reduction, from the accessibility settings of
/// API 23 and later, when it shows and whenever the setting changes.
pub fn set_reduce_motion(reduced: bool) {
    REDUCE_MOTION.store(reduced, Ordering::Relaxed);
    crate::motion::system_changed();
}

pub(crate) fn reduce_motion() -> bool {
    REDUCE_MOTION.load(Ordering::Relaxed)
}

/// Changes arrive through `set_reduce_motion`.
pub(crate) fn watch_reduce_motion() {}

#[cfg(test)]
mod tests {
    #[test]
    fn trims_the_reported_name_and_ignores_empty_reports() {
        super::set_device_name("  HUAWEI MateBook Pro  ");
        assert_eq!(super::device_description().name, "HUAWEI MateBook Pro");
        super::set_device_name("   ");
        assert_eq!(super::device_description().name, "HUAWEI MateBook Pro");
    }
}
