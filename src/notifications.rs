//! New-mail notifications: the host publisher and the event helper.
//!
//! The core emits `CoreEvent::MailNew` for chime-worthy arrivals. Hosts that
//! can raise system notifications install a publisher on the
//! `PlatformContext` (the OpenHarmony shell publishes through the system
//! notification service); without one — or with notifications turned off —
//! everything here is a no-op.

use std::sync::Arc;

use super::CoreMailSource;

/// Callback a host installs to publish system notifications.
type PublishHandler = Arc<dyn Fn(&str, &str) + Send + Sync + 'static>;

/// Host publisher for system notifications. The default does nothing, so a
/// host without a notification service can leave it untouched.
#[derive(Clone, Default)]
pub struct NotificationPublisher {
    handler: Option<PublishHandler>,
}

impl NotificationPublisher {
    /// Publish a notification; the host marshals the call onto whatever
    /// thread its native API requires and returns promptly.
    pub fn new(publish: impl Fn(&str, &str) + Send + Sync + 'static) -> Self {
        Self {
            handler: Some(Arc::new(publish)),
        }
    }

    fn publish(&self, title: &str, body: &str) {
        if let Some(handler) = &self.handler {
            handler(title, body);
        }
    }
}

/// Publish a system notification for a chime-worthy arrival.
pub(crate) async fn notify_new_mail(
    core: &CoreMailSource,
    thread_ids: &[i64],
    publisher: &NotificationPublisher,
) {
    if publisher.handler.is_none() {
        return;
    }
    let Ok(settings) = core.load_settings().await else {
        return;
    };
    if !settings.notifications_enabled {
        return;
    }
    let Some(thread_id) = thread_ids.first().copied() else {
        return;
    };
    let Ok(outline) = core.file_core().get_thread_outline(thread_id).await else {
        return;
    };
    let summary = outline.thread;
    // The sender is the most useful conversation opener; fall back to the
    // account when every participant is one of our own addresses.
    let sender = summary
        .participants
        .iter()
        .find(|address| !address.email.eq_ignore_ascii_case(&summary.account_email))
        .or_else(|| summary.participants.first())
        .map(|address| {
            address
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| address.email.clone())
        })
        .unwrap_or_else(|| summary.account_email.clone());
    let subject = if summary.subject.trim().is_empty() {
        "(no subject)".to_owned()
    } else {
        summary.subject.clone()
    };
    let title = if thread_ids.len() > 1 {
        format!(
            "{} — {} new messages",
            summary.account_email,
            thread_ids.len()
        )
    } else {
        sender
    };
    publisher.publish(&title, &subject);
}
