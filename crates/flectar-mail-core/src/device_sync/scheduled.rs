//! Send later on every device (issue #39).
//!
//! A device that schedules a send with Flectar Mail Sync shares it as a
//! `scheduled` record and holds its send. Shortly before the send time, each
//! device that can send it claims it in that record. The server takes the
//! changes to one record one at a time, so the first claim to reach it wins
//! and every other device sees it; only the device whose claim the server
//! took sends, and only while the claim holds. That device then marks the
//! record sent, or failed.
//!
//! The device that scheduled it sends its own draft, and claims first.
//! Other devices send the draft as the mail server keeps it, so they take
//! part only once it holds the version that device uploaded. An IMAP or
//! POP3 account keeps drafts on the device that wrote them, so there the
//! record carries the message itself, which other devices keep as a draft
//! until it goes out. A message too large for a record goes into a file in
//! Flectar Mail Sync's storage instead: the record carries a preview that
//! other devices show, and only the device that sends it downloads it. When Flectar Mail Sync cannot be reached, only the
//! device that scheduled it sends it, a little late, after checking that no
//! other device did.

use super::store::{self, RecordId};
use crate::db::repo;
use crate::db::repo::actions::HELD;
use crate::db::repo::synced_sends::{self, CANCELLED, FAILED, SCHEDULED, SENT, Shared, SyncedSend};
use crate::error::Result;
use base64::Engine as _;
use flectar_mail_sync_client::Entry;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

/// The device that scheduled a send claims it this long before its time.
pub(crate) const ORIGIN_LEAD_MS: i64 = 2 * 60_000;
/// Other devices claim it this long before its time, if no device has.
pub(crate) const HELPER_LEAD_MS: i64 = 60_000;
/// A claim holds this long past the send time, or past when it was made.
pub(crate) const CLAIM_MS: i64 = 15 * 60_000;
/// A claim whose send still waits, for example to retry, is renewed when
/// this little of it is left.
const RENEW_LEFT_MS: i64 = 5 * 60_000;
/// When Flectar Mail Sync cannot be reached, the device that scheduled a send
/// sends it this long after its time, so a device that claimed it before
/// had time to send it, and its copy reached Sent.
const UNREACHABLE_DELAY_MS: i64 = 10 * 60_000;
/// Finished sends are forgotten after a week.
const KEEP_FINISHED_MS: i64 = 7 * 24 * 60 * 60_000;
/// A send no device here can send, a month past its time, is forgotten.
const KEEP_STUCK_MS: i64 = 30 * 24 * 60 * 60_000;
/// The largest IMAP or POP3 message a record carries: its base64 text stays
/// well under the server's record limit, as one record too large would stop
/// every upload. A larger message goes out from the device that scheduled
/// it.
pub(crate) const MAX_SHARED_MESSAGE_BYTES: usize = 176 * 1024;
/// The version of a message too large to share: only the device that
/// scheduled it can send it.
pub(crate) const LOCAL_ONLY: &str = "local";
/// The largest message that goes into a file: what mail servers accept.
pub(crate) const MAX_FILE_MESSAGE_BYTES: usize = 25 * 1000 * 1000;

/// A shared send as its record says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Record<'a> {
    pub account: &'a str,
    pub message_id: &'a str,
    pub send_at: i64,
    pub origin: &'a str,
    pub draft: Option<&'a str>,
    pub state: &'a str,
    pub error: Option<&'a str>,
    pub claim: Option<(&'a str, i64)>,
    /// An IMAP or POP3 message, in base64.
    pub mime: Option<&'a str>,
    /// The file a message too large for the record went into.
    pub file: Option<&'a Value>,
    /// What other devices show of that message, in base64.
    pub preview: Option<&'a str>,
}

impl Record<'_> {
    /// The message the record carries, when it is the version to send.
    pub(super) fn message(&self) -> Option<Vec<u8>> {
        let text = self.mime?;
        base64::engine::general_purpose::STANDARD.decode(text).ok()
    }

    /// The file reference, as stored.
    pub(super) fn file_ref(&self) -> Option<String> {
        self.file
            .and_then(FileRef::parse)
            .map(|file| file.to_json())
    }

    pub(super) fn preview_bytes(&self) -> Option<Vec<u8>> {
        let text = self.preview?;
        base64::engine::general_purpose::STANDARD.decode(text).ok()
    }
}

/// A file in Flectar Mail Sync's storage, as records name it: its id, the
/// key it is sealed under, and its sealed size.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct FileRef {
    pub id: [u8; 16],
    pub key: [u8; 32],
    pub size: u64,
}

impl std::fmt::Debug for FileRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileRef")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

fn hex_bytes<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; N];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn hex_text(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl FileRef {
    pub(crate) fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            id: hex_bytes(value.get("id")?.as_str()?)?,
            key: hex_bytes(value.get("key")?.as_str()?)?,
            size: value.get("size")?.as_u64()?,
        })
    }

    pub(crate) fn from_json(text: &str) -> Option<Self> {
        Self::parse(&serde_json::from_str(text).ok()?)
    }

    pub(crate) fn to_json(&self) -> String {
        json!({"id": hex_text(&self.id), "key": hex_text(&self.key), "size": self.size}).to_string()
    }
}

/// What a shared send's record carries besides its schedule.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Carried<'a> {
    pub mime: Option<&'a [u8]>,
    /// The file reference, as stored.
    pub file: Option<&'a str>,
    pub preview: Option<&'a [u8]>,
}

pub(super) fn parse(body: &Value) -> Option<Record<'_>> {
    let state = body.get("state").and_then(Value::as_str)?;
    if ![SCHEDULED, SENT, CANCELLED, FAILED].contains(&state) {
        return None;
    }
    let claim = body.get("claim").and_then(|claim| {
        Some((
            claim.get("device").and_then(Value::as_str)?,
            claim.get("until").and_then(Value::as_i64)?,
        ))
    });
    Some(Record {
        account: body.get("account").and_then(Value::as_str)?,
        message_id: body
            .get("messageId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())?,
        send_at: body.get("sendAt").and_then(Value::as_i64)?,
        origin: body.get("origin").and_then(Value::as_str)?,
        draft: body.get("draft").and_then(Value::as_str),
        state,
        error: body.get("error").and_then(Value::as_str),
        claim,
        mime: body.get("mime").and_then(Value::as_str),
        file: body.get("file").filter(|file| file.is_object()),
        preview: body.get("preview").and_then(Value::as_str),
    })
}

/// The record body of a shared send of the account with sync id `account`,
/// with the IMAP or POP3 message, or file and preview, it carries.
pub(super) fn body(send: &SyncedSend, account: &str, carried: Carried) -> Value {
    let mut body = json!({
        "account": account,
        "messageId": send.message_id,
        "sendAt": send.send_at,
        "origin": send.origin,
        "draft": send.draft_ref,
        "state": send.state,
        "error": send.error,
        "claim": match (&send.claimed_by, send.claim_until) {
            (Some(device), Some(until)) => json!({"device": device, "until": until}),
            _ => Value::Null,
        },
    });
    let base64 = &base64::engine::general_purpose::STANDARD;
    if let Some(mime) = carried.mime {
        body["mime"] = json!(base64.encode(mime));
    }
    if let Some(file) = carried
        .file
        .and_then(|file| serde_json::from_str::<Value>(file).ok())
    {
        body["file"] = file;
    }
    if let Some(preview) = carried.preview {
        body["preview"] = json!(base64.encode(preview));
    }
    body
}

