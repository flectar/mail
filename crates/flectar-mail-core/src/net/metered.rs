//! Whether this device's connection costs by the byte, as its platform
//! tells, and whether downloading ahead of time waits while it does (#253).
//! Mail still arrives on a metered connection, as checking for it downloads
//! headers alone, and messages and attachments download when they're
//! opened.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

/// How the platform tells whether the connection is metered: such as a
/// phone's mobile data or a hotspot, or one the user marked so.
pub type Probe = Arc<dyn Fn() -> bool + Send + Sync>;

/// The device's metered connection and its choice for one.
#[derive(Default)]
pub struct Metered {
    probe: RwLock<Option<Probe>>,
    /// Downloading ahead of time waits on a metered connection.
    pause: AtomicBool,
}

impl Metered {
    /// The app tells how to find out, once it can. Without a probe no
    /// connection is metered.
    pub fn set_probe(&self, probe: Probe) {
        *self.probe.write().unwrap_or_else(PoisonError::into_inner) = Some(probe);
    }

    /// Whether the connection is metered, as far as the platform tells.
    pub fn is_metered(&self) -> bool {
        let probe = self
            .probe
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        probe.is_some_and(|probe| probe())
    }

    /// Choose whether downloading ahead of time waits on a metered
    /// connection. Whether the choice changed.
    pub(crate) fn set_pause(&self, pause: bool) -> bool {
        self.pause.swap(pause, Ordering::Relaxed) != pause
    }

    /// Whether downloading ahead of time waits now.
    pub fn downloads_wait(&self) -> bool {
        self.pause.load(Ordering::Relaxed) && self.is_metered()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// Downloading waits only while the connection is metered and the
    /// device chose so, and a device without a probe has no metered
    /// connection.
    #[test]
    fn downloads_wait_on_a_metered_connection_when_chosen() {
        let metered = Metered::default();
        assert!(!metered.is_metered());
        assert!(metered.set_pause(true));
        assert!(!metered.set_pause(true));
        assert!(!metered.downloads_wait());

        let costly = Arc::new(AtomicBool::new(true));
        let probe = costly.clone();
        metered.set_probe(Arc::new(move || probe.load(Ordering::Relaxed)));
        assert!(metered.downloads_wait());
        costly.store(false, Ordering::Relaxed);
        assert!(!metered.downloads_wait());
        costly.store(true, Ordering::Relaxed);
        assert!(metered.set_pause(false));
        assert!(!metered.downloads_wait());
    }
}
