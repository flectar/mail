//! Host hook for keeping a native app language aligned with the shared UI.
//!
//! The service only stores the last selected pair and forwards it to the host.
//! A host that installs its native callback later replays the stored value;
//! the shared crate keeps no process-wide handler.

use std::sync::{Arc, Mutex};

type SetHandler = Arc<dyn Fn(&str, &str) + Send + Sync + 'static>;

/// Host language sink carried by [`crate::PlatformContext`].
#[derive(Clone, Default)]
pub struct AppLanguage {
    set: Option<SetHandler>,
    last: Arc<Mutex<Option<(String, String)>>>,
}

impl AppLanguage {
    /// Install the host callback. It receives the preference mode ("system"
    /// or an explicit language) and the selected bundled language.
    pub fn new(set: impl Fn(&str, &str) + Send + Sync + 'static) -> Self {
        Self {
            set: Some(Arc::new(set)),
            last: Arc::new(Mutex::new(None)),
        }
    }

    /// Remember the selection and forward it when the host is available.
    pub fn set(&self, mode: &str, selected: &str) {
        if let Ok(mut last) = self.last.lock() {
            *last = Some((mode.to_owned(), selected.to_owned()));
        }
        if let Some(set) = &self.set {
            set(mode, selected);
        }
    }

    /// Re-send the selection after a native callback becomes available.
    pub fn replay(&self) {
        let Some(set) = &self.set else {
            return;
        };
        if let Ok(last) = self.last.lock()
            && let Some((mode, selected)) = last.as_ref()
        {
            set(mode, selected);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AppLanguage;
    use std::sync::{Arc, Mutex};

    #[test]
    fn replay_forwards_the_last_selection_once_the_host_is_ready() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let language = AppLanguage::new(move |mode, selected| {
            log.lock().unwrap().push((mode.to_owned(), selected.to_owned()));
        });

        language.set("system", "en");
        language.replay();

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                ("system".to_owned(), "en".to_owned()),
                ("system".to_owned(), "en".to_owned())
            ]
        );
    }
}
