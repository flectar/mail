#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(glibc)]
    tune_allocator();
    // Slint 1.18 keeps the native window and its rendering surface when
    // hidden on X11 and Windows. Use its window recreation path for tray mode,
    // as it already does on Wayland, so a hidden window holds no surface.
    // macOS keeps its window: AppKit attributes set after creation would be
    // lost on recreation.
    #[cfg(any(linux_desktop, target_os = "windows"))]
    if std::env::var_os("SLINT_DESTROY_WINDOW_ON_HIDE").is_none() {
        // SAFETY: the desktop entry point has not started any threads or
        // initialized libraries yet. Never mutate this from a tray callback.
        unsafe { std::env::set_var("SLINT_DESTROY_WINDOW_ON_HIDE", "1") };
    }
    #[cfg(desktop)]
    if let Some(code) = flectar_mail::pdf_preview::run_worker_if_requested() {
        std::process::exit(code);
    }
    flectar_mail::run_desktop(flectar_mail::platform::launch_context()?)
}

/// The mail shell has a small number of worker threads. One glibc allocation
/// arena is enough and prevents per-thread arenas from retaining render/sync
/// scratch memory after a burst of work. Set in-process rather than through
/// the environment, which the browser and other launched programs would
/// inherit. Explicit user tuning wins.
#[cfg(glibc)]
fn tune_allocator() {
    let tunables = std::env::var("GLIBC_TUNABLES").unwrap_or_default();
    let user_set = |variable: &str, tunable: &str| {
        std::env::var_os(variable).is_some() || tunables.contains(tunable)
    };
    // SAFETY: mallopt only adjusts allocator parameters; no pointers change.
    unsafe {
        if !user_set("MALLOC_ARENA_MAX", "glibc.malloc.arena_max") {
            libc::mallopt(libc::M_ARENA_MAX, 1);
        }
        if !user_set("MALLOC_TRIM_THRESHOLD_", "glibc.malloc.trim_threshold") {
            libc::mallopt(libc::M_TRIM_THRESHOLD, 64 * 1024);
        }
    }
}
