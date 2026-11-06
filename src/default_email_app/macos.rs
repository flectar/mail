//! macOS: Launch Services. Mail and other apps list every application that
//! declares the `mailto` scheme (platform/macos/Info.plist.in); changing the
//! default asks the user to confirm.

use super::{Applied, Done, State, Status};
use block2::RcBlock;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSBundle, NSError, NSURL, ns_string};
use std::{
    io,
    sync::{Mutex, PoisonError},
};

const BUNDLE_ID: &str = "com.flectar.mail";
/// `NSUserCancelledError`: the user kept the current app.
const USER_CANCELLED: isize = 3072;

/// Only the bundled application can be chosen; a build run straight from
/// Cargo has no bundle.
pub(super) fn supported() -> bool {
    NSBundle::mainBundle()
        .bundleIdentifier()
        .is_some_and(|identifier| identifier.to_string() == BUNDLE_ID)
}

pub(super) fn state() -> io::Result<State> {
    let link = NSURL::URLWithString(ns_string!("mailto:"))
        .ok_or_else(|| io::Error::other("mailto: is not a URL"))?;
    // Compare bundle identifiers, as Thunderbird does: another copy of
    // Flectar Mail is still Flectar Mail.
    let default = NSWorkspace::sharedWorkspace()
        .URLForApplicationToOpenURL(&link)
        .and_then(|application| NSBundle::bundleWithURL(&application))
        .and_then(|bundle| bundle.bundleIdentifier())
        .is_some_and(|identifier| identifier.to_string() == BUNDLE_ID);
    Ok(State {
        status: if default {
            Status::Default
        } else {
            Status::NotDefault
        },
        removable: false,
    })
}

pub(super) fn make_default(done: Done) {
    let bundle = NSBundle::mainBundle().bundleURL();
    // Gatekeeper runs a quarantined app from a randomized copy that
    // disappears when it quits; links must not point there.
    if bundle
        .path()
        .is_some_and(|path| path.to_string().contains("/AppTranslocation/"))
    {
        done(Err(io::Error::other(
            "move Flectar Mail to the Applications folder first",
        )));
        return;
    }
    let done = Mutex::new(Some(done));
    let finished = RcBlock::new(move |error: *mut NSError| {
        let Some(done) = done.lock().unwrap_or_else(PoisonError::into_inner).take() else {
            return;
        };
        // SAFETY: Launch Services passes null or an error that is valid for
        // the duration of the call.
        let result = match unsafe { error.as_ref() } {
            None => Ok(Applied::Done),
            Some(error) if error.code() == USER_CANCELLED => Ok(Applied::Unchanged),
            Some(error) => Err(io::Error::other(error.localizedDescription().to_string())),
        };
        done(result);
    });
    NSWorkspace::sharedWorkspace()
        .setDefaultApplicationAtURL_toOpenURLsWithScheme_completionHandler(
            &bundle,
            ns_string!("mailto"),
            Some(&finished),
        );
}

pub(super) fn remove() -> io::Result<()> {
    Ok(())
}
