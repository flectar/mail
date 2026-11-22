//! Whether the device's connection costs by the byte (#253), as the system
//! tells: Windows from the connection's data plan, macOS and iOS from an
//! expensive path or Low Data Mode, Android from its connectivity manager,
//! and Linux from the desktop portal's network monitor or NetworkManager.
//! Each watcher reports the answer at once and again whenever it changes.

#[cfg(target_os = "android")]
mod android;
#[cfg(target_vendor = "apple")]
mod apple;
#[cfg(linux_desktop)]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

/// Start watching, reporting each answer to `changed`. Whether this
/// platform can tell; where it can't, no connection counts as metered.
pub(crate) fn watch(
    changed: impl Fn(bool) + Send + Sync + 'static,
    runtime: &tokio::runtime::Handle,
) -> bool {
    #[cfg(linux_desktop)]
    {
        linux::watch(changed, runtime);
        true
    }
    #[cfg(target_os = "windows")]
    {
        let _ = runtime;
        windows::watch(changed);
        true
    }
    #[cfg(target_vendor = "apple")]
    {
        let _ = runtime;
        apple::watch(changed);
        true
    }
    #[cfg(target_os = "android")]
    {
        android::watch(changed, runtime)
    }
    #[cfg(not(any(
        linux_desktop,
        target_os = "windows",
        target_vendor = "apple",
        target_os = "android"
    )))]
    {
        let _ = (changed, runtime);
        false
    }
}
