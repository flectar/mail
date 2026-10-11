//! Core events broadcast to the native host and forwarded to the Slint UI.

use serde::Serialize;
use tokio::sync::{broadcast, watch};

use crate::models::SyncStatus;

/// Opt-in receive diagnostics. Callers supply fixed stages/error codes only;
/// addresses, server responses, credentials and message content stay out.
pub(crate) fn sync_diagnostic(stage: &'static str, account_id: Option<i64>, outcome: &str) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !ENABLED.get_or_init(|| std::env::var("FLECTAR_SYNC_DIAGNOSTICS").as_deref() == Ok("1")) {
        return;
    }
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    eprintln!(
        "FLECTAR_SYNC_DIAGNOSTIC {}",
        serde_json::json!({
            "elapsed_ms": START.get_or_init(std::time::Instant::now).elapsed().as_millis(),
            "stage": stage, "account_id": account_id, "outcome": outcome,
        })
    );
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub account_id: i64,
    pub folder: String,
    pub phase: String, // folders|headers|bodies|history|idle
    pub done: u64,
    pub total: u64,
}

/// Minimal description of an event the pull just discovered, enough for a
/// "new event" desktop notification without shipping the whole row.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewEventInfo {
    pub summary: Option<String>,
    pub starts_at: i64,
    pub all_day: bool,
}

#[derive(Debug, Clone)]
pub enum CoreEvent {
    SyncProgress(SyncProgress),
    SyncStatus(SyncStatus),
    MailNew {
        account_id: i64,
        thread_ids: Vec<i64>,
    },
    MailUpdated {
        thread_ids: Vec<i64>,
    },
    ActionState {
        action_id: i64,
        state: String,
        error: Option<String>,
    },
    ThreadWoke {
        thread_id: i64,
    },
    NetworkState {
        online: bool,
    },
    AccountState {
        account_id: i64,
        sync_state: String,
        sync_error: Option<String>,
    },
    /// RAG "ask" retrieved its source messages (emitted before the answer
    /// streams, so the UI can show sources immediately).
    AskCitations {
        request_id: String,
        citations: Vec<crate::models::AskCitation>,
    },
    /// One incremental chunk of the streamed "ask" answer.
    AskDelta {
        request_id: String,
        delta: String,
    },
    /// One incremental chunk of the model's reasoning (shown clipped in the UI).
    AskReasoning {
        request_id: String,
        delta: String,
    },
    /// The streamed "ask" answer finished.
    AskDone {
        request_id: String,
    },
    /// One incremental raw JSON chunk of a structured thread summary.
    AiSummaryDelta {
        thread_id: i64,
        delta: String,
    },
    /// Calendar data changed (CalDAV pull or local mutation synced).
    CalendarUpdated {
        account_id: i64,
    },
    /// New events the incremental CalDAV pull discovered (not initial backfill),
    /// for a "new event" notification.
    CalendarEventsAdded {
        account_id: i64,
        events: Vec<NewEventInfo>,
    },
    /// A meeting starts within the reminder window.
    EventReminder {
        event: Box<crate::models::CalendarEvent>,
        occurrence_start: i64,
    },
    /// A local edit lost against the server (kept as a conflict copy).
    CalendarConflict {
        event_id: i64,
        summary: Option<String>,
    },
    /// CardDAV pull or push changed the unified contact directory.
    ContactsUpdated {
        account_id: i64,
    },
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<CoreEvent>,
    // Retained per-Core state: workers created after a native suspend must
    // observe it too. Desktop hosts leave the default active value unchanged.
    platform_active: watch::Sender<bool>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        EventBus {
            tx,
            platform_active: watch::channel(true).0,
        }
    }

    pub fn emit(&self, ev: CoreEvent) {
        match &ev {
            CoreEvent::AccountState {
                account_id,
                sync_state,
                ..
            } => {
                sync_diagnostic("account_state", Some(*account_id), sync_state);
            }
            CoreEvent::MailUpdated { .. } => sync_diagnostic("mail_updated", None, "emitted"),
            _ => {}
        }
        // Nobody listening is fine (e.g. during tests).
        let _ = self.tx.send(ev);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.tx.subscribe()
    }

    pub(crate) fn set_platform_active(&self, active: bool) {
        self.platform_active.send_replace(active);
    }

    pub(crate) fn platform_is_active(&self) -> bool {
        *self.platform_active.borrow()
    }

    pub(crate) fn platform_activity(&self) -> watch::Receiver<bool> {
        self.platform_active.subscribe()
    }

    pub(crate) async fn wait_for_platform_active(&self) {
        let mut activity = self.platform_activity();
        // wait_for checks the retained value before sleeping, so resume cannot
        // be lost between this check and installing the waiter.
        let _ = activity.wait_for(|active| *active).await;
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod platform_activity_tests {
    use super::EventBus;

    #[tokio::test]
    async fn retained_suspend_blocks_late_workers_and_resume_is_not_lost() {
        let bus = EventBus::new();
        let separate_core = EventBus::new();
        bus.set_platform_active(false);
        assert!(separate_core.platform_is_active());
        let late_worker = bus.clone();
        let mut worker = tokio::spawn(async move {
            late_worker.wait_for_platform_active().await;
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut worker)
                .await
                .is_err()
        );
        bus.set_platform_active(true);
        tokio::time::timeout(std::time::Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
        // A resume before subscribing must also let new workers proceed.
        bus.wait_for_platform_active().await;
    }
}
