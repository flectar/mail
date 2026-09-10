#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(target_os = "ios"))]
    if let Some(code) = flectar_mail::pdf_preview::run_worker_if_requested() {
        std::process::exit(code);
    }
    flectar_mail::run(flectar_mail::PlatformContext::desktop()?)
}
