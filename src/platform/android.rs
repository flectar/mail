//! Android integration used by platform/android. The Kotlin/Java activity and
//! JNI services live in that shell; this module only holds what the shared
//! application needs at runtime.
use flectar_mail_core::models::AppIcon;
use std::{
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

static PDFIUM_LIBRARY: OnceLock<PathBuf> = OnceLock::new();
/// Hands the chosen icon's name to the activity.
type LauncherIconSwitch = Box<dyn Fn(&str) + Send + Sync>;
static LAUNCHER_ICON: OnceLock<LauncherIconSwitch> = OnceLock::new();

/// Names this platform to the settings that describe it.
pub(crate) const NAME: &str = "android";

/// The spell checker is the one the user chose in Android's settings, which the host brings
/// as `PlatformContext::spelling`.
pub(crate) const CHECKS_SPELLING: bool = false;

pub(crate) fn spell_checker() -> Option<std::sync::Arc<dyn crate::spelling::Checker>> {
    None
}

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

/// Bionic returns free pages through mallopt. M_PURGE_FAST (API 37) is bounded
/// for frequent use, M_PURGE (API 28) returns every free page, and M_PURGE_ALL
/// (API 34) also drains every thread cache and takes longer. Options a device
/// lacks return 0, which is harmless, and fall back to M_PURGE.
pub(crate) fn purge_allocator(purge: crate::memory::Purge) {
    use crate::memory::Purge;
    use std::ffi::c_int;
    const M_PURGE: c_int = -101;
    const M_PURGE_ALL: c_int = -104;
    const M_PURGE_FAST: c_int = -105;
    unsafe extern "C" {
        fn mallopt(param: c_int, value: c_int) -> c_int;
    }
    let preferred = match purge {
        Purge::Fast => Some(M_PURGE_FAST),
        Purge::Normal => None,
        Purge::All => Some(M_PURGE_ALL),
    };
    // SAFETY: these options only return free pages; no pointers change.
    unsafe {
        if preferred.is_none_or(|option| mallopt(option, 0) != 1) {
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

/// Whether the activity is in front of the user, as its host reports it.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// The activity was resumed (`true`) or paused. Coming to the front, the
/// user sees what came while the app wasn't running, and with an account set
/// up it is the moment to ask for permission to notify.
pub fn set_app_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
    if active {
        crate::motion::system_changed();
        crate::notifications::app_became_active();
        let _ = slint::invoke_from_event_loop(crate::notifications::ui::came_to_front);
    }
}

/// Whether the activity is in front of the user.
pub(crate) fn app_is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

type ReduceMotion = Box<dyn Fn() -> bool + Send + Sync>;

static REDUCE_MOTION: OnceLock<ReduceMotion> = OnceLock::new();

/// Supply the bridge to FlectarActivity.animationsRemoved, once.
pub fn configure_reduce_motion(read: impl Fn() -> bool + Send + Sync + 'static) {
    let _ = REDUCE_MOTION.set(Box::new(read));
}

/// Remove animations, in the accessibility settings, asks apps to reduce
/// motion.
pub(crate) fn reduce_motion() -> bool {
    REDUCE_MOTION.get().is_some_and(|read| read())
}

/// Android announces no change of the setting, so the app reads it again
/// whenever it comes back to the front (`set_app_active`).
pub(crate) fn watch_reduce_motion() {}

/// What the host reports and opens about the app's work in the background.
pub struct BackgroundBridge {
    /// Whether Android lets the app check for mail in the background now.
    pub state: Box<dyn Fn() -> super::BackgroundRefresh + Send + Sync>,
    /// The app's page in Settings, where its background use is allowed.
    pub open_settings: Box<dyn Fn() + Send + Sync>,
}

static BACKGROUND: OnceLock<BackgroundBridge> = OnceLock::new();

/// Supply the bridge to the host's background state, once.
pub fn configure_background(bridge: BackgroundBridge) {
    let _ = BACKGROUND.set(bridge);
}

pub(crate) fn background_refresh() -> super::BackgroundRefresh {
    BACKGROUND
        .get()
        .map_or(super::BackgroundRefresh::On, |bridge| (bridge.state)())
}

pub(crate) fn open_app_settings() {
    if let Some(bridge) = BACKGROUND.get() {
        (bridge.open_settings)();
    }
}

/// Keeps a mail store opened for a background errand open while the guard
/// lives, such as while a button's action goes to the server.
pub(crate) fn stay_awake() -> crate::notifications::engine::Hold {
    crate::notifications::engine::hold()
}
