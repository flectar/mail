#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Slint 1.18 normally keeps X11's software surface when hidden. Use its
    // window recreation path for tray mode, as it already does on Wayland.
    #[cfg(target_os = "linux")]
    if std::env::var_os("SLINT_DESTROY_WINDOW_ON_HIDE").is_none() {
        // SAFETY: the desktop entry point has not started any threads or
        // initialized libraries yet. Never mutate this from a tray callback.
        unsafe { std::env::set_var("SLINT_DESTROY_WINDOW_ON_HIDE", "1") };
    }
    #[cfg(not(target_os = "ios"))]
    if let Some(code) = flectar_mail::pdf_preview::run_worker_if_requested() {
        std::process::exit(code);
    }
    flectar_mail::run_desktop(flectar_mail::PlatformContext::desktop()?)
}
