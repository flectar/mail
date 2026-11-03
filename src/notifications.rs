//! New-mail notifications raised by the host.
//!
//! Core keeps a durable outbox of new mail and decides what to notify for: the
//! user's Important/All/tabs scope, mail that is still new and unread, and
//! threads whose AI route has resolved. The host only raises each
//! notification, formatting and translating the title and body itself, so the
//! shared layer never emits user-visible English text.

use std::{sync::Arc, time::Duration};

use flectar_mail_core::{Core, events::CoreEvent, models::now_ms};
use tokio::sync::broadcast::{self, error::RecvError};

pub use flectar_mail_core::NewMailNotification;

type PublishHandler = Arc<dyn Fn(&NewMailNotification) + Send + Sync + 'static>;

/// Host publisher for system notifications. The default does nothing, so a
/// host without a notification service can leave it untouched.
#[derive(Clone, Default)]
pub struct NotificationPublisher {
    handler: Option<PublishHandler>,
}

impl NotificationPublisher {
    /// Publish a notification; the host marshals the call onto whatever
    /// thread its native API requires and returns promptly.
    pub fn new(publish: impl Fn(&NewMailNotification) + Send + Sync + 'static) -> Self {
        Self {
            handler: Some(Arc::new(publish)),
        }
    }
}

/// How long to wait before trying again after a dispatch failed.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Raise new-mail notifications through the host publisher: once at startup,
/// after new mail arrives, and when a postponed notification falls due. A host
/// without a publisher starts nothing.
pub(crate) fn spawn_dispatcher(
    runtime: &tokio::runtime::Runtime,
    core: Arc<Core>,
    publisher: NotificationPublisher,
) {
    let Some(publish) = publisher.handler else {
        return;
    };
    let mut events = core.bus.subscribe();
    runtime.spawn(async move {
        if let Err(error) = core.recover_notification_deliveries().await {
            tracing::warn!(%error, "notifications: could not recover interrupted deliveries");
        }
        loop {
            let wait = match core.dispatch_notifications(&*publish).await {
                Ok(next) => {
                    next.map(|at| Duration::from_millis(u64::try_from(at - now_ms()).unwrap_or(0)))
                }
                Err(error) => {
                    tracing::warn!(%error, "notifications: dispatch failed");
                    Some(RETRY_AFTER)
                }
            };
            tokio::select! {
                running = new_mail(&mut events) => {
                    if !running {
                        return;
                    }
                }
                () = sleep(wait) => {}
            }
        }
    });
}

/// Wait for new mail. A receiver that lagged may have missed some, so that
/// counts as new mail too. Returns false once core shuts down.
async fn new_mail(events: &mut broadcast::Receiver<CoreEvent>) -> bool {
    loop {
        match events.recv().await {
            Ok(CoreEvent::MailNew { .. }) | Err(RecvError::Lagged(_)) => return true,
            Ok(_) => {}
            Err(RecvError::Closed) => return false,
        }
    }
}

async fn sleep(wait: Option<Duration>) {
    match wait {
        Some(wait) => tokio::time::sleep(wait).await,
        None => std::future::pending().await,
    }
}
