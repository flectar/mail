//! A metered connection on Android and HarmonyOS (#253), as the host reads it:
//! on Android ConnectivityManager's answer for the network in use, through
//! JNI, and on HarmonyOS Network Kit's for the default network. Either way it
//! is mobile data or a network the user marked as metered. The app frameworks
//! tell of changes only to callbacks registered from Java or ArkTS, so the
//! answer is read again every quarter minute.

use std::time::Duration;

const EVERY: Duration = Duration::from_secs(15);

/// Whether the host told how to ask; without it, no connection is metered.
pub(super) fn watch(
    changed: impl Fn(bool) + Send + Sync + 'static,
    runtime: &tokio::runtime::Handle,
) -> bool {
    if crate::platform::connection_metered().is_none() {
        return false;
    }
    runtime.spawn(async move {
        let mut tick = tokio::time::interval(EVERY);
        loop {
            tick.tick().await;
            if let Some(metered) = crate::platform::connection_metered() {
                changed(metered);
            }
        }
    });
    true
}