/// Settle a merge of this device's change to a shared send with another
/// device's that reached the server first. A claim is never merged: the one
/// the server took stands, so two devices never both think they may send.
/// A send that went out stays sent.
pub(super) fn settle(base: Option<&Value>, local: &Value, remote: &Value, merged: &mut Value) {
    let Some(fields) = merged.as_object_mut() else {
        return;
    };
    let claim_of = |body: Option<&Value>| body.and_then(|body| body.get("claim")).cloned();
    let remote_claim = claim_of(Some(remote));
    if remote_claim != claim_of(base) {
        fields.insert("claim".into(), remote_claim.unwrap_or(Value::Null));
    } else if let Some(local_claim) = claim_of(Some(local)) {
        fields.insert("claim".into(), local_claim);
    }
    let sent = [local, remote]
        .iter()
        .any(|body| body.get("state").and_then(Value::as_str) == Some(SENT));
    if sent {
        fields.insert("state".into(), json!(SENT));
        fields.insert("error".into(), Value::Null);
    }
}

/// The draft of a shared send here: its id, conversation and version on the
/// mail server.
struct Draft {
    id: i64,
    thread_id: Option<i64>,
    version: Option<String>,
}

fn draft(conn: &Connection, account_id: i64, message_id: &str) -> Result<Option<Draft>> {
    Ok(conn
        .query_row(
            "SELECT m.id, m.thread_id,
                    CASE a.mail_protocol
                      WHEN 'gmail' THEN CASE WHEN m.gmail_draft_id IS NOT NULL THEN m.gm_msgid END
                      WHEN 'jmap' THEN m.jmap_id
                      -- Another device's message kept here as a draft.
                      ELSE (SELECT ss.draft_ref FROM synced_sends ss WHERE ss.shadow_id = m.id) END
             FROM messages m JOIN accounts a ON a.id = m.account_id
             WHERE m.account_id = ?1 AND m.message_id = ?2 AND m.is_draft = 1
             ORDER BY m.id DESC LIMIT 1",
            params![account_id, message_id],
            |row| {
                Ok(Draft {
                    id: row.get(0)?,
                    thread_id: row.get(1)?,
                    version: row.get(2)?,
                })
            },
        )
        .optional()?)
}

/// Whether the account holds a sent copy of the message.
fn sent_copy(conn: &Connection, account_id: i64, message_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM messages
           WHERE account_id = ?1 AND message_id = ?2 AND is_draft = 0)",
        params![account_id, message_id],
        |row| row.get(0),
    )?)
}

fn protocol(conn: &Connection, account_id: i64) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT mail_protocol FROM accounts WHERE id = ?1",
            params![account_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Whether another device can send the account's drafts: as the mail server
/// keeps them, or as the record carries them.
fn shareable(conn: &Connection, account_id: i64) -> Result<bool> {
    Ok(matches!(
        protocol(conn, account_id)?.as_deref(),
        Some("gmail" | "jmap" | "imap" | "pop3")
    ))
}

/// Whether the account's scheduled messages travel in their records.
pub(crate) fn carries_message(protocol: &str) -> bool {
    matches!(protocol, "imap" | "pop3")
}

/// Keep another device's IMAP or POP3 message here as a draft, in the
/// account's Drafts, while it waits to go out. Returns its conversation.
fn materialize(conn: &Connection, send: &SyncedSend, mime: &[u8]) -> Result<Option<i64>> {
    let Some(drafts) = repo::folders::by_role(conn, send.account_id, crate::models::roles::DRAFTS)?
    else {
        return Ok(None);
    };
    let parsed = crate::sync::ingest::parse_or_text(mime);
    if parsed.headers.message_id.as_deref() != Some(send.message_id.as_str()) {
        tracing::warn!("a shared message does not carry the Message-ID of its schedule");
        return Ok(None);
    }
    let has_attachments = parsed.attachments.iter().any(|file| !file.is_inline);
    let mut importer = crate::sync::ingest::Importer::new(conn, send.account_id)?;
    let imported = importer.insert(
        conn,
        &parsed.headers,
        &crate::sync::ingest::Delivery {
            folder_id: drafts.id,
            folder_role: Some(crate::models::roles::DRAFTS),
            uid: None,
            received_ms: None,
            seen: true,
            flagged: false,
            draft: true,
            has_attachments,
            size: i64::try_from(mime.len()).ok(),
        },
    )?;
    // Its files open from the copy that goes out; this one only shows it.
    repo::messages::store_body(
        conn,
        imported.message_id,
        parsed.text.as_deref(),
        parsed.html.as_deref(),
        None,
        has_attachments,
        Some(&parsed.snippet),
    )?;
    repo::threads::recompute(conn, imported.thread_id)?;
    repo::search::index_message(conn, imported.message_id)?;
    synced_sends::set_shadow(conn, &send.sync_id, Some(imported.message_id))?;
    Ok(Some(imported.thread_id))
}

fn thread_of_action(conn: &Connection, action_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT m.thread_id FROM pending_actions pa
             JOIN messages m ON m.id = pa.message_id WHERE pa.id = ?1",
            params![action_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

fn action_state(conn: &Connection, action_id: i64) -> Result<Option<(String, Option<String>)>> {
    Ok(conn
        .query_row(
            "SELECT state, last_error FROM pending_actions WHERE id = ?1",
            params![action_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

/// Hold a waiting send until this device's claim on it holds.
fn hold(conn: &Connection, action_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE pending_actions SET not_before = ?2 WHERE id = ?1 AND state = 'pending'",
        params![action_id, HELD],
    )?;
    Ok(())
}

/// Let a held send go out at `at`. Returns whether it was held.
fn release(conn: &Connection, action_id: i64, at: i64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE pending_actions SET not_before = ?2
         WHERE id = ?1 AND state = 'pending' AND not_before = ?3",
        params![action_id, at, HELD],
    )? > 0)
}

/// Let a send go out at `at`: a held one, or one not tried yet that waits
/// for another time, as when the send was moved. A send that waits to try
/// again keeps its wait. Returns whether its time changed.
fn send_at(conn: &Connection, action_id: i64, at: i64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE pending_actions SET not_before = ?2
         WHERE id = ?1 AND state = 'pending' AND not_before IS NOT ?2
           AND (not_before = ?3 OR attempts = 0)",
        params![action_id, at, HELD],
    )? > 0)
}

/// Take on a shared send another device changed. Returns the conversation
/// whose scheduled send changed here.
pub(super) fn put(
    conn: &Connection,
    sync_id: &str,
    shared: &Shared,
    now: i64,
) -> Result<Option<i64>> {
    let before = synced_sends::get(conn, sync_id)?;
    synced_sends::put_shared(conn, sync_id, shared, now)?;
    let Some(before) = before else {
        return Ok(None);
    };
    let Some(action) = before.action_id else {
        return Ok(None);
    };
    let thread = thread_of_action(conn, action)?;
    // Another claim, or none: this device stops sending it until the next
    // drive finds its own claim holds.
    if (before.claimed_by.as_deref(), before.claim_until) != (shared.claimed_by, shared.claim_until)
        && before.may_send_until.is_some()
    {
        synced_sends::set_may_send_until(conn, sync_id, None)?;
        hold(conn, action)?;
    }
    // Another version of the draft: a device that sends the server's draft
    // waits for that version to arrive, and one that keeps another device's
    // message as a draft takes on the new one.
    if before.draft_ref.as_deref() != shared.draft_ref && sends_as_is(conn, action)? {
        repo::actions::try_cancel(conn, action)?;
        synced_sends::set_action(conn, sync_id, None)?;
        synced_sends::drop_shadow(conn, sync_id)?;
        return Ok(thread);
    }
    match shared.state {
        // Sent or cancelled on another device: this device's send stops.
        SENT | CANCELLED => {
            repo::actions::try_cancel(conn, action)?;
            synced_sends::set_action(conn, sync_id, None)?;
            synced_sends::set_may_send_until(conn, sync_id, None)?;
            synced_sends::drop_shadow(conn, sync_id)?;
        }
        FAILED => {
            conn.execute(
                "UPDATE pending_actions SET state = 'failed', last_error = ?2, finished_at = ?3
                 WHERE id = ?1 AND state = 'pending'",
                params![action, shared.error, now],
            )?;
        }
        _ => {
            // Scheduled again after it failed or was cancelled.
            if before.state != SCHEDULED {
                match action_state(conn, action)?
                    .map(|(state, _)| state)
                    .as_deref()
                {
                    Some("failed") => {
                        conn.execute(
                            "UPDATE pending_actions SET state = 'pending', not_before = ?2,
                                    attempts = 0, last_error = NULL, finished_at = NULL
                             WHERE id = ?1",
                            params![action, HELD],
                        )?;
                    }
                    Some("pending") => hold(conn, action)?,
                    _ => synced_sends::set_action(conn, sync_id, None)?,
                }
            }
            set_send_at(conn, action, shared.send_at)?;
        }
    }
    Ok(thread)
}

/// Whether this device's send goes out as the mail server keeps the draft.
fn sends_as_is(conn: &Connection, action_id: i64) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT json_extract(payload, '$.asIs') = 1 FROM pending_actions WHERE id = ?1",
            params![action_id],
            |row| row.get::<_, Option<bool>>(0),
        )
        .optional()?
        .flatten()
        .unwrap_or(false))
}

