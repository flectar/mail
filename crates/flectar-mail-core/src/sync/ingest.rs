//! Importing newly delivered mail, shared by the protocols that download
//! messages themselves: threading and the message row, contact suggestions,
//! new-mail notifications, tab routing, and the parsed body. Each protocol
//! keeps its own rules for recognizing a message it already has.

use super::engine::SyncCtx;
use crate::calendar::IcsEvent;
use crate::db::repo::{self, messages::NewMessage, sender_identities::OwnAddresses};
use crate::error::Result;
use crate::events::CoreEvent;
use crate::mime::{ParsedBody, ParsedHeaders};
use crate::models::{Settings, now_ms, roles};
use rusqlite::Connection;

/// A message older than this never triggers the new-mail chime or a
/// notification, even if it lands unread, so old mail that a server replays
/// as new stays silent.
const CHIME_RECENT_MS: i64 = 24 * 60 * 60 * 1000;

/// How a protocol delivered a message.
pub(crate) struct Delivery<'a> {
    pub folder_id: i64,
    pub folder_role: Option<&'a str>,
    pub uid: Option<i64>,
    /// When the server received the message, if it records that.
    pub received_ms: Option<i64>,
    pub seen: bool,
    pub flagged: bool,
    pub draft: bool,
    pub has_attachments: bool,
    pub size: Option<i64>,
}

/// A message row created by [`Importer::insert`].
pub(crate) struct Imported {
    pub message_id: i64,
    pub thread_id: i64,
    pub is_read: bool,
    pub is_outgoing: bool,
    is_automated: bool,
    from_email: String,
    folder_role: Option<String>,
    /// The server's receipt time, else the message's own date.
    arrival_ms: i64,
}

/// One write transaction's worth of imports into an account.
pub(crate) struct Importer {
    account_id: i64,
    settings: Settings,
    outgoing_learning_since: i64,
    incoming_learning_since: i64,
    own_addresses: OwnAddresses,
    thread_ids: Vec<i64>,
    /// Threads already queued for a desktop notification, so a thread that
    /// gains several messages at once notifies once.
    notified_threads: Vec<i64>,
    fresh_thread_ids: Vec<i64>,
}

impl Importer {
    pub(crate) fn new(conn: &Connection, account_id: i64) -> Result<Self> {
        let settings = repo::settings::get(conn)?;
        let (outgoing_learning_since, incoming_learning_since) =
            repo::contacts::learning_boundaries(conn, account_id, now_ms())?;
        Ok(Self {
            account_id,
            settings,
            outgoing_learning_since,
            incoming_learning_since,
            own_addresses: repo::sender_identities::own_addresses(conn, account_id)?,
            thread_ids: Vec::new(),
            notified_threads: Vec::new(),
            fresh_thread_ids: Vec::new(),
        })
    }

    /// Insert a message the account didn't have, in its thread. Recompute
    /// the thread and index the message once the protocol has stored the rest
    /// of what it knows about it.
    pub(crate) fn insert(
        &mut self,
        conn: &Connection,
        headers: &ParsedHeaders,
        delivery: &Delivery,
    ) -> Result<Imported> {
        let date_ms = headers
            .date_ms
            .or(delivery.received_ms)
            .unwrap_or_else(now_ms);
        let from_email = headers
            .from
            .as_ref()
            .map(|address| address.email.to_lowercase())
            .unwrap_or_default();
        let is_outgoing =
            self.own_addresses.contains(&from_email) || delivery.folder_role == Some(roles::SENT);
        let thread_id = super::threading::resolve_thread(conn, self.account_id, headers, date_ms)?;
        let message = NewMessage {
            account_id: self.account_id,
            folder_id: delivery.folder_id,
            uid: delivery.uid,
            message_id: headers.message_id.clone(),
            gm_msgid: None,
            gm_thrid: None,
            subject: headers.subject.clone(),
            from: headers.from.clone(),
            to: headers.to.clone(),
            cc: headers.cc.clone(),
            bcc: headers.bcc.clone(),
            date: date_ms,
            internal_date: delivery.received_ms,
            is_read: delivery.seen || is_outgoing,
            is_starred: delivery.flagged,
            is_draft: delivery.draft || delivery.folder_role == Some(roles::DRAFTS),
            is_outgoing,
            is_automated: headers.is_automated,
            has_attachments: delivery.has_attachments,
            size: delivery.size,
            snippet: String::new(),
            references: headers.references.clone(),
            list_unsubscribe: headers.list_unsubscribe.clone(),
            list_unsubscribe_post: headers.list_unsubscribe_post.clone(),
            sender_addr: headers.via.clone(),
            sender_verification: headers.sender_verification,
        };
        let message_id = repo::messages::insert(conn, &message, thread_id)?;
        if !self.thread_ids.contains(&thread_id) {
            self.thread_ids.push(thread_id);
        }
        Ok(Imported {
            message_id,
            thread_id,
            is_read: message.is_read,
            is_outgoing,
            is_automated: headers.is_automated,
            from_email,
            folder_role: delivery.folder_role.map(str::to_owned),
            arrival_ms: delivery.received_ms.unwrap_or(date_ms),
        })
    }

