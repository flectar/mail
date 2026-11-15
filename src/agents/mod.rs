//! AI agents using mail through the Model Context Protocol (#237).
//!
//! `flectar-mail mcp` is a bridge that an agent's app starts. It speaks MCP
//! over stdio and forwards each tool call to the running Flectar Mail over a
//! local endpoint only this user can open, which the app opens only while
//! agent access is on in Settings. So there is still one sync engine and one
//! database writer, and the app alone decides what each call may see.

mod bridge;
pub(crate) mod endpoint;
pub(crate) mod protocol;
pub(crate) mod server;
mod settings;
pub(crate) mod tools;

pub use bridge::run_bridge_if_requested;
pub(crate) use settings::register as register_settings;

use flectar_mail_core::Core;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::runtime::Handle;

/// What it takes to open the endpoint, once the core is ready, and the
/// server while it's open.
struct Lifecycle {
    runtime: Handle,
    data_dir: PathBuf,
    core: Arc<Core>,
    server: Option<server::AgentServer>,
}

static LIFECYCLE: Mutex<Option<Lifecycle>> = Mutex::new(None);

/// Starts answering agents once the core is ready, if access is on.
pub(crate) fn core_ready(runtime: &Handle, data_dir: &Path, core: Arc<Core>) {
    let settings_core = Arc::clone(&core);
    if let Ok(mut lifecycle) = LIFECYCLE.lock() {
        *lifecycle = Some(Lifecycle {
            runtime: runtime.clone(),
            data_dir: data_dir.to_owned(),
            core,
            server: None,
        });
    }
    runtime.spawn(async move {
        if let Ok(settings) = settings_core.get_settings().await {
            set_enabled(settings.agent_access.enabled);
        }
    });
}

/// Opens or closes the endpoint to match the access setting. The endpoint
/// exists only while access is on.
pub(crate) fn set_enabled(enabled: bool) {
    let Ok(mut lifecycle) = LIFECYCLE.lock() else {
        return;
    };
    let Some(lifecycle) = lifecycle.as_mut() else {
        return;
    };
    match (enabled, lifecycle.server.is_some()) {
        (true, false) => match server::AgentServer::start(
            &lifecycle.runtime,
            &lifecycle.data_dir,
            Arc::clone(&lifecycle.core),
        ) {
            Ok(server) => lifecycle.server = Some(server),
            Err(error) => eprintln!("Agent access couldn't open its connection: {error}"),
        },
        (false, true) => lifecycle.server = None,
        _ => {}
    }
}

/// Closes the endpoint as the app quits.
pub(crate) fn shutdown() {
    if let Ok(mut lifecycle) = LIFECYCLE.lock() {
        *lifecycle = None;
    }
}

type ActivityListener = Box<dyn Fn() + Send>;

static ACTIVITY_LISTENER: Mutex<Option<ActivityListener>> = Mutex::new(None);

/// Calls `listener`, on the thread that recorded it, whenever an agent's
/// request lands in the activity log.
pub(crate) fn on_activity(listener: impl Fn() + Send + 'static) {
    if let Ok(mut slot) = ACTIVITY_LISTENER.lock() {
        *slot = Some(Box::new(listener));
    }
}

fn activity_recorded() {
    if let Ok(slot) = ACTIVITY_LISTENER.lock()
        && let Some(listener) = slot.as_ref()
    {
        listener();
    }
}
