//! Native HarmonyOS host for the unchanged Slint/Blitz application.
mod backend;
mod framebuffer;
#[cfg(target_env = "ohos")]
mod gpu;
mod native;
#[cfg(all(feature = "application", debug_assertions))]
mod offline_fixture;
mod performance;
#[cfg(feature = "application")]
mod services;

use backend::Event;
use std::{
    ffi::{CStr, c_char},
    path::PathBuf,
    sync::{Mutex, OnceLock, mpsc},
};

static EVENTS: OnceLock<Mutex<Option<mpsc::Sender<Event>>>> = OnceLock::new();
static PERFORMANCE_DIAGNOSTICS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(crate) fn diagnostics_enabled() -> bool {
    PERFORMANCE_DIAGNOSTICS.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn send(event: Event) {
    if let Some(sender) = EVENTS
        .get()
        .and_then(|state| state.lock().ok())
        .and_then(|state| state.clone())
    {
        let _ = sender.send(event);
    }
}

// All incoming data is copied during the synchronous C ABI call. No borrowed native
// pointer survives a callback, and all Slint objects are created on the owner thread.
unsafe fn string(pointer: *const c_char) -> String {
    if pointer.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    }
}
unsafe fn utf16(pointer: *const u16, length: usize) -> String {
    if pointer.is_null() || length > 16 * 1024 * 1024 {
        String::new()
    } else {
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) })
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartOptions {
    files_dir: PathBuf,
    cache_dir: PathBuf,
    #[serde(default)]
    font_paths: Vec<String>,
    #[serde(default)]
    system_language: Option<String>,
    #[serde(default)]
    debug_ui_fixture: bool,
    #[serde(default)]
    performance_diagnostics: bool,
}

