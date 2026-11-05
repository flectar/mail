//! Android integration used by platform/android. The Kotlin/Java activity and
//! JNI services live in that shell; this module only holds what the shared
//! application needs at runtime.
use flectar_mail_core::models::AppIcon;
use std::{path::PathBuf, sync::OnceLock};

static PDFIUM_LIBRARY: OnceLock<PathBuf> = OnceLock::new();
/// Hands the chosen icon's name to the activity.
type LauncherIconSwitch = Box<dyn Fn(&str) + Send + Sync>;
static LAUNCHER_ICON: OnceLock<LauncherIconSwitch> = OnceLock::new();

/// Names this platform to the settings that describe it.
pub(crate) const NAME: &str = "android";

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

/// `onTrimMemory` from the activity, with the device's API level.
pub fn trim_memory(level: i32, sdk: i32) {
    if let Some(pressure) = super::memory_levels::android_trim_level(level, sdk) {
        crate::memory::memory_pressure(pressure);
    }
}

/// The activity forwards trim levels as they arrive; there is nothing to
/// subscribe to.
pub(crate) fn watch_memory_pressure() {}

/// Bionic returns free pages through mallopt. M_PURGE (API 28) is quick;
/// M_PURGE_ALL (API 34) also drains every thread cache and takes longer, so it
/// is kept for critical pressure. Releases without them return 0, which is
/// harmless; Zygote falls back the same way.
pub(crate) fn purge_allocator(urgent: bool) {
    use std::ffi::c_int;
    const M_PURGE: c_int = -101;
    const M_PURGE_ALL: c_int = -104;
    unsafe extern "C" {
        fn mallopt(param: c_int, value: c_int) -> c_int;
    }
    // SAFETY: these options only return free pages; no pointers change.
    unsafe {
        if !urgent || mallopt(M_PURGE_ALL, 0) != 1 {
            mallopt(M_PURGE, 0);
        }
    }
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

/// Supply the bridge to FlectarActivity.setLauncherIcon, which receives the
/// icon's name and switches the launcher entry once the app leaves the
/// foreground.
pub fn configure_launcher_icon(switch: impl Fn(&str) + Send + Sync + 'static) {
    let _ = LAUNCHER_ICON.set(Box::new(switch));
}

/// Android apps may switch their launcher icon, so Settings offers the choice.
pub(crate) const SWITCHES_APP_ICON: bool = true;

pub(crate) fn show_app_icon(icon: AppIcon) {
    if let Some(switch) = LAUNCHER_ICON.get() {
        switch(icon.as_str());
    }
}

/// Android and iOS reach system fonts through the fontique system backend.
pub(crate) fn email_fonts() -> Option<&'static super::EmailFonts> {
    None
}
