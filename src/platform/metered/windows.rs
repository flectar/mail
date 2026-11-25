//! A metered connection on Windows (#253): the cost of the connection to the
//! Internet, as Windows tells apps. A fixed or variable data plan, roaming,
//! or a plan over its limit counts as metered, as Microsoft's guidance for
//! apps that download in the background has it.

use windows::Networking::Connectivity::{
    NetworkCostType, NetworkInformation, NetworkStatusChangedEventHandler,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

pub(super) fn watch(changed: impl Fn(bool) + Send + Sync + 'static) {
    let started = std::thread::Builder::new()
        .name("metered".into())
        .spawn(move || {
            // SAFETY: called once on this new thread, before any other COM
            // use on it. The system calls the handler on its own threads.
            let joined = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if let Err(error) = joined.ok() {
                tracing::warn!(%error, "the connection's cost can't be read");
                return;
            }
            let changed = std::sync::Arc::new(changed);
            changed(metered());
            let on_change = std::sync::Arc::clone(&changed);
            let handler = NetworkStatusChangedEventHandler::new(move |_| {
                on_change(metered());
                Ok(())
            });
            if let Err(error) = NetworkInformation::NetworkStatusChanged(&handler) {
                tracing::warn!(%error, "changes to the connection's cost won't be followed");
                return;
            }
            // The thread stays in the apartment the handler was registered
            // from, for the life of the app.
            loop {
                std::thread::park();
            }
        });
    if let Err(error) = started {
        tracing::warn!(%error, "the connection's cost can't be watched");
    }
}

/// Whether the connection to the Internet is metered now. Without one,
/// there's nothing to download anyway.
fn metered() -> bool {
    let Ok(cost) = NetworkInformation::GetInternetConnectionProfile()
        .and_then(|profile| profile.GetConnectionCost())
    else {
        return false;
    };
    cost.NetworkCostType()
        .is_ok_and(|kind| kind == NetworkCostType::Fixed || kind == NetworkCostType::Variable)
        || cost.Roaming().unwrap_or(false)
        || cost.OverDataLimit().unwrap_or(false)
}
