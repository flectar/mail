//! Desktop Linux reports memory pressure through the
//! `org.freedesktop.portal.MemoryMonitor` portal, which forwards
//! low-memory-monitor or GLib's monitor on the host, inside Flatpak as well.

use super::{MemoryPressure, memory_pressure};
use futures_util::StreamExt;

pub(super) fn watch(runtime: &tokio::runtime::Handle) {
    runtime.spawn(async {
        // ashpd shares its session connection, so this adds no thread.
        let Ok(monitor) = ashpd::desktop::memory_monitor::MemoryMonitor::new().await else {
            return;
        };
        // ashpd 0.11 decodes the signal's byte as an i32 and so drops every
        // real signal. Read the signal through the portal's zbus proxy.
        let Ok(mut warnings) = monitor.receive_signal("LowMemoryWarning").await else {
            return;
        };
        while let Some(message) = warnings.next().await {
            if let Some(level) = message.body().deserialize::<u8>().ok().and_then(pressure) {
                memory_pressure(level);
            }
        }
    });
}

/// Levels follow low-memory-monitor: 50 asks apps to drop caches, 100 to try
/// harder, and 255 warns that processes are about to be killed.
fn pressure(level: u8) -> Option<MemoryPressure> {
    match level {
        0 => None,
        1..100 => Some(MemoryPressure::Moderate),
        _ => Some(MemoryPressure::Critical),
    }
}

#[cfg(test)]
mod tests {
    use super::{MemoryPressure, pressure};

    #[test]
    fn portal_levels_map_to_pressure() {
        assert_eq!(pressure(0), None);
        assert_eq!(pressure(50), Some(MemoryPressure::Moderate));
        assert_eq!(pressure(100), Some(MemoryPressure::Critical));
        assert_eq!(pressure(255), Some(MemoryPressure::Critical));
    }
}