/// A shared send another device forgot. Returns its conversation here.
pub(super) fn remove(conn: &Connection, sync_id: &str) -> Result<Option<i64>> {
    let Some(send) = synced_sends::delete(conn, sync_id)? else {
        return Ok(None);
    };
    let Some(action) = send.action_id else {
        return Ok(None);
    };
    let held: bool = conn
        .query_row(
            "SELECT not_before = ?2 FROM pending_actions WHERE id = ?1 AND state = 'pending'",
            params![action, HELD],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false);
    if held {
        repo::actions::try_cancel(conn, action)?;
    }
    thread_of_action(conn, action)
}

/// Keep the send time a held send shows in step with its record.
fn set_send_at(conn: &Connection, action_id: i64, send_at: i64) -> Result<()> {
    conn.execute(
        "UPDATE pending_actions SET payload = json_set(payload, '$.sendAt', ?2)
         WHERE id = ?1 AND state IN ('pending', 'failed')
           AND json_extract(payload, '$.sendAt') IS NOT ?2",
        params![action_id, send_at],
    )?;
    Ok(())
}

/// Whether Flectar Mail Sync can take claims right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    /// The last run reached the server, which takes this device's changes.
    Usable,
    /// The server cannot be reached, or the last run failed.
    Unreachable,
    /// The server refuses changes until the group pays: no device can
    /// claim, so the device that scheduled a send sends it on time.
    PaymentRequired,
}

/// What driving the shared sends changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Driven {
    /// Accounts whose sends came due.
    pub nudge: Vec<i64>,
    /// Conversations whose scheduled send changed.
    pub threads: Vec<i64>,
    /// A claim or a result waits to reach the other devices.
    pub upload: bool,
    /// Shared sends this device may send once their file is downloaded.
    pub fetch: Vec<String>,
}

impl Driven {
    fn thread(&mut self, thread: Option<i64>) {
        if let Some(thread) = thread
            && !self.threads.contains(&thread)
        {
            self.threads.push(thread);
        }
    }

    fn nudge(&mut self, account_id: i64) {
        if !self.nudge.contains(&account_id) {
            self.nudge.push(account_id);
        }
    }
}

/// The claim the server holds for a shared send, unless this device has a
/// change to it on the way: it then waits for the server's answer.
fn committed_claim(conn: &Connection, id: &RecordId) -> Result<Option<Option<(String, i64)>>> {
    if store::has_pending(conn, id)? {
        return Ok(None);
    }
    let entry: Option<Entry> = store::mirrored_entry(conn, id)?;
    Ok(Some(
        entry
            .as_ref()
            .filter(|entry| entry.collection == super::collections::SCHEDULED)
            .and_then(|entry| parse(&entry.body))
            .and_then(|record| record.claim)
            .map(|(device, until)| (device.to_owned(), until)),
    ))
}

