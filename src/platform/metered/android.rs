//! A metered connection on Android (#253): ConnectivityManager's answer for
//! the network in use, mobile data or one the user marked as metered, which
//! the host reads through JNI. Android tells of changes only to callbacks
//! registered from Java, so the answer is read again every quarter minute.

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
