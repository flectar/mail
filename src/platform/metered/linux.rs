//! A metered connection on Linux (#253): the desktop portal's network
//! monitor, which Flatpak allows and GNOME and KDE serve, or else
//! NetworkManager's own answer on the system bus.

use futures_util::StreamExt;
use std::time::Duration;

const PORTAL_TIMEOUT: Duration = Duration::from_secs(3);
/// NetworkManager's `NMMetered`: yes, or a guess of yes, which it gives for
/// a phone's hotspot. 0 is unknown, 2 no, and 4 a guess of no.
const METERED: [u32; 2] = [1, 3];

pub(super) fn watch(
    changed: impl Fn(bool) + Send + Sync + 'static,
    runtime: &tokio::runtime::Handle,
) {
    runtime.spawn(async move {
        if !portal(&changed).await {
            network_manager(&changed).await;
        }
    });
}

/// Follow the portal's answer. Whether the portal answered at all.
async fn portal(changed: &impl Fn(bool)) -> bool {
    let Ok(Ok(monitor)) = tokio::time::timeout(
        PORTAL_TIMEOUT,
        ashpd::desktop::network_monitor::NetworkMonitor::new(),
    )
    .await
    else {
        return false;
    };
    let Ok(Ok(metered)) = tokio::time::timeout(PORTAL_TIMEOUT, monitor.is_metered()).await else {
        return false;
    };
    changed(metered);
    let Ok(mut changes) = monitor.receive_changed().await else {
        return true;
    };
    while changes.next().await.is_some() {
        if let Ok(Ok(metered)) = tokio::time::timeout(PORTAL_TIMEOUT, monitor.is_metered()).await {
            changed(metered);
        }
    }
    true
}

/// Follow NetworkManager's answer, where there's no portal.
async fn network_manager(changed: &impl Fn(bool)) {
    let Ok(Ok(connection)) = tokio::time::timeout(PORTAL_TIMEOUT, zbus::Connection::system()).await
    else {
        return;
    };
    let Ok(proxy) = zbus::Proxy::new(
        &connection,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
    )
    .await
    else {
        return;
    };
    let mut changes = proxy.receive_property_changed::<u32>("Metered").await;
    let Ok(Ok(metered)) =
        tokio::time::timeout(PORTAL_TIMEOUT, proxy.get_property::<u32>("Metered")).await
    else {
        return;
    };
    changed(METERED.contains(&metered));
    while let Some(change) = changes.next().await {
        if let Ok(metered) = change.get().await {
            changed(METERED.contains(&metered));
        }
    }
}