/// Move the shared sends along on this device, as device `me` (hex): hold
/// or create its sends, claim the ones due, release the ones it claimed,
/// and report how its own sends went.
pub(crate) fn drive(conn: &Connection, me: &str, now: i64, reach: Reach) -> Result<Driven> {
    let mut driven = Driven::default();
    for mut send in synced_sends::all(conn)? {
        if send.state != SCHEDULED {
            continue;
        }
        let origin = send.origin == me;

        // How this device's own send went.
        if let Some(action) = send.action_id {
            match action_state(conn, action)? {
                Some((state, error))
                    if matches!(state.as_str(), "done" | "failed" | "cancelled") =>
                {
                    let result = match state.as_str() {
                        "done" => SENT,
                        "failed" => FAILED,
                        _ => CANCELLED,
                    };
                    let error = error.filter(|_| result == FAILED);
                    synced_sends::set_state(conn, &send.sync_id, result, error.as_deref(), now)?;
                    if result != FAILED {
                        synced_sends::set_action(conn, &send.sync_id, None)?;
                        // Sent from here, or cancelled here: the message
                        // kept for another device is this device's own now.
                        synced_sends::set_shadow(conn, &send.sync_id, None)?;
                    }
                    driven.upload = true;
                    driven.thread(thread_of_action(conn, action)?);
                    continue;
                }
                Some(_) => {}
                None => {
                    synced_sends::set_action(conn, &send.sync_id, None)?;
                    send.action_id = None;
                }
            }
        }

        // Already sent, as the account's Sent shows.
        if sent_copy(conn, send.account_id, &send.message_id)? {
            if let Some(action) = send.action_id {
                repo::actions::try_cancel(conn, action)?;
                driven.thread(thread_of_action(conn, action)?);
            }
            synced_sends::set_state(conn, &send.sync_id, SENT, None, now)?;
            synced_sends::set_action(conn, &send.sync_id, None)?;
            driven.upload = true;
            continue;
        }

        // Another device's IMAP or POP3 message comes with its schedule: it
        // waits here as a draft.
        if !origin
            && send.action_id.is_none()
            && send.shadow_id.is_none()
            && send
                .draft_ref
                .as_deref()
                .is_some_and(|version| version != LOCAL_ONLY)
            && protocol(conn, send.account_id)?
                .as_deref()
                .is_some_and(carries_message)
            && let Some(shown) = match synced_sends::mime(conn, &send.sync_id)? {
                Some(mime) => Some(mime),
                None => synced_sends::file(conn, &send.sync_id)?.1,
            }
        {
            driven.thread(materialize(conn, &send, &shown)?);
        }
        let local_draft = draft(conn, send.account_id, &send.message_id)?;

        // The device that scheduled it shares the version it uploaded.
        if origin
            && let Some(version) = local_draft.as_ref().and_then(|draft| draft.version.clone())
            && send.draft_ref.as_deref() != Some(version.as_str())
            && !draft_upload_waiting(conn, local_draft.as_ref().map(|draft| draft.id))?
        {
            synced_sends::set_draft_ref(conn, &send.sync_id, &version, now)?;
            send.draft_ref = Some(version);
            driven.upload = true;
        }

        // Make this device's send for it, held.
        if send.action_id.is_none() {
            let usable_draft = local_draft.as_ref().filter(|draft| {
                origin
                    || (send.draft_ref.is_some()
                        && draft.version.as_deref() == send.draft_ref.as_deref())
            });
            match usable_draft {
                Some(draft) if origin || shareable(conn, send.account_id)? => {
                    let action = hold_new(conn, &send, draft, origin)?;
                    synced_sends::set_action(conn, &send.sync_id, Some(action))?;
                    send.action_id = Some(action);
                    driven.thread(draft.thread_id);
                }
                // The device that scheduled it no longer has the draft: it
                // was deleted here.
                None if origin => {
                    synced_sends::set_state(conn, &send.sync_id, CANCELLED, None, now)?;
                    driven.upload = true;
                    continue;
                }
                // Nothing here can send it, long after its time.
                _ if send.send_at < now - KEEP_STUCK_MS => {
                    synced_sends::delete(conn, &send.sync_id)?;
                    driven.upload = true;
                    continue;
                }
                _ => continue,
            }
        }
        let Some(action) = send.action_id else {
            continue;
        };
        // Another device's draft changed here since: wait for the version
        // the device that scheduled it uploaded.
        if !origin
            && local_draft
                .as_ref()
                .and_then(|draft| draft.version.as_deref())
                != send.draft_ref.as_deref()
            && sends_as_is(conn, action)?
        {
            repo::actions::try_cancel(conn, action)?;
            synced_sends::set_action(conn, &send.sync_id, None)?;
            driven.thread(thread_of_action(conn, action)?);
            continue;
        }

        if reach != Reach::Usable {
            let late = match reach {
                Reach::PaymentRequired => 0,
                _ => UNREACHABLE_DELAY_MS,
            };
            // Without Flectar Mail Sync, only the device that scheduled it
            // sends it, and then without a claim.
            if origin
                && now >= send.send_at.saturating_add(late)
                && send.may_send_until != Some(HELD)
            {
                synced_sends::set_may_send_until(conn, &send.sync_id, Some(HELD))?;
                synced_sends::set_claim(conn, &send.sync_id, Some(me), Some(HELD), now)?;
                if release(conn, action, now)? {
                    driven.nudge(send.account_id);
                }
            }
            continue;
        }
        let Some(record_id) = super::collections::sync_id_bytes(&send.sync_id) else {
            continue;
        };
        let Some(claim) = committed_claim(conn, &record_id)? else {
            // This device's change waits to reach the server: upload it,
            // then act on the answer.
            driven.upload = true;
            continue;
        };
        match claim {
            // The send moved past this device's claim: it gives the claim
            // up, and the devices claim it again nearer the new time.
            Some((device, until)) if device == me && until > now && until <= send.send_at => {
                synced_sends::set_may_send_until(conn, &send.sync_id, None)?;
                hold(conn, action)?;
                synced_sends::set_claim(conn, &send.sync_id, None, None, now)?;
                driven.upload = true;
            }
            Some((device, until)) if device == me && until > now => {
                if send.may_send_until != Some(until) {
                    synced_sends::set_may_send_until(conn, &send.sync_id, Some(until))?;
                }
                // Another device's message in a file is fetched before its
                // send may start.
                if !origin && waits_for_file(conn, &send.sync_id)? {
                    driven.fetch.push(send.sync_id.clone());
                } else if send_at(conn, action, send.send_at)? {
                    driven.nudge(send.account_id);
                    driven.thread(thread_of_action(conn, action)?);
                }
                // A send that still waits, say to retry, keeps its claim.
                if until != HELD && until - now < RENEW_LEFT_MS {
                    synced_sends::set_claim(
                        conn,
                        &send.sync_id,
                        Some(me),
                        Some(now + CLAIM_MS),
                        now,
                    )?;
                    driven.upload = true;
                }
            }
            Some((_, until)) if until > now => {
                // Another device sends it.
                if send.may_send_until.is_some() {
                    synced_sends::set_may_send_until(conn, &send.sync_id, None)?;
                }
                hold(conn, action)?;
            }
            _ => {
                if send.may_send_until.is_some() {
                    synced_sends::set_may_send_until(conn, &send.sync_id, None)?;
                }
                hold(conn, action)?;
                let lead = if origin {
                    ORIGIN_LEAD_MS
                } else {
                    HELPER_LEAD_MS
                };
                if now >= send.send_at - lead {
                    let until = send.send_at.max(now) + CLAIM_MS;
                    synced_sends::set_claim(conn, &send.sync_id, Some(me), Some(until), now)?;
                    driven.upload = true;
                }
            }
        }
    }
    if synced_sends::prune_finished(conn, now - KEEP_FINISHED_MS)? > 0 {
        driven.upload = true;
    }
    Ok(driven)
}

/// Whether a shared message is in a file that is not here yet.
fn waits_for_file(conn: &Connection, sync_id: &str) -> Result<bool> {
    Ok(synced_sends::file(conn, sync_id)?.0.is_some()
        && synced_sends::mime(conn, sync_id)?.is_none())
}

/// Whether the draft's latest edit still waits to reach the mail server.
fn draft_upload_waiting(conn: &Connection, draft_id: Option<i64>) -> Result<bool> {
    let Some(draft_id) = draft_id else {
        return Ok(false);
    };
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pending_actions
           WHERE kind = 'save_draft' AND state IN ('pending', 'inflight')
             AND (message_id = ?1 OR json_extract(payload, '$.draftId') = ?1))",
        params![draft_id],
        |row| row.get(0),
    )?)
}

/// A held send of `draft` for a shared send. Another device's draft goes
/// out as the mail server keeps it.
fn hold_new(conn: &Connection, send: &SyncedSend, draft: &Draft, origin: bool) -> Result<i64> {
    let mut payload = json!({
        "draftId": draft.id,
        "scheduled": true,
        "sendAt": send.send_at,
        "syncedSend": send.sync_id,
    });
    if !origin {
        payload["asIs"] = json!(true);
        let protocol: String = conn.query_row(
            "SELECT mail_protocol FROM accounts WHERE id = ?1",
            params![send.account_id],
            |row| row.get(0),
        )?;
        // A JMAP send submits the server's Email as it is.
        if protocol == "jmap"
            && let Some(version) = &send.draft_ref
        {
            payload["jmapPreparedEmailId"] = json!(version);
        }
    }
    repo::actions::enqueue(
        conn,
        send.account_id,
        "send",
        Some(draft.id),
        draft.thread_id,
        &payload,
        Some(HELD),
    )
}

