//! Durable mail notifications use the same outbox and routing policy as sync.
use super::{core, notification_can_dispatch, request};
use core::{Core, RoutedTab};
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Deliver,
    Defer,
    Suppress(&'static str),
}

fn decision(enabled: bool, scope: &str, tabs: &[String], route: Option<&RoutedTab>) -> Decision {
    if !enabled {
        return Decision::Suppress("notifications disabled");
    }
    // All includes automated mail and does not wait for AI classification.
    if scope == "all" {
        return Decision::Deliver;
    }
    match route {
        Some(RoutedTab::Pending) => Decision::Defer,
        Some(RoutedTab::Resolved(tab))
            if (scope == "tabs" && tabs.contains(tab))
                || (scope != "tabs" && tab == "important") =>
        {
            Decision::Deliver
        }
        Some(RoutedTab::Resolved(_)) => Decision::Suppress("outside notification scope"),
        None => Decision::Suppress("notification thread unavailable"),
    }
}

fn retry_delay_ms(attempts: i64) -> i64 {
    (5_000_i64 << attempts.clamp(0, 6)).min(300_000)
}

pub(super) async fn drain_due(core: &Arc<Core>) -> Result<usize, String> {
    if !notification_can_dispatch(core) {
        return Ok(0);
    }
    let settings = core.get_settings().await.map_err(|e| e.to_string())?;
    let pending = core
        .due_notifications(50)
        .await
        .map_err(|e| e.to_string())?;
    let mut delivered = 0;
    for item in pending {
        // The system may stop a scheduled job while a previous OS call completes.
        // Leave remaining rows pending for the next foreground/scheduled window.
        if !notification_can_dispatch(core) {
            break;
        }
        let route = if settings.notifications_enabled && settings.notification_scope != "all" {
            match item.thread_id {
                Some(id) => core
                    .notification_thread_tab(id)
                    .await
                    .map_err(|e| e.to_string())?,
                None => None,
            }
        } else {
            None
        };
        match decision(
            settings.notifications_enabled,
            &settings.notification_scope,
            &settings.notification_tabs,
            route.as_ref(),
        ) {
            Decision::Defer => {
                core.defer_notification_delivery(
                    item.id,
                    core::models::now_ms().saturating_add(3_000),
                )
                .await
                .map_err(|e| e.to_string())?;
            }
            Decision::Suppress(reason) => {
                core.suppress_notification_delivery(item.id, reason)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Decision::Deliver => {
                if !notification_can_dispatch(core)
                    || !core
                        .claim_notification_delivery(item.id)
                        .await
                        .map_err(|e| e.to_string())?
                {
                    continue;
                }
                let sender = item
                    .sender_name
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .or(item.sender_addr.as_deref())
                    .unwrap_or("新邮件");
                let payload = json!({
                    "title": format!("飞鸿邮箱 · {sender}"),
                    "body": item.subject,
                    // Stable across crashes: repeated delivery replaces the same
                    // OS notification, whose ArkTS request has isAlertOnce=true.
                    "messageId": format!("mail:{}:{}", item.account_id, item.message_id),
                    "sound": settings.sound_enabled,
                });
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    request("notify", payload),
                )
                .await
                {
                    Ok(Ok(_)) => {
                        core.mark_notification_delivered(item.id)
                            .await
                            .map_err(|e| e.to_string())?;
                        delivered += 1;
                    }
                    _ => {
                        // Do not log message contents or service payloads.
                        core.retry_notification_delivery(
                            item.id,
                            retry_delay_ms(item.attempts),
                            "HarmonyOS notification service unavailable",
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    }
                }
            }
        }
    }
    Ok(delivered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_includes_other_and_pending_while_selected_tabs_wait_for_routing() {
        let other = RoutedTab::Resolved("other".into());
        assert_eq!(decision(true, "all", &[], Some(&other)), Decision::Deliver);
        assert_eq!(
            decision(true, "all", &[], Some(&RoutedTab::Pending)),
            Decision::Deliver
        );
        assert_eq!(
            decision(true, "important", &[], Some(&other)),
            Decision::Suppress("outside notification scope")
        );
        assert_eq!(
            decision(true, "tabs", &["other".into()], Some(&other)),
            Decision::Deliver
        );
        assert_eq!(
            decision(true, "tabs", &["other".into()], Some(&RoutedTab::Pending)),
            Decision::Defer
        );
        assert_eq!(
            decision(false, "all", &[], Some(&other)),
            Decision::Suppress("notifications disabled")
        );
    }

    #[test]
    fn retries_are_bounded_without_discarding_a_failed_delivery() {
        assert_eq!(retry_delay_ms(0), 5_000);
        assert_eq!(retry_delay_ms(1), 10_000);
        assert_eq!(retry_delay_ms(i64::MAX), 300_000);
    }
}
