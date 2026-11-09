//! Host withdrawal of a privacy consent stored outside the shared app state.
//!
//! Desktop builds have no consent gate, so the default service is unsupported.
//! Hosts that gate first use on a persisted consent (currently the OHOS shell)
//! install a callback that clears it and stops the app.

use std::sync::{Arc, Mutex, OnceLock};

type WithdrawHandler = Arc<dyn Fn() -> Result<(), String> + Send + Sync + 'static>;
type LanguageHandler = Arc<dyn Fn(&str) + Send + Sync + 'static>;
type ThemeHandler = Arc<dyn Fn(&str) + Send + Sync + 'static>;

static LANGUAGE_HANDLER: OnceLock<LanguageHandler> = OnceLock::new();
static LAST_LANGUAGE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static THEME_HANDLER: OnceLock<ThemeHandler> = OnceLock::new();
static LAST_THEME: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// Install the host hook that makes the native consent screen use the shared
/// theme on the next cold start.
pub fn install_theme_handler(handler: impl Fn(&str) + Send + Sync + 'static) {
    let _ = THEME_HANDLER.set(Arc::new(handler));
}

pub(crate) fn sync_consent_theme(theme_json: &str) {
    let last = LAST_THEME.get_or_init(|| Mutex::new(None));
    if let Ok(mut last) = last.lock() {
        *last = Some(theme_json.to_owned());
    }
    if let Some(handler) = THEME_HANDLER.get() {
        handler(theme_json);
    }
}

/// Flush a theme selected before the ArkTS bridge installed its callback.
pub fn sync_pending_theme() {
    let Some(handler) = THEME_HANDLER.get() else {
        return;
    };
    let Some(theme) = LAST_THEME
        .get()
        .and_then(|last| last.lock().ok().and_then(|last| last.clone()))
    else {
        return;
    };
    handler(&theme);
}

/// Install the host hook that makes the native app language follow the shared
/// UI language on the next cold start.
pub fn install_language_handler(handler: impl Fn(&str) + Send + Sync + 'static) {
    let _ = LANGUAGE_HANDLER.set(Arc::new(handler));
}

pub(crate) fn sync_app_language(language_mode: &str, selected: &str) {
    let language = if language_mode == "system" {
        "default"
    } else if selected == "zh_Hans" {
        "zh-Hans"
    } else {
        selected
    };
    let last = LAST_LANGUAGE.get_or_init(|| Mutex::new(None));
    if let Ok(mut last) = last.lock() {
        *last = Some(language.to_owned());
    }
    if let Some(handler) = LANGUAGE_HANDLER.get() {
        handler(language);
    }
}

/// Flush a language selected before the ArkTS bridge installed its callback.
pub fn sync_pending_language() {
    let Some(handler) = LANGUAGE_HANDLER.get() else {
        return;
    };
    let Some(language) = LAST_LANGUAGE
        .get()
        .and_then(|last| last.lock().ok().and_then(|last| last.clone()))
    else {
        return;
    };
    handler(&language);
}

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