/// Stop sharing sends: Flectar Mail Sync was turned off here. A send of this
/// device's own draft goes out at its time from this device alone; a send
/// of another device's draft is dropped.
pub(crate) fn detach(conn: &Connection) -> Result<Vec<i64>> {
    let mut threads = Vec::new();
    for send in synced_sends::all(conn)? {
        if let Some(action) = send.action_id
            && send.state == SCHEDULED
        {
            if !sends_as_is(conn, action)? {
                conn.execute(
                    "UPDATE pending_actions SET not_before = ?2
                     WHERE id = ?1 AND state = 'pending' AND not_before = ?3",
                    params![action, send.send_at, HELD],
                )?;
            } else {
                repo::actions::try_cancel(conn, action)?;
            }
            if let Some(thread) = thread_of_action(conn, action)? {
                threads.push(thread);
            }
        }
        synced_sends::delete(conn, &send.sync_id)?;
    }
    Ok(threads)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    const AT: i64 = 10_000_000;
    const ORIGIN: &str = "aa";
    const HELPER: &str = "bb";

    /// A JMAP account and its draft, uploaded as Email `version`.
    fn setup(version: &str) -> Connection {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute_batch(
            "UPDATE accounts SET mail_protocol = 'jmap', sync_id = '0123456789abcdef0123456789abcdef';
             INSERT INTO threads (id, account_id, subject_norm, last_message_at) VALUES (5, 1, 'plans', 1);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (id, thread_id, account_id, folder_id, message_id, subject,
                                   from_addr, date, is_read, is_draft, jmap_id)
             VALUES (9, 5, 1, 1, 'plans@example.com', 'Plans', 'me@test.dev', 1, 1, 1, ?1)",
            params![version],
        )
        .unwrap();
        conn
    }

    /// The device that scheduled the draft: its held send and shared send.
    fn scheduled_here(conn: &Connection) -> (String, i64) {
        let action = repo::actions::enqueue(
            conn,
            1,
            "send",
            Some(9),
            Some(5),
            &json!({"draftId": 9, "scheduled": true, "sendAt": AT}),
            Some(HELD),
        )
        .unwrap();
        let sync_id =
            synced_sends::create(conn, 1, "plans@example.com", AT, ORIGIN, action, 0).unwrap();
        (sync_id, action)
    }

    /// Another device's shared send of the draft, as its record arrives.
    fn scheduled_elsewhere(conn: &Connection, draft: Option<&str>) -> String {
        let sync_id = "fedcba9876543210fedcba9876543210".to_owned();
        put(conn, &sync_id, &shared(draft, None), 0).unwrap();
        sync_id
    }

    fn shared<'a>(draft: Option<&'a str>, claim: Option<(&'static str, i64)>) -> Shared<'a> {
        Shared {
            account_id: 1,
            message_id: "plans@example.com",
            send_at: AT,
            origin: ORIGIN,
            draft_ref: draft,
            state: SCHEDULED,
            error: None,
            claimed_by: claim.map(|(device, _)| device),
            claim_until: claim.map(|(_, until)| until),
        }
    }

    /// The server took the record with `claim`.
    fn server_has(conn: &Connection, sync_id: &str, claim: Option<(&str, i64)>) {
        let send = synced_sends::get(conn, sync_id).unwrap().unwrap();
        let mut body = body(
            &send,
            "0123456789abcdef0123456789abcdef",
            Carried::default(),
        );
        body["claim"] = match claim {
            Some((device, until)) => json!({"device": device, "until": until}),
            None => Value::Null,
        };
        let id = super::super::collections::sync_id_bytes(sync_id).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO device_sync_mirror
               (record_id, seq, stamp_ms, stamp_counter, stamp_device, collection, schema, body)
             VALUES (?1, 1, 1, 0, ?2, 'scheduled', 1, ?3)",
            params![&id[..], &[0u8; 16][..], body.to_string()],
        )
        .unwrap();
        // This device takes the server's version.
        let shared = Shared {
            send_at: send.send_at,
            claimed_by: claim.map(|(device, _)| device),
            claim_until: claim.map(|(_, until)| until),
            ..shared(send.draft_ref.as_deref(), None)
        };
        synced_sends::put_shared(conn, sync_id, &shared, 0).unwrap();
    }

    fn not_before(conn: &Connection, action: i64) -> i64 {
        conn.query_row(
            "SELECT not_before FROM pending_actions WHERE id = ?1",
            params![action],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn action_state(conn: &Connection, action: i64) -> String {
        conn.query_row(
            "SELECT state FROM pending_actions WHERE id = ?1",
            params![action],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn the_device_that_scheduled_it_claims_it_and_sends_once_the_server_took_the_claim() {
        let conn = setup("E1");
        let (sync_id, action) = scheduled_here(&conn);

        // Well before the time: it shares the uploaded version, and waits.
        let driven = drive(&conn, ORIGIN, AT - 10 * 60_000, Reach::Usable).unwrap();
        assert!(driven.upload);
        let send = synced_sends::get(&conn, &sync_id).unwrap().unwrap();
        assert_eq!(send.draft_ref.as_deref(), Some("E1"));
        assert_eq!(send.claimed_by, None);

        // Shortly before: it claims.
        let now = AT - ORIGIN_LEAD_MS;
        assert!(drive(&conn, ORIGIN, now, Reach::Usable).unwrap().upload);
        let send = synced_sends::get(&conn, &sync_id).unwrap().unwrap();
        assert_eq!(send.claimed_by.as_deref(), Some(ORIGIN));
        assert_eq!(send.claim_until, Some(AT + CLAIM_MS));
        assert_eq!(
            not_before(&conn, action),
            HELD,
            "it holds the send until the server answers"
        );

        // The server took it: the send goes out at its time.
        server_has(&conn, &sync_id, Some((ORIGIN, AT + CLAIM_MS)));
        let driven = drive(&conn, ORIGIN, now + 1_000, Reach::Usable).unwrap();
        assert_eq!(driven.nudge, vec![1]);
        assert_eq!(not_before(&conn, action), AT);
        assert_eq!(
            synced_sends::get(&conn, &sync_id)
                .unwrap()
                .unwrap()
                .may_send_until,
            Some(AT + CLAIM_MS)
        );

        // Once sent, every device learns it.
        repo::actions::set_state(&conn, action, "done", None).unwrap();
        assert!(
            drive(&conn, ORIGIN, AT + 5_000, Reach::Usable)
                .unwrap()
                .upload
        );
        let send = synced_sends::get(&conn, &sync_id).unwrap().unwrap();
        assert_eq!((send.state.as_str(), send.action_id), (SENT, None));
    }

    #[test]
    fn the_device_holding_the_claim_follows_the_send_when_it_moves() {
        let conn = setup("E1");
        let (sync_id, action) = scheduled_here(&conn);
        let now = AT - ORIGIN_LEAD_MS;
        drive(&conn, ORIGIN, now, Reach::Usable).unwrap();
        server_has(&conn, &sync_id, Some((ORIGIN, AT + CLAIM_MS)));
        drive(&conn, ORIGIN, now, Reach::Usable).unwrap();
        assert_eq!(not_before(&conn, action), AT);

        // "Send now", from any device: it sends now, under the same claim.
        synced_sends::reschedule(&conn, &sync_id, now + 5, now + 5).unwrap();
        server_has(&conn, &sync_id, Some((ORIGIN, AT + CLAIM_MS)));
        assert_eq!(
            drive(&conn, ORIGIN, now + 10, Reach::Usable).unwrap().nudge,
            vec![1]
        );
        assert_eq!(not_before(&conn, action), now + 5);

        // Moved to tomorrow, past the claim: it gives the claim up.
        let tomorrow = AT + 86_400_000;
        synced_sends::reschedule(&conn, &sync_id, tomorrow, now + 20).unwrap();
        server_has(&conn, &sync_id, Some((ORIGIN, AT + CLAIM_MS)));
        assert!(
            drive(&conn, ORIGIN, now + 30, Reach::Usable)
                .unwrap()
                .upload
        );
        assert_eq!(not_before(&conn, action), HELD);
        let send = synced_sends::get(&conn, &sync_id).unwrap().unwrap();
        assert_eq!((send.claimed_by, send.may_send_until), (None, None));
    }

    #[test]
    fn a_device_holds_its_send_while_another_devices_claim_holds() {
        let conn = setup("E1");
        let sync_id = scheduled_elsewhere(&conn, Some("E1"));

        // It holds a send of the server's draft, ready to step in.
        drive(&conn, HELPER, AT - 10 * 60_000, Reach::Usable).unwrap();
        let send = synced_sends::get(&conn, &sync_id).unwrap().unwrap();
        let action = send.action_id.expect("a held send");
        assert_eq!(not_before(&conn, action), HELD);
        let payload: String = conn
            .query_row(
                "SELECT payload FROM pending_actions WHERE id = ?1",
                params![action],
                |row| row.get(0),
            )
            .unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["asIs"], json!(true));
        assert_eq!(payload["jmapPreparedEmailId"], json!("E1"));

        // The device that scheduled it claimed it first.
        server_has(&conn, &sync_id, Some((ORIGIN, AT + CLAIM_MS)));
        let driven = drive(&conn, HELPER, AT, Reach::Usable).unwrap();
        assert!(!driven.upload, "no claim of its own");
        assert_eq!(not_before(&conn, action), HELD);
        // Even made due, the send waits: this device may not send it.
        conn.execute(
            "UPDATE pending_actions SET not_before = 0 WHERE id = ?1",
            params![action],
        )
        .unwrap();
        assert!(!repo::actions::try_claim(&conn, action).unwrap());
        assert_eq!(not_before(&conn, action), HELD);

        // That claim ran out with nothing sent: this device steps in.
        let later = AT + CLAIM_MS + 1;
        assert!(drive(&conn, HELPER, later, Reach::Usable).unwrap().upload);
        let send = synced_sends::get(&conn, &sync_id).unwrap().unwrap();
        assert_eq!(send.claimed_by.as_deref(), Some(HELPER));
        assert_eq!(send.claim_until, Some(later + CLAIM_MS));
    }

    #[test]
    fn other_devices_send_only_the_version_that_was_uploaded() {
        let conn = setup("E1");
        // Not uploaded yet.
        let sync_id = scheduled_elsewhere(&conn, None);
        drive(&conn, HELPER, AT, Reach::Usable).unwrap();
        assert_eq!(
            synced_sends::get(&conn, &sync_id)
                .unwrap()
                .unwrap()
                .action_id,
            None
        );

        // Another version than this device has.
        put(&conn, &sync_id, &shared(Some("E2"), None), 1).unwrap();
        drive(&conn, HELPER, AT, Reach::Usable).unwrap();
        assert_eq!(
            synced_sends::get(&conn, &sync_id)
                .unwrap()
                .unwrap()
                .action_id,
            None
        );

        // That version arrives here.
        conn.execute("UPDATE messages SET jmap_id = 'E2' WHERE id = 9", [])
            .unwrap();
        drive(&conn, HELPER, AT - 10 * 60_000, Reach::Usable).unwrap();
        let action = synced_sends::get(&conn, &sync_id)
            .unwrap()
            .unwrap()
            .action_id
            .unwrap();

        // And it changes again: this device drops its send of the old one.
        put(&conn, &sync_id, &shared(Some("E3"), None), 2).unwrap();
        assert_eq!(action_state(&conn, action), "cancelled");
        assert_eq!(
            synced_sends::get(&conn, &sync_id)
                .unwrap()
                .unwrap()
                .action_id,
            None
        );
        drive(&conn, HELPER, AT - 10 * 60_000, Reach::Usable).unwrap();
        let send = synced_sends::get(&conn, &sync_id).unwrap().unwrap();
        assert_eq!(
            send.state, SCHEDULED,
            "a dropped old version is no cancellation"
        );
        assert_eq!(send.action_id, None);
    }

    #[test]
    fn a_send_finished_elsewhere_stops_here() {
        let conn = setup("E1");
        let sync_id = scheduled_elsewhere(&conn, Some("E1"));
        drive(&conn, HELPER, AT - 10 * 60_000, Reach::Usable).unwrap();
        let action = synced_sends::get(&conn, &sync_id)
            .unwrap()
            .unwrap()
            .action_id
            .unwrap();

        // It failed on the device that tried: it shows failed here too.
        let failed = Shared {
            state: FAILED,
            error: Some("mailbox full"),
            ..shared(Some("E1"), None)
        };
        assert_eq!(put(&conn, &sync_id, &failed, 1).unwrap(), Some(5));
        assert_eq!(action_state(&conn, action), "failed");

        // Scheduled again: held again.
        put(&conn, &sync_id, &shared(Some("E1"), None), 2).unwrap();
        assert_eq!(action_state(&conn, action), "pending");
        assert_eq!(not_before(&conn, action), HELD);

        // Sent elsewhere: this device's send is cancelled.
        let sent = Shared {
            state: SENT,
            ..shared(Some("E1"), None)
        };
        put(&conn, &sync_id, &sent, 3).unwrap();
        assert_eq!(action_state(&conn, action), "cancelled");
        let send = synced_sends::get(&conn, &sync_id).unwrap().unwrap();
        assert_eq!((send.state.as_str(), send.action_id), (SENT, None));
    }

    #[test]
    fn a_cancelled_or_failed_send_here_is_reported() {
        for (state, expected) in [("cancelled", CANCELLED), ("failed", FAILED)] {
            let conn = setup("E1");
            let (sync_id, action) = scheduled_here(&conn);
            repo::actions::set_state(&conn, action, state, Some("refused")).unwrap();
            assert!(drive(&conn, ORIGIN, AT, Reach::Usable).unwrap().upload);
            let send = synced_sends::get(&conn, &sync_id).unwrap().unwrap();
            assert_eq!(send.state, expected);
            assert_eq!(send.error.is_some(), expected == FAILED);
        }
    }

    #[test]
    fn a_draft_deleted_where_it_was_scheduled_cancels_it_everywhere() {
        let conn = setup("E1");
        let (sync_id, action) = scheduled_here(&conn);
        conn.execute("DELETE FROM pending_actions WHERE id = ?1", params![action])
            .unwrap();
        conn.execute("DELETE FROM messages WHERE id = 9", [])
            .unwrap();
        drive(&conn, ORIGIN, AT, Reach::Usable).unwrap();
        assert_eq!(
            synced_sends::get(&conn, &sync_id).unwrap().unwrap().state,
            CANCELLED
        );
    }

    #[test]
    fn a_send_nothing_here_can_send_is_forgotten_a_month_later() {
        let conn = setup("E1");
        let sync_id = scheduled_elsewhere(&conn, None);
        drive(&conn, HELPER, AT + KEEP_STUCK_MS - 1, Reach::Usable).unwrap();
        assert!(synced_sends::get(&conn, &sync_id).unwrap().is_some());
        assert!(
            drive(&conn, HELPER, AT + KEEP_STUCK_MS + 1, Reach::Usable)
                .unwrap()
                .upload
        );
        assert!(synced_sends::get(&conn, &sync_id).unwrap().is_none());
    }

    #[test]
    fn a_gmail_draft_counts_once_gmail_named_it() {
        let conn = setup("E1");
        conn.execute_batch(
            "UPDATE accounts SET provider = 'gmail', mail_protocol = 'gmail', auth_kind = 'oauth2';
             UPDATE messages SET gm_msgid = 'G1' WHERE id = 9;",
        )
        .unwrap();
        let sync_id = scheduled_elsewhere(&conn, Some("G1"));
        drive(&conn, HELPER, AT - 10 * 60_000, Reach::Usable).unwrap();
        assert_eq!(
            synced_sends::get(&conn, &sync_id)
                .unwrap()
                .unwrap()
                .action_id,
            None
        );
        conn.execute(
            "UPDATE messages SET gmail_draft_id = 'r-1' WHERE id = 9",
            [],
        )
        .unwrap();
        drive(&conn, HELPER, AT - 10 * 60_000, Reach::Usable).unwrap();
        let action = synced_sends::get(&conn, &sync_id)
            .unwrap()
            .unwrap()
            .action_id
            .unwrap();
        let payload: String = conn
            .query_row(
                "SELECT payload FROM pending_actions WHERE id = ?1",
                params![action],
                |row| row.get(0),
            )
            .unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["asIs"], json!(true));
        assert!(payload.get("jmapPreparedEmailId").is_none());
    }

    #[test]
    fn a_copy_in_sent_means_it_went_out() {
        let conn = setup("E1");
        let sync_id = scheduled_elsewhere(&conn, Some("E1"));
        drive(&conn, HELPER, AT - 10 * 60_000, Reach::Usable).unwrap();
        let action = synced_sends::get(&conn, &sync_id)
            .unwrap()
            .unwrap()
            .action_id
            .unwrap();
        conn.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, message_id, subject,
                                   from_addr, date, is_read, is_draft)
             VALUES (5, 1, 1, 'plans@example.com', 'Plans', 'me@test.dev', 2, 1, 0)",
            [],
        )
        .unwrap();
        drive(&conn, HELPER, AT, Reach::Usable).unwrap();
        assert_eq!(action_state(&conn, action), "cancelled");
        assert_eq!(
            synced_sends::get(&conn, &sync_id).unwrap().unwrap().state,
            SENT
        );
    }

    #[test]
    fn without_flectar_mail_sync_only_the_device_that_scheduled_it_sends_it() {
        let conn = setup("E1");
        let (sync_id, action) = scheduled_here(&conn);
        drive(
            &conn,
            ORIGIN,
            AT + UNREACHABLE_DELAY_MS - 1,
            Reach::Unreachable,
        )
        .unwrap();
        assert_eq!(
            not_before(&conn, action),
            HELD,
            "a device that claimed it may be sending"
        );
        let now = AT + UNREACHABLE_DELAY_MS;
        assert_eq!(
            drive(&conn, ORIGIN, now, Reach::Unreachable).unwrap().nudge,
            vec![1]
        );
        assert_eq!(not_before(&conn, action), now);
        assert!(repo::actions::try_claim(&conn, action).unwrap());

        // Uploads refused until the group pays: no device can claim, so it
        // goes out on time.
        let conn = setup("E1");
        let (_, action) = scheduled_here(&conn);
        drive(&conn, ORIGIN, AT, Reach::PaymentRequired).unwrap();
        assert_eq!(not_before(&conn, action), AT);

        // Another device never sends without a claim.
        let conn = setup("E1");
        let elsewhere = scheduled_elsewhere(&conn, Some("E1"));
        drive(&conn, HELPER, AT - 10 * 60_000, Reach::Usable).unwrap();
        let action = synced_sends::get(&conn, &elsewhere)
            .unwrap()
            .unwrap()
            .action_id
            .unwrap();
        drive(&conn, HELPER, AT + 60 * 60_000, Reach::Unreachable).unwrap();
        assert_eq!(not_before(&conn, action), HELD);
        let _ = sync_id;
    }

    #[test]
    fn turning_sync_off_keeps_own_sends_and_drops_the_others() {
        let conn = setup("E1");
        let (_, own) = scheduled_here(&conn);
        conn.execute(
            "INSERT INTO messages (id, thread_id, account_id, folder_id, message_id, subject,
                                   from_addr, date, is_read, is_draft, jmap_id)
             VALUES (10, 5, 1, 1, 'other@example.com', 'Other', 'me@test.dev', 1, 1, 1, 'E9')",
            [],
        )
        .unwrap();
        let elsewhere = "fedcba9876543210fedcba9876543210";
        put(
            &conn,
            elsewhere,
            &Shared {
                message_id: "other@example.com",
                ..shared(Some("E9"), None)
            },
            0,
        )
        .unwrap();
        drive(&conn, HELPER, AT - 10 * 60_000, Reach::Usable).unwrap();
        let theirs = synced_sends::get(&conn, elsewhere)
            .unwrap()
            .unwrap()
            .action_id
            .unwrap();

        detach(&conn).unwrap();
        assert_eq!(
            not_before(&conn, own),
            AT,
            "its own send goes out at its time"
        );
        assert_eq!(action_state(&conn, theirs), "cancelled");
        assert!(!synced_sends::any(&conn).unwrap());
    }

    #[test]
    fn the_claim_the_server_took_stands_in_a_merge() {
        let base = json!({"sendAt": 1, "state": "scheduled", "claim": null});
        let local =
            json!({"sendAt": 1, "state": "scheduled", "claim": {"device": "aa", "until": 9}});
        let remote =
            json!({"sendAt": 1, "state": "scheduled", "claim": {"device": "bb", "until": 7}});
        // Newer local stamp: the plain merge would mix both claims.
        let mut merged = crate::device_sync::merge::merge(Some(&base), &local, &remote, false);
        settle(Some(&base), &local, &remote, &mut merged);
        assert_eq!(merged["claim"], json!({"device": "bb", "until": 7}));

        // A send that went out stays sent.
        let local = json!({"sendAt": 1, "state": "sent", "error": null, "claim": null});
        let remote = json!({"sendAt": 5, "state": "cancelled", "error": null, "claim": null});
        let mut merged = crate::device_sync::merge::merge(Some(&base), &local, &remote, true);
        settle(Some(&base), &local, &remote, &mut merged);
        assert_eq!(merged["state"], json!("sent"));
    }

    /// A helper with an IMAP account, which keeps its drafts on each device.
    fn imap_helper() -> Connection {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute_batch(
            "UPDATE accounts SET sync_id = '0123456789abcdef0123456789abcdef';
             INSERT INTO folders (id, account_id, path, role) VALUES (3, 1, 'Drafts', 'drafts');",
        )
        .unwrap();
        conn
    }

    const MESSAGE: &[u8] = b"Message-ID: <plans@example.com>\r\nFrom: Me <me@test.dev>\r\n\
        To: ann@example.com\r\nBcc: boss@example.com\r\nSubject: Plans\r\n\
        Date: Mon, 2 Nov 2026 09:00:00 +0000\r\n\r\nSee you on Monday.\r\n";

    /// The draft kept here for another device's message: its id, whether it
    /// is still a draft, and its subject.
    fn shadow(conn: &Connection, sync_id: &str) -> Option<(i64, bool, String)> {
        let id = synced_sends::get(conn, sync_id).unwrap()?.shadow_id?;
        conn.query_row(
            "SELECT id, is_draft, subject FROM messages WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .unwrap()
    }

    #[test]
    fn another_devices_imap_message_waits_here_as_a_draft_and_goes_out_as_it_is() {
        let conn = imap_helper();
        let sync_id = scheduled_elsewhere(&conn, Some("v1"));
        synced_sends::put_mime(&conn, &sync_id, Some(MESSAGE)).unwrap();
        let driven = drive(&conn, HELPER, AT - 10 * 60_000, Reach::Usable).unwrap();
        let (draft, is_draft, subject) = shadow(&conn, &sync_id).expect("kept as a draft");
        assert!(is_draft);
        assert_eq!(subject, "Plans");
        assert_eq!(driven.threads.len(), 1);
        // Its send waits, held, and goes out as the record carries it.
        let action = synced_sends::get(&conn, &sync_id)
            .unwrap()
            .unwrap()
            .action_id
            .expect("a held send");
        assert_eq!(not_before(&conn, action), HELD);
        let payload: String = conn
            .query_row(
                "SELECT payload FROM pending_actions WHERE id = ?1",
                params![action],
                |row| row.get(0),
            )
            .unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["asIs"], true);
        assert_eq!(payload["draftId"], draft);
        assert_eq!(payload["syncedSend"], sync_id.as_str());

        // A new version replaces the draft kept here.
        put(&conn, &sync_id, &shared(Some("v2"), None), 0).unwrap();
        assert_eq!(shadow(&conn, &sync_id), None);
        assert_eq!(action_state(&conn, action), "cancelled");
        let gone: bool = conn
            .query_row(
                "SELECT NOT EXISTS (SELECT 1 FROM messages WHERE id = ?1)",
                params![draft],
                |row| row.get(0),
            )
            .unwrap();
        assert!(gone);
        synced_sends::put_mime(&conn, &sync_id, Some(MESSAGE)).unwrap();
        drive(&conn, HELPER, AT - 10 * 60_000, Reach::Usable).unwrap();
        assert!(shadow(&conn, &sync_id).is_some());

        // Sent from another device: the draft kept here goes too.
        let sent = Shared {
            state: SENT,
            ..shared(Some("v2"), None)
        };
        put(&conn, &sync_id, &sent, 0).unwrap();
        assert_eq!(shadow(&conn, &sync_id), None);
    }

    #[test]
    fn a_message_too_large_to_share_goes_out_from_its_device_alone() {
        let conn = imap_helper();
        let sync_id = scheduled_elsewhere(&conn, Some(LOCAL_ONLY));
        drive(&conn, HELPER, AT, Reach::Usable).unwrap();
        let send = synced_sends::get(&conn, &sync_id).unwrap().unwrap();
        assert_eq!(send.shadow_id, None);
        assert_eq!(send.action_id, None);
    }

    #[test]
    fn the_device_that_scheduled_an_imap_message_keeps_no_copy() {
        let conn = imap_helper();
        let sync_id = scheduled_elsewhere(&conn, Some("v1"));
        synced_sends::put_mime(&conn, &sync_id, Some(MESSAGE)).unwrap();
        drive(&conn, ORIGIN, AT - 10 * 60_000, Reach::Usable).unwrap();
        assert_eq!(shadow(&conn, &sync_id), None);
    }

    #[test]
    fn the_record_carries_an_imap_message() {
        let conn = imap_helper();
        let sync_id = scheduled_elsewhere(&conn, Some("v1"));
        let send = synced_sends::get(&conn, &sync_id).unwrap().unwrap();
        let carried = Carried {
            mime: Some(MESSAGE),
            ..Carried::default()
        };
        let body = body(&send, "0123456789abcdef0123456789abcdef", carried);
        assert_eq!(parse(&body).unwrap().message().as_deref(), Some(MESSAGE));
        assert!(
            parse(&super::body(
                &send,
                "0123456789abcdef0123456789abcdef",
                Carried::default()
            ))
            .unwrap()
            .message()
            .is_none()
        );
    }

    #[test]
    fn a_draft_kept_for_another_device_stays_once_cancelled_here() {
        let conn = imap_helper();
        let sync_id = scheduled_elsewhere(&conn, Some("v1"));
        synced_sends::put_mime(&conn, &sync_id, Some(MESSAGE)).unwrap();
        drive(&conn, HELPER, AT - 10 * 60_000, Reach::Usable).unwrap();
        let (draft, ..) = shadow(&conn, &sync_id).unwrap();
        let action = synced_sends::get(&conn, &sync_id)
            .unwrap()
            .unwrap()
            .action_id
            .unwrap();
        repo::actions::try_cancel(&conn, action).unwrap();
        drive(&conn, HELPER, AT - 9 * 60_000, Reach::Usable).unwrap();
        // Forgotten a week later, the send leaves the draft to the user.
        drive(&conn, HELPER, AT + KEEP_FINISHED_MS + 60_000, Reach::Usable).unwrap();
        assert_eq!(synced_sends::get(&conn, &sync_id).unwrap(), None);
        let kept: bool = conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM messages WHERE id = ?1 AND is_draft = 1)",
                params![draft],
                |row| row.get(0),
            )
            .unwrap();
        assert!(kept);
    }

    #[test]
    fn another_devices_large_message_waits_for_its_file_before_its_send_starts() {
        let conn = imap_helper();
        let sync_id = scheduled_elsewhere(&conn, Some("v1"));
        let preview = crate::mime::preview(MESSAGE, "See you on Monday.");
        let file = FileRef {
            id: [3; 16],
            key: [4; 32],
            size: 9000,
        }
        .to_json();
        synced_sends::put_file(&conn, &sync_id, Some(&file), Some(&preview)).unwrap();

        // Shown from its preview, and held.
        drive(&conn, HELPER, AT - 10 * 60_000, Reach::Usable).unwrap();
        let (_, is_draft, subject) = shadow(&conn, &sync_id).expect("kept as a draft");
        assert!(is_draft);
        assert_eq!(subject, "Plans");
        let action = synced_sends::get(&conn, &sync_id)
            .unwrap()
            .unwrap()
            .action_id
            .expect("a held send");

        // Its claim holds: the file comes first.
        let now = AT - HELPER_LEAD_MS;
        drive(&conn, HELPER, now, Reach::Usable).unwrap();
        server_has(&conn, &sync_id, Some((HELPER, AT + CLAIM_MS)));
        let driven = drive(&conn, HELPER, now, Reach::Usable).unwrap();
        assert_eq!(driven.fetch, vec![sync_id.clone()]);
        assert_eq!(not_before(&conn, action), HELD);

        // Fetched: it goes out at its time.
        synced_sends::put_mime(&conn, &sync_id, Some(MESSAGE)).unwrap();
        let driven = drive(&conn, HELPER, now, Reach::Usable).unwrap();
        assert!(driven.fetch.is_empty());
        assert_eq!(not_before(&conn, action), AT);
    }

    #[test]
    fn a_file_reference_and_preview_travel_in_the_record() {
        let conn = imap_helper();
        let sync_id = scheduled_elsewhere(&conn, Some("v1"));
        let send = synced_sends::get(&conn, &sync_id).unwrap().unwrap();
        let file = FileRef {
            id: [3; 16],
            key: [4; 32],
            size: 9000,
        };
        let json = file.to_json();
        let carried = Carried {
            file: Some(&json),
            preview: Some(b"Subject: Plans\r\n\r\nSee you"),
            ..Carried::default()
        };
        let body = body(&send, "0123456789abcdef0123456789abcdef", carried);
        let record = parse(&body).unwrap();
        assert_eq!(FileRef::from_json(&record.file_ref().unwrap()), Some(file));
        assert_eq!(
            record.preview_bytes().as_deref(),
            Some(&b"Subject: Plans\r\n\r\nSee you"[..])
        );
        assert!(record.message().is_none());
    }
}
