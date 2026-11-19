//! The operating system's proxy settings, which connections follow while
//! Settings, Network, says to use the system's. The core asks the reader
//! here, after the environment's proxy variables, and keeps its answers for
//! a minute.

#[cfg(target_vendor = "apple")]
mod apple;
#[cfg(any(test, target_vendor = "apple"))]
mod apple_settings;
#[cfg(linux_desktop)]
mod linux;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(any(test, target_os = "windows"))]
mod windows_settings;

use flectar_mail_core::net::proxy::SystemProxies;

/// Hand the platform's reader to the core. Later calls do nothing.
pub(crate) fn install() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        if let Some(reader) = reader() {
            flectar_mail_core::net::proxy::set_system_source(reader);
        }
    });
}

/// This platform's reader. Android's host brings its own
/// (`configure_system_proxy`), and OpenHarmony has none, so the environment
/// counts there.
fn reader() -> Option<Box<dyn SystemProxies>> {
    #[cfg(target_os = "windows")]
    let reader: Option<Box<dyn SystemProxies>> = Some(Box::new(windows::Reader::new()));
    #[cfg(target_vendor = "apple")]
    let reader: Option<Box<dyn SystemProxies>> = Some(Box::new(apple::Reader::new()));
    #[cfg(linux_desktop)]
    let reader: Option<Box<dyn SystemProxies>> = Some(Box::new(linux::Reader::new()));
    #[cfg(not(any(target_os = "windows", target_vendor = "apple", linux_desktop)))]
    let reader = None;
    reader
}
