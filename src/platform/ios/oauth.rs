//! Gmail and Outlook sign-in with ASWebAuthenticationSession
//! (platform/ios/WebAuthentication.m).
//!
//! Both providers need iOS registrations, bundled with
//! `FLECTAR_GOOGLE_IOS_CLIENT_ID` and `FLECTAR_MICROSOFT_IOS_CLIENT_ID`
//! (platform/ios/README.md): Google's iOS clients redirect to their reversed
//! client ID, and Microsoft's iOS platform registration to
//! `msauth.com.flectar.mail://auth`.

use super::super::web_authentication::{Outcome, SessionPresenter, WebAuthentication};
use std::{
    ffi::{CStr, CString, c_char},
    sync::OnceLock,
};

unsafe extern "C" {
    fn flectar_start_web_authentication(
        id: i64,
        url: *const c_char,
        scheme: *const c_char,
        finished: extern "C" fn(i64, *const c_char, *const c_char, bool),
    );
    fn flectar_cancel_web_authentication(id: i64);
}

/// The sessions of WebAuthentication.m.
pub(super) struct NativeSessions;

impl SessionPresenter for NativeSessions {
    fn start(&self, id: i64, url: &str, scheme: &str) {
        let (Ok(url), Ok(scheme)) = (CString::new(url), CString::new(scheme)) else {
            broker().finished(id, Outcome::Failed("the sign-in address is invalid".into()));
            return;
        };
        // SAFETY: WebAuthentication.m copies both strings before returning and
        // calls `finished` once for this id.
        unsafe { flectar_start_web_authentication(id, url.as_ptr(), scheme.as_ptr(), finished) }
    }

    fn cancel(&self, id: i64) {
        // SAFETY: this only dispatches to the main queue; an id without a
        // session is ignored.
        unsafe { flectar_cancel_web_authentication(id) }
    }
}

/// The app's sign-in state, which the native callback reports to.
pub(super) fn broker() -> WebAuthentication<NativeSessions> {
    static BROKER: OnceLock<WebAuthentication<NativeSessions>> = OnceLock::new();
    BROKER
        .get_or_init(|| WebAuthentication::new(NativeSessions))
        .clone()
}

/// A string the native bridge keeps alive for the callback.
fn string(pointer: *const c_char) -> Option<String> {
    // SAFETY: WebAuthentication.m passes NULL or a NUL-terminated string that
    // lives until this callback returns.
    (!pointer.is_null()).then(|| {
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    })
}

/// WebAuthentication.m reports how session `id` ended: the URL the provider
/// redirected to, or an error and whether the user dismissed the sheet.
extern "C" fn finished(id: i64, redirect: *const c_char, error: *const c_char, cancelled: bool) {
    let outcome = match (string(redirect), cancelled) {
        (Some(redirect), _) => Outcome::Redirected(redirect),
        (None, true) => Outcome::Cancelled,
        (None, false) => Outcome::Failed(string(error).unwrap_or_default()),
    };
    broker().finished(id, outcome);
}
