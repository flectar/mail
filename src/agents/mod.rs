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

type ReviewListener = Box<dyn Fn(i64, String) + Send>;

/// Drafts agents asked to send, which open for the user to check.
struct Reviews {
    /// Opens a draft for the user, given its id and the agent's name.
    listener: Mutex<Option<ReviewListener>>,
    /// The draft the user is checking. One at a time, so an agent can't bury
    /// the user in windows.
    in_review: Mutex<Option<i64>>,
}

static REVIEWS: Reviews = Reviews::new();

/// Why a draft an agent asked to send didn't open.
#[derive(Debug, PartialEq, Eq)]
enum ReviewRefused {
    /// The user is checking this other draft.
    Checking(i64),
    /// Nothing in the app can show it.
    NoComposer,
}

impl Reviews {
    const fn new() -> Self {
        Self {
            listener: Mutex::new(None),
            in_review: Mutex::new(None),
        }
    }

    fn listen(&self, listener: ReviewListener) {
        if let Ok(mut slot) = self.listener.lock() {
            *slot = Some(listener);
        }
    }

    /// Opens draft `draft_id` for the user to check and send, unless
    /// they're checking another one.
    fn request(&self, draft_id: i64, agent: String) -> Result<(), ReviewRefused> {
        let Ok(mut in_review) = self.in_review.lock() else {
            return Err(ReviewRefused::NoComposer);
        };
        match *in_review {
            Some(other) if other != draft_id => return Err(ReviewRefused::Checking(other)),
            _ => *in_review = Some(draft_id),
        }
        drop(in_review);
        let listener = self.listener.lock();
        if let Ok(slot) = &listener
            && let Some(listener) = slot.as_ref()
        {
            listener(draft_id, agent);
            Ok(())
        } else {
            self.ended(draft_id);
            Err(ReviewRefused::NoComposer)
        }
    }

    fn ended(&self, draft_id: i64) {
        if let Ok(mut in_review) = self.in_review.lock()
            && *in_review == Some(draft_id)
        {
            *in_review = None;
        }
    }
}

/// Calls `listener` with a draft and the agent's name when an agent asks to
/// send that draft. It opens the draft for the user to check, and calls
/// [`review_ended`] once the user sent or closed it, or when it couldn't open.
pub(crate) fn on_send_review(listener: impl Fn(i64, String) + Send + 'static) {
    REVIEWS.listen(Box::new(listener));
}

fn request_review(draft_id: i64, agent: String) -> Result<(), ReviewRefused> {
    REVIEWS.request(draft_id, agent)
}

/// The user sent or closed draft `draft_id`, which an agent asked to send.
pub(crate) fn review_ended(draft_id: i64) {
    REVIEWS.ended(draft_id);
}

fn activity_recorded() {
    if let Ok(slot) = ACTIVITY_LISTENER.lock()
        && let Some(listener) = slot.as_ref()
    {
        listener();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_checks_one_draft_an_agent_asks_to_send_at_a_time() {
        let reviews = Reviews::new();
        assert_eq!(
            reviews.request(7, "Claude".into()),
            Err(ReviewRefused::NoComposer),
            "nothing can show it yet"
        );
        let opened = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&opened);
        reviews.listen(Box::new(move |draft_id, agent| {
            seen.lock().unwrap().push((draft_id, agent));
        }));

        assert_eq!(reviews.request(7, "Claude".into()), Ok(()));
        // Asked again, it comes forward again.
        assert_eq!(reviews.request(7, "Claude".into()), Ok(()));
        assert_eq!(
            reviews.request(8, "Codex".into()),
            Err(ReviewRefused::Checking(7))
        );
        // Another draft's end leaves this one in review.
        reviews.ended(8);
        assert_eq!(
            reviews.request(8, "Codex".into()),
            Err(ReviewRefused::Checking(7))
        );
        reviews.ended(7);
        assert_eq!(reviews.request(8, "Codex".into()), Ok(()));
        assert_eq!(
            *opened.lock().unwrap(),
            [
                (7, "Claude".into()),
                (7, "Claude".into()),
                (8, "Codex".into())
            ]
        );
    }
}