/// Starts once per process. Ability recreation resumes the retained UI owner thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn feihong_start(options: *const c_char) -> i32 {
    let options: StartOptions = match serde_json::from_str(&unsafe { string(options) }) {
        Ok(options) => options,
        Err(error) => {
            native::log(&format!("start options: {error}"));
            return -1;
        }
    };
    let state = EVENTS.get_or_init(|| Mutex::new(None));
    let mut current = state.lock().unwrap_or_else(|e| e.into_inner());
    if current.is_some() {
        send_unlocked(&current, Event::Foreground(true));
        return 0;
    }
    let performance = cfg!(debug_assertions) && options.performance_diagnostics;
    PERFORMANCE_DIAGNOSTICS.store(performance, std::sync::atomic::Ordering::Relaxed);
    let (sender, receiver) = mpsc::channel();
    *current = Some(sender.clone());
    // Native Surface callbacks hold surfaceMutex before posting to EVENTS.
    // Release EVENTS before entering native diagnostics to keep that order.
    drop(current);
    native::set_performance_diagnostics(performance);
    let spawn = std::thread::Builder::new()
        .name("feihong-ui".into())
        .spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
                    slint::platform::set_platform(Box::new(backend::OhosPlatform::new(
                        sender, receiver,
                    )))
                    .map_err(|e| e.to_string())?;
                    let mut fonts = options.font_paths;
                    // OHOS font discovery is explicit; Fontconfig is not available on this target.
                    // Host may additionally supply app-private licensed bundled font files.
                    for path in [
                        "/system/fonts/HarmonyOS_Sans_SC.ttf",
                        "/system/fonts/HarmonyOS_Sans.ttf",
                        "/system/fonts/NotoSansCJK-Regular.ttc",
                        "/system/fonts/NotoColorEmoji.ttf",
                    ] {
                        if std::path::Path::new(path).is_file() {
                            fonts.push(path.to_owned());
                        }
                    }
                    if let Ok(entries) = std::fs::read_dir("/system/fonts") {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path
                                .extension()
                                .and_then(|e| e.to_str())
                                .is_some_and(|e| matches!(e, "ttf" | "ttc" | "otf"))
                            {
                                fonts.push(path.to_string_lossy().into_owned());
                            }
                        }
                    }
                    fonts.sort();
                    fonts.dedup();
                    backend::register_fonts(&fonts)?;
                    #[cfg(all(feature = "application", target_env = "ohos"))]
                    flectar_mail::configure_platform_fonts(&fonts);
                    #[cfg(all(debug_assertions, feature = "application", target_env = "ohos"))]
                    if performance {
                        flectar_mail::configure_platform_font_diagnostics(native::log);
                    }
                    #[cfg(feature = "application")]
                    {
                        #[cfg(debug_assertions)]
                        services::configure_ui_fixture(options.debug_ui_fixture);
                        #[cfg(not(debug_assertions))]
                        let _ = options.debug_ui_fixture;
                        if let Some(locale) = options.system_language {
                            services::configure_system_locale(locale);
                        }
                        let platform =
                            services::platform_context(options.files_dir, options.cache_dir)?;
                        let platform = services::prepare_ui_fixture(platform)?;
                        flectar_mail::run(platform).map_err(|e| e.to_string())?;
                    }
                    #[cfg(not(feature = "application"))]
                    {
                        let _ = (
                            options.files_dir,
                            options.cache_dir,
                            options.system_language,
                            options.debug_ui_fixture,
                        );
                    }
                    Ok(())
                }));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => native::log(&format!("startup failed: {error}")),
                Err(_) => native::log("Rust UI thread panicked"),
            }
            // Slint's global EventLoopProxy is process-wide; do not create a second provider.
        });
    if let Err(error) = spawn {
        *state.lock().unwrap_or_else(|e| e.into_inner()) = None;
        native::log(&error.to_string());
        return -2;
    }
    0
}
fn send_unlocked(sender: &Option<mpsc::Sender<Event>>, event: Event) {
    if let Some(sender) = sender {
        let _ = sender.send(event);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn feihong_set_viewport(
    width: u32,
    height: u32,
    scale: f32,
    insets: *const c_char,
) {
    let values: serde_json::Value =
        serde_json::from_str(&unsafe { string(insets) }).unwrap_or_default();
    let edge = |name: &str| {
        values
            .get(name)
            .and_then(|v| v.as_f64())
            .unwrap_or(0.)
            .max(0.)
            .min(i32::MAX as f64) as i32
    };
    send(Event::Viewport {
        width,
        height,
        density: scale,
        insets: i_slint_core::lengths::PhysicalEdges::new(
            edge("top"),
            edge("bottom"),
            edge("left"),
            edge("right"),
        ),
    });
}
#[unsafe(no_mangle)]
pub extern "C" fn feihong_surface(generation: u64, available: bool) {
    send(Event::Surface {
        generation,
        available,
    });
}
#[unsafe(no_mangle)]
pub extern "C" fn feihong_vsync() {
    send(Event::Frame);
}
#[unsafe(no_mangle)]
pub extern "C" fn feihong_set_foreground(value: bool) {
    send(Event::Foreground(value));
}
#[unsafe(no_mangle)]
pub extern "C" fn feihong_focus(value: bool) {
    send(Event::Focus(value));
}
#[unsafe(no_mangle)]
pub extern "C" fn feihong_stop() {
    send(Event::Suspend);
}
#[unsafe(no_mangle)]
pub extern "C" fn feihong_pointer(kind: i32, x: f32, y: f32, button: i32) {
    send(Event::Pointer { kind, x, y, button });
}
#[unsafe(no_mangle)]
pub extern "C" fn feihong_touch(kind: i32, id: i32, x: f32, y: f32) {
    send(Event::Touch { kind, id, x, y });
}
#[unsafe(no_mangle)]
pub extern "C" fn feihong_scroll(x: f32, y: f32, dx: f32, dy: f32) {
    send(Event::Scroll { x, y, dx, dy });
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn feihong_key(pressed: bool, text: *const c_char) {
    send(Event::Key {
        pressed,
        text: unsafe { string(text) },
    });
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn feihong_ime_insert(text: *const u16, length: usize) {
    send(Event::Insert(unsafe { utf16(text, length) }));
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn feihong_ime_preview(
    text: *const u16,
    length: usize,
    start: i32,
    end: i32,
) {
    send(Event::Preview {
        text: unsafe { utf16(text, length) },
        start,
        end,
    });
}
#[unsafe(no_mangle)]
pub extern "C" fn feihong_ime_finish_preview() {
    send(Event::FinishPreview);
}
#[unsafe(no_mangle)]
pub extern "C" fn feihong_ime_selection(start: i32, end: i32) {
    send(Event::Selection { start, end });
}
#[unsafe(no_mangle)]
pub extern "C" fn feihong_ime_delete(forward: bool, units: i32) {
    send(Event::Delete {
        forward,
        units: units.max(0) as usize,
    });
}
#[unsafe(no_mangle)]
pub extern "C" fn feihong_handle_back() -> bool {
    let (sender, receiver) = mpsc::sync_channel(1);
    send(Event::Back(sender));
    receiver
        .recv_timeout(std::time::Duration::from_millis(100))
        .unwrap_or(false)
}
