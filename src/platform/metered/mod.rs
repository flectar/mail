//! Whether the device's connection costs by the byte (#253), as the system
//! tells: on Linux the desktop portal's network monitor, or NetworkManager.
//! Each watcher reports the answer at once and again whenever it changes.

#[cfg(linux_desktop)]
mod linux;

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
    #[cfg(not(linux_desktop))]
    {
        let _ = (changed, runtime);
        false
    }
}