    /// Learn compose suggestions without polluting the real address book.
    /// Outgoing recipients are the useful default; incoming senders require
    /// opt-in and exclude automated and junk mail.
    pub(crate) fn learn_contacts(
        &self,
        conn: &Connection,
        headers: &ParsedHeaders,
        imported: &Imported,
    ) -> Result<()> {
        let when = imported.arrival_ms;
        if imported.is_outgoing
            && self.settings.collect_outgoing_contacts
            && when >= self.outgoing_learning_since
        {
            let mut harvested = std::collections::HashSet::new();
            for address in headers
                .to
                .iter()
                .chain(headers.cc.iter())
                .chain(headers.bcc.iter())
            {
                let email = address.email.to_ascii_lowercase();
                if !self.own_addresses.contains(&email) && harvested.insert(email) {
                    repo::contacts::harvest(conn, self.account_id, address, true, when)?;
                }
            }
        } else if self.settings.collect_incoming_contacts
            && when >= self.incoming_learning_since
            && !headers.is_automated
            && !matches!(
                imported.folder_role.as_deref(),
                Some(roles::SPAM) | Some(roles::TRASH)
            )
            && let Some(from) = &headers.from
            && !crate::mime::robot_sender(&from.email)
        {
            repo::contacts::harvest(conn, self.account_id, from, false, when)?;
        }
        Ok(())
    }

    /// Queue the notification for a live arrival: mail that reached the
    /// mailbox while the account was being followed, rather than history
    /// caught up on.
    pub(crate) fn announce(
        &mut self,
        conn: &Connection,
        imported: &Imported,
        live: bool,
    ) -> Result<()> {
        if !arrival_eligible(
            live,
            imported.is_read,
            imported.is_outgoing,
            imported.arrival_ms,
            now_ms(),
        ) {
            return Ok(());
        }
        let thread_id = imported.thread_id;
        // A durable outbox row, once per thread in this batch. The native
        // dispatcher consumes it after commit and applies the user's
        // notification scope against the thread's resolved tab, so bulk mail
        // is queued too and filtered there. Snapshotting sender and subject now
        // keeps delivery independent of any later UI or hydration step.
        if !self.notified_threads.contains(&thread_id) {
            self.notified_threads.push(thread_id);
            repo::notifications::enqueue(conn, imported.message_id)?;
        }
        // The audible chime stays human-only regardless of scope.
        if chime_worthy(imported.is_automated, &imported.from_email)
            && !self.fresh_thread_ids.contains(&thread_id)
        {
            tracing::info!(
                account_id = self.account_id,
                thread_id,
                from = %imported.from_email,
                "receive: chime-worthy new mail",
            );
            self.fresh_thread_ids.push(thread_id);
        }
        Ok(())
    }

    /// Resolve each touched thread to a tab now that all of its messages are
    /// in place (see crate::route). Returns every touched thread and the
    /// threads that gained chime-worthy mail.
    pub(crate) fn finish(self, conn: &Connection) -> Result<(Vec<i64>, Vec<i64>)> {
        if self.settings.auto_labels_enabled {
            let splits = repo::splits::list(conn)?;
            for &thread_id in &self.thread_ids {
                crate::route::route_thread_deterministic(
                    conn,
                    &splits,
                    self.settings.ai_categorize,
                    thread_id,
                )?;
            }
        }
        Ok((self.thread_ids, self.fresh_thread_ids))
    }
}

