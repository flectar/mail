//! Host withdrawal of a privacy consent stored outside the shared app state.
//!
//! Desktop builds have no consent gate, so the default service is unsupported.
//! Hosts that gate first use on a persisted consent install a callback that
//! clears it and stops the app.

use std::sync::Arc;

type WithdrawHandler = Arc<dyn Fn() -> Result<(), String> + Send + Sync + 'static>;

/// Host withdrawal hook carried by [`crate::PlatformContext`].
#[derive(Clone, Default)]
pub struct PrivacyConsent {
    withdraw: Option<WithdrawHandler>,
}

impl PrivacyConsent {
    /// Install the host callback used by Settings -> About.
    pub fn new(withdraw: impl Fn() -> Result<(), String> + Send + Sync + 'static) -> Self {
        Self {
            withdraw: Some(Arc::new(withdraw)),
        }
    }

    /// Whether this host exposes a consent that can be withdrawn.
    pub fn is_supported(&self) -> bool {
        self.withdraw.is_some()
    }

    /// Clear the host consent. The host is responsible for stopping processing.
    pub fn withdraw(&self) -> Result<(), String> {
        match &self.withdraw {
            Some(withdraw) => withdraw(),
            None => Err("This build does not use a withdrawable privacy consent.".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PrivacyConsent;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn hosts_without_a_stored_consent_offer_no_withdrawal() {
        let consent = PrivacyConsent::default();
        assert!(!consent.is_supported());
        assert!(consent.withdraw().is_err());
    }

    #[test]
    fn withdrawing_runs_the_host_hook_and_returns_its_error() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let consent = PrivacyConsent::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert!(consent.is_supported());
        consent.withdraw().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let failing = PrivacyConsent::new(|| Err("the preference could not be cleared".into()));
        assert_eq!(
            failing.withdraw().unwrap_err(),
            "the preference could not be cleared"
        );
    }
}
