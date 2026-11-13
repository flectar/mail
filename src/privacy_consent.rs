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