/// Whether an arrival may surface as new mail: live, unread, incoming, and
/// genuinely recent. The desktop-notification scope (Important, all, or
/// specific tabs) is applied later by the host dispatcher, which can see the
/// thread's resolved tab; bulk and robot filtering lives in
/// [`chime_worthy`].
fn arrival_eligible(
    live: bool,
    is_read: bool,
    is_outgoing: bool,
    arrival_ms: i64,
    now: i64,
) -> bool {
    live && !is_read && !is_outgoing && now - arrival_ms < CHIME_RECENT_MS
}

/// Whether a fresh arrival should ring the audible new-mail chime. The chime
/// stays human-only regardless of the desktop-notification scope, so a
/// newsletter or monitoring flood never rings the bell every minute.
fn chime_worthy(is_automated: bool, from_email: &str) -> bool {
    !is_automated && !crate::mime::robot_sender(from_email)
}

/// Store a parsed full message: its text and sanitized HTML, the raw file,
/// attachments, sender verification, and the search index.
pub(crate) fn store_body(
    conn: &Connection,
    message_id: i64,
    raw_path: &str,
    parsed: &ParsedBody,
) -> Result<()> {
    repo::messages::store_body(
        conn,
        message_id,
        parsed.text.as_deref(),
        parsed.html.as_deref(),
        Some(raw_path),
        // Real files only, mirroring BODYSTRUCTURE's disposition check and
        // the message-card footer (both ignore inline images), so the list
        // paperclip stays consistent before and after the body arrives.
        parsed
            .attachments
            .iter()
            .any(|attachment| !attachment.is_inline),
        Some(&parsed.snippet),
    )?;
    repo::messages::set_sender_verification(conn, message_id, parsed.headers.sender_verification)?;
    let attachments: Vec<_> = parsed
        .attachments
        .iter()
        .map(|attachment| repo::messages::NewAttachment {
            message_id,
            part_id: Some(&attachment.part_id),
            filename: attachment.filename.as_deref(),
            mime_type: attachment.mime_type.as_deref(),
            size: Some(attachment.size),
            content_id: attachment.content_id.as_deref(),
            is_inline: attachment.is_inline,
        })
        .collect();
    repo::messages::replace_attachments(conn, message_id, &attachments)?;
    repo::search::index_message(conn, message_id)
}

/// The meeting invitations a message carries.
pub(crate) fn calendar_events(parsed: &ParsedBody) -> Vec<IcsEvent> {
    parsed
        .calendar_parts
        .iter()
        .flat_map(|ics| crate::calendar::parse_ics(ics))
        .collect()
}

/// Record a stored message's invitations in the calendar.
pub(crate) async fn store_calendar_events(
    ctx: &SyncCtx,
    account_id: i64,
    message_id: i64,
    events: Vec<IcsEvent>,
) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    ctx.calendar_db
        .write(move |conn| {
            let tx = conn.transaction()?;
            for event in &events {
                repo::calendar::upsert(&tx, account_id, message_id, event)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    ctx.bus.emit(CoreEvent::CalendarUpdated { account_id });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CHIME_RECENT_MS, arrival_eligible, chime_worthy};

    fn eligible(live: bool, read: bool, outgoing: bool, age_ms: i64) -> bool {
        let now = 2 * CHIME_RECENT_MS;
        arrival_eligible(live, read, outgoing, now - age_ms, now)
    }

    #[test]
    fn only_live_unread_incoming_mail_is_a_notification_candidate() {
        // Automated and robot senders are still candidates here; scope
        // filtering happens later in the host dispatcher.
        assert!(eligible(true, false, false, 1_000));
        // History being caught up on, read or outgoing mail, and mail that
        // arrived outside the recency window all disqualify.
        assert!(!eligible(false, false, false, 1_000));
        assert!(!eligible(true, true, false, 1_000));
        assert!(!eligible(true, false, true, 1_000));
        assert!(!eligible(true, false, false, CHIME_RECENT_MS));
    }

    #[test]
    fn chime_stays_human_only() {
        assert!(chime_worthy(false, "alice@example.com"));
        assert!(!chime_worthy(true, "digest@example.com"));
        assert!(!chime_worthy(false, "notifications@example.com"));
    }
}
