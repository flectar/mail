//! New-mail notifications: the host publisher and the outbox dispatcher.
//!
//! The sync engine enqueues one durable row per chime-worthy arrival. The
//! dispatcher applies the user's Important/All/tabs scope, waits for pending AI
//! routing, and hands raw message fields to the host publisher. Hosts format
//! and translate the title and body themselves, so the shared layer never emits
//! user-visible English text.

use std::sync::Arc;

use flectar_mail_core::models::{Settings, now_ms};
use flectar_mail_core::{NotificationOutboxItem, RoutedTab};

use super::CoreMailSource;

/// One new-mail event handed to a host publisher.
///
/// The fields are raw database values: hosts build the translated title and
/// body, choose the notification slot, and attach the thread or message id to a
/// `wantAgent` or the platform's equivalent action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMailNotification {
    pub account_id: i64,
    pub message_id: i64,
    pub thread_id: Option<i64>,
    pub sender: String,
    pub subject: String,
}

impl NewMailNotification {
    fn from_outbox(item: &NotificationOutboxItem) -> Self {
        let sender = item
            .sender_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| item.sender_addr.clone())
            .unwrap_or_default();
        Self {
            account_id: item.account_id,
            message_id: item.message_id,
            thread_id: item.thread_id,
            sender,
            subject: item.subject.clone(),
        }
    }
}

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

    fn publish(&self, notification: &NewMailNotification) {
        if let Some(handler) = &self.handler {
            handler(notification);
        }
    }

    fn is_enabled(&self) -> bool {
        self.handler.is_some()
    }
}

/// Apply the user's notification scope to a resolved tab key.
fn scope_allows(settings: &Settings, tab: &str) -> bool {
    match settings.notification_scope.as_str() {
        "all" => true,
        "tabs" => settings
            .notification_tabs
            .iter()
            .any(|selected| selected == tab),
        // "important" and any future or unknown value keep the historical
        // Important-only behaviour rather than notifying for every message.
        _ => tab == "important",
    }
}

/// Drain the notification outbox for a host that can raise notifications.
///
/// A pending AI route is deferred and retried on the next dispatch; a message
/// outside the selected scope is suppressed permanently. The publisher is
/// called only after the row is claimed, and the row is marked delivered once
/// the host callback returns.
pub(crate) async fn dispatch_notifications(
    core: &CoreMailSource,
    publisher: &NotificationPublisher,
) {
    if !publisher.is_enabled() {
        return;
    }
    let Ok(settings) = core.load_settings().await else {
        return;
    };
    if !settings.notifications_enabled {
        return;
    }
    let file_core = core.file_core();
    let Ok(items) = file_core.due_notifications(16).await else {
        return;
    };
    for item in items {
        let Some(thread_id) = item.thread_id else {
            let _ = file_core
                .suppress_notification_delivery(item.id, "thread was deleted")
                .await;
            continue;
        };
        let tab = match file_core.notification_thread_tab(thread_id).await {
            Ok(Some(RoutedTab::Resolved(tab))) => tab,
            Ok(Some(RoutedTab::Pending)) => {
                let _ = file_core
                    .defer_notification_delivery(item.id, now_ms() + 60_000)
                    .await;
                continue;
            }
            Ok(None) => {
                let _ = file_core
                    .suppress_notification_delivery(item.id, "thread was deleted")
                    .await;
                continue;
            }
            Err(_) => {
                let _ = file_core
                    .retry_notification_delivery(item.id, 60_000, "thread tab lookup failed")
                    .await;
                continue;
            }
        };
        if !scope_allows(&settings, &tab) {
            let _ = file_core
                .suppress_notification_delivery(item.id, "outside notification scope")
                .await;
            continue;
        }
        if !file_core
            .claim_notification_delivery(item.id)
            .await
            .unwrap_or(false)
        {
            continue;
        }
        publisher.publish(&NewMailNotification::from_outbox(&item));
        let _ = file_core.mark_notification_delivered(item.id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn important_scope_only_notifies_important() {
        let settings = Settings::default();
        assert!(scope_allows(&settings, "important"));
        assert!(!scope_allows(&settings, "other"));
        assert!(!scope_allows(&settings, "split:1"));
    }

    #[test]
    fn all_scope_notifies_every_tab() {
        let settings = Settings {
            notification_scope: "all".into(),
            ..Settings::default()
        };
        assert!(scope_allows(&settings, "important"));
        assert!(scope_allows(&settings, "other"));
        assert!(scope_allows(&settings, "label:7"));
    }

    #[test]
    fn tabs_scope_honours_the_selected_tabs() {
        let settings = Settings {
            notification_scope: "tabs".into(),
            notification_tabs: vec!["other".into(), "label:7".into()],
            ..Settings::default()
        };
        assert!(!scope_allows(&settings, "important"));
        assert!(scope_allows(&settings, "other"));
        assert!(scope_allows(&settings, "label:7"));
        assert!(!scope_allows(&settings, "split:1"));
    }
}
