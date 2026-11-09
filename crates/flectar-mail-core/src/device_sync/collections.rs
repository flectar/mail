//! What syncs, and how the app's items become records and back.
//!
//! Each item is one record. Accounts carry their per-account preferences;
//! preferences, signatures and profiles are named records (their record ids
//! derive from their key), templates, snoozes, scheduled sends and reply
//! reminders are records under their own sync id, and each device describes
//! itself in a `device` record under its own device id.

use super::store::{Incoming, RecordId, body_hash, set_local_hash};
use crate::db::repo;
use crate::error::Result;
use crate::models::{
    AccountSettings, MailProfile, MailProtocol, PortableAccountConfig, Settings, Signature,
    SignatureDefaults,
};
use flectar_mail_sync_client::{Entry, Mirrored};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};

pub(crate) const PREF: &str = "pref";
pub(crate) const ACCOUNT: &str = "account";
pub(crate) const SIGNATURE: &str = "signature";
pub(crate) const PROFILE: &str = "profile";
pub(crate) const DEVICE: &str = "device";
pub(crate) const TEMPLATE: &str = "template";
pub(crate) const SNOOZE: &str = "snooze";
pub(crate) const SCHEDULED: &str = "scheduled";
pub(crate) const REMINDER: &str = "reminder";
pub(crate) const SIGN_IN: &str = "signin";

/// The largest template body that syncs, well under the server's record
/// limit: one record too large would stop every upload.
const MAX_TEMPLATE_BYTES: usize = 192 * 1024;

/// The schema this version writes and reads for `collection`, or `None` for
/// a collection it does not know. A version that changes what a record
/// holds raises its collection's schema: records with a newer schema stay
/// unread here, and this device leaves them alone until it updates.
pub(crate) fn schema(collection: &str) -> Option<u32> {
    match collection {
        PREF | ACCOUNT | SIGNATURE | PROFILE | DEVICE | TEMPLATE | SNOOZE | SCHEDULED
        | REMINDER | SIGN_IN => Some(1),
        _ => None,
    }
}

/// Preferences that follow the user to every device: how the app looks and
/// reads, and how mail and contacts behave. Window, notification, sound,
/// battery and storage choices stay per device, as do AI and OAuth app
/// settings, which go with secrets kept on each device.
pub(crate) const SYNCED_PREFERENCES: &[&str] = &[
    "theme",
    "messageAppearance",
    "themePreset",
    "customTheme",
    "showAvatars",
    "groupMailByDate",
    "groupByDate",
    "showTabs",
    "conversationView",
    "workspaceLayout",
    "monochromeSidebarIcons",
    "language",
    "timeFormat",
    "calendarWeekStart",
    "undoSendSeconds",
    "loadRemoteImages",
    "markReadOnOpen",
    "autoAdvance",
    "selectAdvance",
    "autoLabelsEnabled",
    "contactSuggestAllAccounts",
    "collectOutgoingContacts",
    "collectIncomingContacts",
    "suggestLearnedContacts",
    "showAccountBadges",
    "syncSignIns",
];

/// Record ids of named items, derived by the sync client from the group's
/// keys, so they are the same on every device.
#[derive(Debug, Default, Clone)]
pub(crate) struct Names {
    ids: HashMap<String, RecordId>,
}

impl Names {
    pub fn insert(&mut self, name: String, id: RecordId) {
        self.ids.insert(name, id);
    }

    pub fn get(&self, name: &str) -> Option<RecordId> {
        self.ids.get(name).copied()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.ids.contains_key(name)
    }
}

pub(crate) fn pref_name(key: &str) -> String {
    format!("{PREF}:{key}")
}

pub(crate) fn signature_name(id: &str) -> String {
    format!("{SIGNATURE}:{id}")
}

pub(crate) fn profile_name(id: &str) -> String {
    format!("{PROFILE}:{id}")
}

/// Every name the app's items currently need an id for.
pub(crate) fn names_needed(settings: &Settings) -> Vec<String> {
    SYNCED_PREFERENCES
        .iter()
        .map(|key| pref_name(key))
        .chain(
            settings
                .signature_list
                .iter()
                .map(|s| signature_name(&s.id)),
        )
        .chain(settings.mail_profiles.iter().map(|p| profile_name(&p.id)))
        .collect()
}

pub(crate) fn sync_id_bytes(sync_id: &str) -> Option<RecordId> {
    if sync_id.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(sync_id.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn sync_id_hex(id: &RecordId) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Local account ids and their sync ids, both ways.
struct AccountIds {
    by_local: HashMap<i64, String>,
    by_sync: HashMap<String, i64>,
}

fn account_ids(conn: &Connection) -> Result<AccountIds> {
    let mut statement =
        conn.prepare("SELECT id, sync_id FROM accounts WHERE sync_id IS NOT NULL")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut ids = AccountIds {
        by_local: HashMap::new(),
        by_sync: HashMap::new(),
    };
    for row in rows {
        let (local, sync) = row?;
        ids.by_local.insert(local, sync.clone());
        ids.by_sync.insert(sync, local);
    }
    Ok(ids)
}

fn entry(collection: &str, body: Value) -> Entry {
    Entry {
        collection: collection.to_owned(),
        schema: schema(collection).unwrap_or_default(),
        body,
    }
}

/// The account's synced settings: everything but how much mail this device
/// keeps, for POP3 what it leaves on the server, and its OpenPGP settings,
/// whose keys only this device has.
fn account_body(
    config: &crate::models::AccountConfig,
    settings: &Settings,
    sort_order: i64,
) -> Result<Value> {
    let mut body = serde_json::to_value(PortableAccountConfig::from(config))?;
    if let Some(account_settings) = body.get_mut("settings").and_then(Value::as_object_mut) {
        account_settings.remove("mailHistory");
        account_settings.remove("keepOnServer");
        account_settings.remove("security");
    }
    let key = config.id.to_string();
    let object = body
        .as_object_mut()
        .expect("an account config serializes to an object");
    if let Some(color) = settings.account_colors.get(&key) {
        object.insert("color".into(), json!(color));
    }
    if let Some(name) = settings.account_short_names.get(&key) {
        object.insert("shortName".into(), json!(name));
    }
    if let Some(theme) = settings.account_themes.get(&key) {
        object.insert("theme".into(), json!(theme));
    }
    if let Some(defaults) = settings.signature_defaults.get(&key) {
        object.insert("signatureDefaults".into(), serde_json::to_value(defaults)?);
    }
    object.insert("sortOrder".into(), json!(sort_order));
    Ok(body)
}

/// The app's synced items as records, with this device's own record.
/// `mirror` holds what the server has: references this device cannot follow
/// yet, such as a profile's account that has not arrived, are kept from it.
pub(crate) fn snapshot(
    conn: &Connection,
    names: &Names,
    mirror: &BTreeMap<RecordId, Mirrored>,
    device: (RecordId, Value),
) -> Result<BTreeMap<RecordId, Entry>> {
    let settings = repo::settings::get(conn)?;
    let ids = account_ids(conn)?;
    let mut records = BTreeMap::new();

    let settings_json = serde_json::to_value(&settings)?;
    for key in SYNCED_PREFERENCES {
        if let (Some(id), Some(value)) = (names.get(&pref_name(key)), settings_json.get(*key)) {
            records.insert(id, entry(PREF, json!({"key": key, "value": value})));
        }
    }

    let sort_orders: HashMap<i64, i64> = {
        let mut statement = conn.prepare("SELECT id, sort_order FROM accounts")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for config in repo::accounts::list_configs(conn)? {
        if config.mail_protocol == MailProtocol::Local {
            continue;
        }
        let Some(id) = ids.by_local.get(&config.id).and_then(|s| sync_id_bytes(s)) else {
            continue;
        };
        let sort_order = sort_orders.get(&config.id).copied().unwrap_or_default();
        records.insert(
            id,
            entry(ACCOUNT, account_body(&config, &settings, sort_order)?),
        );
    }

    for signature in &settings.signature_list {
        let (Some(id), Some(account)) = (
            names.get(&signature_name(&signature.id)),
            ids.by_local.get(&signature.account_id),
        ) else {
            continue;
        };
        records.insert(
            id,
            entry(
                SIGNATURE,
                json!({
                    "id": signature.id,
                    "account": account,
                    "name": signature.name,
                    "html": signature.html,
                }),
            ),
        );
    }

    for profile in &settings.mail_profiles {
        let Some(id) = names.get(&profile_name(&profile.id)) else {
            continue;
        };
        let accounts = profile_accounts(
            mirror.get(&id).and_then(|mirrored| mirrored.entry.as_ref()),
            profile
                .account_ids
                .iter()
                .filter_map(|account| ids.by_local.get(account)),
            &ids,
        );
        records.insert(
            id,
            entry(
                PROFILE,
                json!({
                    "id": profile.id,
                    "name": profile.name,
                    "color": profile.color,
                    "accounts": accounts,
                }),
            ),
        );
    }

    for (sync_id, body) in template_bodies(conn)? {
        if let Some(id) = sync_id_bytes(&sync_id) {
            records.insert(id, entry(TEMPLATE, body));
        }
    }

    let mut snoozes = conn.prepare(
        "SELECT s.sync_id, a.sync_id, s.anchor, s.wake_at FROM synced_snoozes s
         JOIN accounts a ON a.id = s.account_id WHERE a.sync_id IS NOT NULL",
    )?;
    let rows = snoozes.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    for row in rows {
        let (sync_id, account, anchor, wake_at) = row?;
        let (Some(id), Ok(anchor)) = (
            sync_id_bytes(&sync_id),
            serde_json::from_str::<Value>(&anchor),
        ) else {
            continue;
        };
        records.insert(
            id,
            entry(
                SNOOZE,
                json!({"account": account, "anchor": anchor, "wakeAt": wake_at}),
            ),
        );
    }

    for send in repo::synced_sends::all(conn)? {
        let (Some(id), Some(account)) = (
            sync_id_bytes(&send.sync_id),
            ids.by_local.get(&send.account_id),
        ) else {
            continue;
        };
        let (file, preview) = repo::synced_sends::file(conn, &send.sync_id)?;
        // A message in a file never goes into the record, even once fetched
        // here, and nothing larger than a record holds ever does: one record
        // too large would stop every upload.
        let mime = repo::synced_sends::mime(conn, &send.sync_id)?.filter(|mime| {
            file.is_none() && mime.len() <= super::scheduled::MAX_SHARED_MESSAGE_BYTES
        });
        let carried = super::scheduled::Carried {
            mime: mime.as_deref(),
            file: file.as_deref(),
            preview: preview.as_deref(),
        };
        records.insert(
            id,
            entry(SCHEDULED, super::scheduled::body(&send, account, carried)),
        );
    }

    // When it came due stays on each device: every device sees for itself
    // that nobody replied.
    for reminder in repo::reply_reminders::all(conn)? {
        let (Some(id), Some(account)) = (
            sync_id_bytes(&reminder.sync_id),
            ids.by_local.get(&reminder.account_id),
        ) else {
            continue;
        };
        records.insert(
            id,
            entry(
                REMINDER,
                json!({
                    "account": account,
                    "messageId": reminder.message_id,
                    "remindAt": reminder.remind_at,
                }),
            ),
        );
    }

    // Passwords and sign-ins, sealed for the group (sign_ins.rs). Turned
    // off, they go from every device.
    if settings.sync_sign_ins {
        for share in repo::sign_ins::shares(conn)? {
            let (Some(id), Some(account)) = (share.record_id, ids.by_local.get(&share.account_id))
            else {
                continue;
            };
            if let Some(body) = super::sign_ins::body(&share, account) {
                records.insert(id, entry(SIGN_IN, body));
            }
        }
    }

    records.insert(device.0, entry(DEVICE, device.1));
    Ok(records)
}

/// Templates as record bodies, by sync id. How often each is used stays on
/// this device: counting every use would upload it each time.
fn template_bodies(conn: &Connection) -> Result<Vec<(String, Value)>> {
    let mut statement = conn.prepare(
        "SELECT sync_id, name, shortcut, subject, body_text FROM snippets
         WHERE sync_id IS NOT NULL ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            json!({
                "name": row.get::<_, String>(1)?,
                "shortcut": row.get::<_, Option<String>>(2)?,
                "subject": row.get::<_, Option<String>>(3)?,
                "bodyText": row.get::<_, String>(4)?,
            }),
        ))
    })?;
    let mut bodies = Vec::new();
    for row in rows {
        let (sync_id, body) = row?;
        if body.to_string().len() > MAX_TEMPLATE_BYTES {
            tracing::debug!("a template is too large to sync");
            continue;
        }
        bodies.push((sync_id, body));
    }
    Ok(bodies)
}

/// A profile's accounts by sync id: the ones this device has, plus the
/// mirrored record's accounts it does not have yet, so applying a profile
/// before all its accounts arrived never takes them out of it. Keeps the
/// mirrored order, so an unchanged profile reproduces the mirrored body.
fn profile_accounts<'a>(
    mirrored: Option<&Entry>,
    local: impl Iterator<Item = &'a String>,
    ids: &AccountIds,
) -> Vec<String> {
    let local: Vec<&str> = local.map(String::as_str).collect();
    let mut accounts: Vec<String> = mirrored
        .filter(|entry| entry.collection == PROFILE)
        .and_then(|entry| entry.body.get("accounts"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|account| !ids.by_sync.contains_key(*account) || local.contains(account))
        .map(str::to_owned)
        .collect();
    for account in local {
        if !accounts.iter().any(|kept| kept == account) {
            accounts.push(account.to_owned());
        }
    }
    accounts
}

/// Work the app must do after the transaction that applied records.
#[derive(Debug)]
pub(crate) enum FollowUp {
    /// Another device removed the account: the local id, and the sync id it
    /// had, which must still be its own when the removal runs.
    RemoveAccount(i64, String),
    /// Another device changed how the account connects.
    RestartAccount(i64),
}

/// What became of a record this device was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Taken on: a later change of it here is a local edit.
    Applied,
    /// Needs something that has not arrived yet, such as its account; a
    /// later run tries again.
    Waiting,
    /// Nothing to take on here: another device's own record, or one this
    /// version cannot use. Never treated as this device's item.
    Ignored,
}

/// What applying records changed.
#[derive(Debug, Default)]
pub(crate) struct Applied {
    pub settings_changed: bool,
    pub accounts_changed: bool,
    pub follow_ups: Vec<FollowUp>,
    /// Conversations whose state changed here, such as one another device
    /// snoozed: the mail lists show them again.
    pub threads_changed: Vec<i64>,
    /// Records taken on or left alone: see [`settle`].
    pub settled: Vec<RecordId>,
}

fn readable(entry: &Entry) -> bool {
    schema(&entry.collection).is_some_and(|known| entry.schema <= known)
}

/// Collections that other records refer to come first, and within one,
/// changes before deletions: an account that moved to another device's id
/// is adopted before the id it leaves is deleted.
fn order(collection: &str, deleted: bool) -> (u8, bool) {
    let rank = match collection {
        ACCOUNT => 0,
        PREF | SIGNATURE | PROFILE | TEMPLATE | SNOOZE | SCHEDULED | REMINDER => 1,
        _ => 2,
    };
    (rank, deleted)
}

/// Apply records to the app: `incoming` from the run, plus every mirrored
/// record this device has not taken on yet, such as one that waited for its
/// account. `local_before` is the app's state when the run started: an item
/// edited here since then keeps the edit, which the next run uploads.
pub(crate) fn apply(
    conn: &Connection,
    incoming: &BTreeMap<RecordId, Incoming>,
    pending_mirror: &BTreeMap<RecordId, Entry>,
    local_before: &BTreeMap<RecordId, Entry>,
    local_now: &BTreeMap<RecordId, Entry>,
) -> Result<Applied> {
    let mut applied = Applied::default();
    let mut settings = repo::settings::get(conn)?;
    let settings_before = serde_json::to_value(&settings)?;

    let mut work: Vec<(RecordId, Option<&Entry>, Option<&Entry>)> = incoming
        .iter()
        .map(|(id, change)| (*id, change.previous.as_ref(), change.next.as_ref()))
        .chain(
            pending_mirror
                .iter()
                .filter(|(id, _)| !incoming.contains_key(*id))
                .map(|(id, entry)| (*id, None, Some(entry))),
        )
        .collect();
    work.sort_by_key(|(_, previous, next)| {
        order(
            next.or(*previous)
                .map_or("", |entry| entry.collection.as_str()),
            next.is_none(),
        )
    });

    for (id, previous, next) in work {
        // Edited here during the run: keep the edit.
        if local_before.get(&id) != local_now.get(&id) {
            continue;
        }
        let outcome = match (previous, next) {
            (_, Some(next)) if !readable(next) => Outcome::Waiting,
            (_, Some(next)) => apply_put(conn, &mut settings, &id, next, &mut applied)?,
            (Some(previous), None) => {
                apply_delete(conn, &mut settings, &id, previous, &mut applied)?;
                Outcome::Applied
            }
            // Deleted while this device was out of the group: its own copy
            // goes too, rather than coming back for every device.
            (None, None) => {
                if let Some(own) = local_before.get(&id) {
                    apply_delete(conn, &mut settings, &id, own, &mut applied)?;
                }
                Outcome::Applied
            }
        };
        match outcome {
            Outcome::Applied => set_local_hash(conn, &id, next.map(body_hash))?,
            // Not this device's item: it never uploads or deletes it.
            Outcome::Ignored => set_local_hash(conn, &id, None)?,
            Outcome::Waiting => {}
        }
        if outcome != Outcome::Waiting {
            applied.settled.push(id);
        }
    }

    if serde_json::to_value(&settings)? != settings_before {
        repo::settings::set(conn, &settings)?;
        applied.settings_changed = true;
    }
    // Snoozes and reply reminders that waited for their conversation's mail.
    applied
        .threads_changed
        .extend(repo::snoozes::attach_waiting(
            conn,
            crate::models::now_ms(),
        )?);
    applied
        .threads_changed
        .extend(repo::reply_reminders::attach_waiting(conn)?);
    Ok(applied)
}

fn apply_put(
    conn: &Connection,
    settings: &mut Settings,
    id: &RecordId,
    entry: &Entry,
    applied: &mut Applied,
) -> Result<Outcome> {
    match entry.collection.as_str() {
        PREF => {
            let (Some(key), Some(value)) = (
                entry.body.get("key").and_then(Value::as_str),
                entry.body.get("value"),
            ) else {
                return Ok(Outcome::Ignored);
            };
            if !SYNCED_PREFERENCES.contains(&key) {
                return Ok(Outcome::Ignored);
            }
            let mut object = serde_json::to_value(&*settings)?;
            if let Some(fields) = object.as_object_mut() {
                fields.insert(key.to_owned(), value.clone());
            }
            match serde_json::from_value::<Settings>(object) {
                Ok(updated) => {
                    *settings = updated;
                    Ok(Outcome::Applied)
                }
                Err(error) => {
                    tracing::warn!(%error, key, "a synced preference does not fit this version");
                    Ok(Outcome::Ignored)
                }
            }
        }
        ACCOUNT => apply_account(conn, settings, id, &entry.body, applied),
        SIGNATURE => {
            let ids = account_ids(conn)?;
            let (Some(signature_id), Some(account), Some(name), Some(html)) = (
                entry.body.get("id").and_then(Value::as_str),
                entry.body.get("account").and_then(Value::as_str),
                entry.body.get("name").and_then(Value::as_str),
                entry.body.get("html").and_then(Value::as_str),
            ) else {
                return Ok(Outcome::Ignored);
            };
            // Its account has not arrived yet; a later run applies it.
            let Some(account_id) = ids.by_sync.get(account).copied() else {
                return Ok(Outcome::Waiting);
            };
            let signature = Signature {
                id: signature_id.to_owned(),
                account_id,
                name: name.to_owned(),
                html: crate::signatures::sanitize_html(html),
            };
            match settings
                .signature_list
                .iter_mut()
                .find(|existing| existing.id == signature.id)
            {
                Some(existing) => *existing = signature,
                None => settings.signature_list.push(signature),
            }
            Ok(Outcome::Applied)
        }
        PROFILE => {
            let ids = account_ids(conn)?;
            let (Some(profile_id), Some(name), Some(color)) = (
                entry.body.get("id").and_then(Value::as_str),
                entry.body.get("name").and_then(Value::as_str),
                entry.body.get("color").and_then(Value::as_str),
            ) else {
                return Ok(Outcome::Ignored);
            };
            let account_ids: Vec<i64> = entry
                .body
                .get("accounts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|account| ids.by_sync.get(account.as_str()?).copied())
                .collect();
            let profile = MailProfile {
                id: profile_id.to_owned(),
                name: name.to_owned(),
                color: color.to_owned(),
                account_ids,
            };
            // An account belongs to one profile at most.
            for other in settings
                .mail_profiles
                .iter_mut()
                .filter(|p| p.id != profile.id)
            {
                other
                    .account_ids
                    .retain(|account| !profile.account_ids.contains(account));
            }
            match settings
                .mail_profiles
                .iter_mut()
                .find(|existing| existing.id == profile.id)
            {
                Some(existing) => *existing = profile,
                None => settings.mail_profiles.push(profile),
            }
            Ok(Outcome::Applied)
        }
        TEMPLATE => apply_template(conn, id, &entry.body),
        SNOOZE => apply_snooze(conn, id, &entry.body, applied),
        SCHEDULED => apply_scheduled(conn, id, &entry.body, applied),
        REMINDER => apply_reminder(conn, id, &entry.body, applied),
        SIGN_IN => apply_sign_in(conn, id, &entry.body),
        // Other devices describe themselves; the app reads them from the
        // mirror, and only each device writes its own.
        DEVICE => Ok(Outcome::Ignored),
        _ => Ok(Outcome::Ignored),
    }
}

fn apply_account(
    conn: &Connection,
    settings: &mut Settings,
    id: &RecordId,
    body: &Value,
    applied: &mut Applied,
) -> Result<Outcome> {
    let config: PortableAccountConfig = match serde_json::from_value(body.clone()) {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(%error, "a synced account does not fit this version");
            return Ok(Outcome::Ignored);
        }
    };
    if crate::validate_portable_account(&config).is_err() {
        return Ok(Outcome::Ignored);
    }
    let sync_id = sync_id_hex(id);
    let mut local = conn
        .query_row(
            "SELECT id FROM accounts WHERE sync_id = ?1",
            params![sync_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if local.is_none() {
        // The same mailbox added on two devices before they synced: they
        // become one account, under one id every device agrees on.
        if let Some(existing) = repo::accounts::find_by_mailbox(conn, config.email.trim())? {
            // The same mailbox over another protocol: this device keeps its
            // own account, and the other device its own.
            if (
                existing.provider,
                existing.auth_kind,
                existing.mail_protocol,
            ) != (config.provider, config.auth_kind, config.mail_protocol)
            {
                return Ok(Outcome::Ignored);
            }
            let own: Option<String> = conn.query_row(
                "SELECT sync_id FROM accounts WHERE id = ?1",
                params![existing.id],
                |row| row.get(0),
            )?;
            // An id the server already has stays unless the other one is
            // smaller: the device with the larger id adopts the smaller and
            // deletes its own, so both end with one account.
            let keep_own = own.as_deref().is_some_and(|own| {
                own < sync_id.as_str() && server_knows(conn, own).unwrap_or(true)
            });
            if keep_own {
                return Ok(Outcome::Ignored);
            }
            conn.execute(
                "UPDATE accounts SET sync_id = ?1 WHERE id = ?2",
                params![sync_id, existing.id],
            )?;
            local = Some(existing.id);
        }
    }
    let local_id = match local {
        Some(local_id) => {
            let Some(current) = repo::accounts::get_config(conn, local_id)? else {
                return Ok(Outcome::Ignored);
            };
            if (current.provider, current.auth_kind, current.mail_protocol)
                != (config.provider, config.auth_kind, config.mail_protocol)
            {
                tracing::warn!("a synced account changed protocol; keeping this device's");
                return Ok(Outcome::Ignored);
            }
            let settings_json = merged_account_settings(&current.settings, &config.settings);
            let reconnect = current.username != config.username
                || current.jmap_url != config.jmap_url
                || current.incoming_host != config.incoming_host
                || current.incoming_port != config.incoming_port
                || current.smtp_host != config.smtp_host
                || current.smtp_port != config.smtp_port
                || current.settings.connection != config.settings.connection;
            conn.execute(
                "UPDATE accounts SET display_name = ?1, username = ?2, jmap_url = ?3,
                        incoming_host = ?4, incoming_port = ?5, smtp_host = ?6, smtp_port = ?7,
                        settings_json = ?8
                 WHERE id = ?9",
                params![
                    config.display_name,
                    config.username.trim(),
                    config.jmap_url.trim(),
                    config.incoming_host.trim(),
                    config.incoming_port,
                    config.smtp_host.trim(),
                    config.smtp_port,
                    serde_json::to_string(&settings_json)?,
                    local_id
                ],
            )?;
            if reconnect {
                applied.follow_ups.push(FollowUp::RestartAccount(local_id));
            }
            local_id
        }
        None => {
            // A new POP3 account keeps all mail on the server until the user
            // chooses otherwise here: another device may still need it.
            let mut account_settings = config.settings.clone();
            account_settings.keep_on_server = Default::default();
            // Signing and encrypting need this device's own keys.
            account_settings.security = Default::default();
            let local_id = repo::accounts::insert_with_sync_state(
                conn,
                &repo::accounts::NewAccount {
                    email: config.email.trim(),
                    display_name: config.display_name.as_deref(),
                    avatar_url: None,
                    provider: config.provider,
                    auth_kind: config.auth_kind,
                    mail_protocol: config.mail_protocol,
                    username: config.username.trim(),
                    jmap_url: config.jmap_url.trim(),
                    jmap_account_id: None,
                    incoming_host: config.incoming_host.trim(),
                    incoming_port: config.incoming_port,
                    smtp_host: config.smtp_host.trim(),
                    smtp_port: config.smtp_port,
                },
                &account_settings,
                // Signed out until its password or sign-in arrives, or the
                // user signs in here.
                "needs_reauth",
            )?;
            conn.execute(
                "UPDATE accounts SET sync_id = ?1 WHERE id = ?2",
                params![sync_id, local_id],
            )?;
            local_id
        }
    };
    if let Some(sort_order) = body.get("sortOrder").and_then(Value::as_i64) {
        conn.execute(
            "UPDATE accounts SET sort_order = ?1 WHERE id = ?2",
            params![sort_order, local_id],
        )?;
    }
    let key = local_id.to_string();
    let text = |field: &str| body.get(field).and_then(Value::as_str).map(str::to_owned);
    set_or_remove(&mut settings.account_colors, &key, text("color"));
    set_or_remove(&mut settings.account_short_names, &key, text("shortName"));
    set_or_remove(&mut settings.account_themes, &key, text("theme"));
    match body
        .get("signatureDefaults")
        .and_then(|value| serde_json::from_value::<SignatureDefaults>(value.clone()).ok())
    {
        Some(defaults) => {
            settings.signature_defaults.insert(key, defaults);
        }
        None => {
            settings.signature_defaults.remove(&key);
        }
    }
    applied.accounts_changed = true;
    Ok(Outcome::Applied)
}

/// Create or update a template from another device. Shortcuts are unique:
/// when two templates claim one, the one with the smaller sync id keeps it,
/// on every device.
fn apply_template(conn: &Connection, id: &RecordId, body: &Value) -> Result<Outcome> {
    let (Some(name), Some(body_text)) = (
        body.get("name").and_then(Value::as_str),
        body.get("bodyText").and_then(Value::as_str),
    ) else {
        return Ok(Outcome::Ignored);
    };
    let subject = body.get("subject").and_then(Value::as_str);
    let mut shortcut = body
        .get("shortcut")
        .and_then(Value::as_str)
        .filter(|shortcut| !shortcut.is_empty());
    let sync_id = sync_id_hex(id);
    if let Some(wanted) = shortcut {
        let holder: Option<(i64, Option<String>)> = conn
            .query_row(
                "SELECT id, sync_id FROM snippets
                 WHERE shortcut = ?1 AND (sync_id IS NULL OR sync_id <> ?2)",
                params![wanted, sync_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((holder, holder_sync_id)) = holder {
            if holder_sync_id.is_some_and(|other| other < sync_id) {
                shortcut = None;
            } else {
                conn.execute(
                    "UPDATE snippets SET shortcut = NULL WHERE id = ?1",
                    params![holder],
                )?;
            }
        }
    }
    let updated = conn.execute(
        "UPDATE snippets SET name = ?2, shortcut = ?3, subject = ?4, body_text = ?5
         WHERE sync_id = ?1",
        params![sync_id, name, shortcut, subject, body_text],
    )?;
    if updated == 0 {
        conn.execute(
            "INSERT INTO snippets (name, shortcut, subject, body_text, sync_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![name, shortcut, subject, body_text, sync_id],
        )?;
    }
    Ok(Outcome::Applied)
}

/// Snooze a conversation as another device did. The shared snooze is kept
/// even before the conversation's mail arrives here; it snoozes it then.
fn apply_snooze(
    conn: &Connection,
    id: &RecordId,
    body: &Value,
    applied: &mut Applied,
) -> Result<Outcome> {
    let (Some(account), Some(anchor), Some(wake_at)) = (
        body.get("account").and_then(Value::as_str),
        body.get("anchor").filter(|anchor| anchor.is_object()),
        body.get("wakeAt").and_then(Value::as_i64),
    ) else {
        return Ok(Outcome::Ignored);
    };
    let Some(account_id) = account_ids(conn)?.by_sync.get(account).copied() else {
        return Ok(Outcome::Waiting);
    };
    // Already over: taken on as gone, so this device removes it everywhere.
    if wake_at <= crate::models::now_ms() {
        return Ok(Outcome::Applied);
    }
    let sync_id = sync_id_hex(id);
    conn.execute(
        "INSERT INTO synced_snoozes (sync_id, account_id, anchor, wake_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(sync_id) DO UPDATE SET
           account_id = excluded.account_id, anchor = excluded.anchor,
           wake_at = excluded.wake_at",
        params![sync_id, account_id, anchor.to_string(), wake_at],
    )?;
    if let Some(thread) = repo::snoozes::attach(conn, &sync_id, account_id, anchor, wake_at)? {
        applied.threads_changed.push(thread);
    }
    Ok(Outcome::Applied)
}

/// Take on a reply reminder another device set or moved. It is kept before
/// its message arrives here, and its conversation comes back then if nobody
/// replied.
/// Another device's password or sign-in: kept sealed until the install step
/// moves it into the credential store (sign_ins.rs).
fn apply_sign_in(conn: &Connection, id: &RecordId, body: &Value) -> Result<Outcome> {
    let Some(record) = super::sign_ins::parse(body) else {
        return Ok(Outcome::Ignored);
    };
    let Some(account_id) = account_ids(conn)?.by_sync.get(record.account).copied() else {
        return Ok(Outcome::Waiting);
    };
    let kept = repo::sign_ins::share_of_record(conn, id)?.is_some();
    match record.sealed {
        Some(sealed) => {
            // Only a device of the kind it was made for takes it on.
            if super::sign_ins::works_here(conn, account_id, record.kind, record.family)? {
                repo::sign_ins::put_arrival(
                    conn,
                    &repo::sign_ins::Arrival {
                        account_id,
                        kind: record.kind.as_str().into(),
                        family: record.family.into(),
                        record_id: *id,
                        sealed: sealed.clone(),
                    },
                )?;
            }
            if kept {
                repo::sign_ins::follow_record(conn, id, &sealed)?;
            }
        }
        // It stopped working on another device: this one says so too, and
        // one whose sign-in works shares that one instead.
        None if kept => {
            repo::sign_ins::follow_stale(conn, id, record.rejected.as_deref())?;
        }
        None => {}
    }
    // Only a record this device keeps a share of is its item. It never
    // uploads or deletes the others: those of other kinds of device, and
    // those it hasn't taken on yet.
    Ok(if kept {
        Outcome::Applied
    } else {
        Outcome::Ignored
    })
}

fn apply_reminder(
    conn: &Connection,
    id: &RecordId,
    body: &Value,
    applied: &mut Applied,
) -> Result<Outcome> {
    let (Some(account), Some(message_id), Some(remind_at)) = (
        body.get("account").and_then(Value::as_str),
        body.get("messageId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty()),
        body.get("remindAt").and_then(Value::as_i64),
    ) else {
        return Ok(Outcome::Ignored);
    };
    let Some(account_id) = account_ids(conn)?.by_sync.get(account).copied() else {
        return Ok(Outcome::Waiting);
    };
    if let Some(thread) = repo::reply_reminders::put_shared(
        conn,
        &sync_id_hex(id),
        account_id,
        message_id,
        remind_at,
    )? {
        applied.threads_changed.push(thread);
    }
    Ok(Outcome::Applied)
}

/// Take on a send another device scheduled, claimed or finished.
fn apply_scheduled(
    conn: &Connection,
    id: &RecordId,
    body: &Value,
    applied: &mut Applied,
) -> Result<Outcome> {
    let Some(record) = super::scheduled::parse(body) else {
        return Ok(Outcome::Ignored);
    };
    let Some(account_id) = account_ids(conn)?.by_sync.get(record.account).copied() else {
        return Ok(Outcome::Waiting);
    };
    let shared = repo::synced_sends::Shared {
        account_id,
        message_id: record.message_id,
        send_at: record.send_at,
        origin: record.origin,
        draft_ref: record.draft,
        state: record.state,
        error: record.error,
        claimed_by: record.claim.map(|(device, _)| device),
        claim_until: record.claim.map(|(_, until)| until),
    };
    let sync_id = sync_id_hex(id);
    if let Some(thread) = super::scheduled::put(conn, &sync_id, &shared, crate::models::now_ms())? {
        applied.threads_changed.push(thread);
    }
    // An IMAP or POP3 message travels with its schedule, or in a file. A
    // message in a file stays here once fetched, while the record names that
    // file: another one holds the message as it was edited since.
    let file = record.file_ref();
    let (fetched_from, _) = repo::synced_sends::file(conn, &sync_id)?;
    match record.message() {
        Some(mime) => repo::synced_sends::put_mime(conn, &sync_id, Some(&mime))?,
        None if file.is_some() && file == fetched_from => {}
        None => repo::synced_sends::put_mime(conn, &sync_id, None)?,
    }
    repo::synced_sends::put_file(
        conn,
        &sync_id,
        file.as_deref(),
        record.preview_bytes().as_deref(),
    )?;
    Ok(Outcome::Applied)
}

/// Whether the server has seen the record with this sync id: it is mirrored,
/// even as a deletion, or waiting to upload.
fn server_knows(conn: &Connection, sync_id: &str) -> Result<bool> {
    let Some(id) = sync_id_bytes(sync_id) else {
        return Ok(false);
    };
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM device_sync_mirror WHERE record_id = ?1)
             OR EXISTS (SELECT 1 FROM device_sync_outbox WHERE record_id = ?1)",
        params![&id[..]],
        |row| row.get(0),
    )?)
}

fn set_or_remove(
    map: &mut std::collections::HashMap<String, String>,
    key: &str,
    value: Option<String>,
) {
    match value {
        Some(value) => {
            map.insert(key.to_owned(), value);
        }
        None => {
            map.remove(key);
        }
    }
}

/// The synced account settings over this device's own mail history, POP3
/// retention and OpenPGP settings.
fn merged_account_settings(current: &AccountSettings, synced: &AccountSettings) -> AccountSettings {
    AccountSettings {
        mail_history: current.mail_history,
        keep_on_server: current.keep_on_server,
        security: current.security.clone(),
        ..synced.clone()
    }
}

/// Whether the account is POP3 and removes downloaded mail from the
/// server, so some of its mail exists only on this device.
fn keeps_mail_only_here(conn: &Connection, account_id: i64) -> Result<bool> {
    use crate::models::KeepOnServer;
    Ok(
        repo::accounts::get_config(conn, account_id)?.is_some_and(|config| {
            config.mail_protocol == MailProtocol::Pop3
                && matches!(
                    config.settings.keep_on_server,
                    KeepOnServer::SevenDays
                        | KeepOnServer::FourteenDays
                        | KeepOnServer::ThirtyDays
                        | KeepOnServer::UntilDownloaded
                )
        }),
    )
}

/// After a run's records are applied: each record taken on or left alone
/// keeps the hash of the item as this device now has it, from `local`, a
/// snapshot taken after applying. An item this device keeps its own way,
/// such as an account it uses over another protocol, is then not sent back
/// over the other device's, and one it can't show yet isn't deleted for
/// missing.
pub(crate) fn settle(
    conn: &Connection,
    settled: &[RecordId],
    local: &BTreeMap<RecordId, Entry>,
) -> Result<()> {
    for id in settled {
        set_local_hash(conn, id, local.get(id).map(body_hash))?;
    }
    Ok(())
}

fn apply_delete(
    conn: &Connection,
    settings: &mut Settings,
    id: &RecordId,
    previous: &Entry,
    applied: &mut Applied,
) -> Result<()> {
    match previous.collection.as_str() {
        ACCOUNT => {
            if let Some(local_id) = conn
                .query_row(
                    "SELECT id FROM accounts WHERE sync_id = ?1",
                    params![sync_id_hex(id)],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
            {
                if keeps_mail_only_here(conn, local_id)? {
                    // Its mail left the mail server, so only this device has
                    // it: the account stays here, synced no more.
                    conn.execute(
                        "UPDATE accounts SET sync_id = NULL WHERE id = ?1",
                        params![local_id],
                    )?;
                } else {
                    applied
                        .follow_ups
                        .push(FollowUp::RemoveAccount(local_id, sync_id_hex(id)));
                }
                applied.accounts_changed = true;
            }
        }
        SIGNATURE => {
            if let Some(signature_id) = previous.body.get("id").and_then(Value::as_str) {
                settings
                    .signature_list
                    .retain(|signature| signature.id != signature_id);
                for defaults in settings.signature_defaults.values_mut() {
                    if defaults.new_id.as_deref() == Some(signature_id) {
                        defaults.new_id = None;
                    }
                    if defaults.reply_id.as_deref() == Some(signature_id) {
                        defaults.reply_id = None;
                    }
                }
            }
        }
        PROFILE => {
            if let Some(profile_id) = previous.body.get("id").and_then(Value::as_str) {
                settings
                    .mail_profiles
                    .retain(|profile| profile.id != profile_id);
                if settings.active_mail_profile.as_deref() == Some(profile_id) {
                    settings.active_mail_profile = None;
                }
            }
        }
        TEMPLATE => {
            conn.execute(
                "DELETE FROM snippets WHERE sync_id = ?1",
                params![sync_id_hex(id)],
            )?;
        }
        SNOOZE => {
            let sync_id = sync_id_hex(id);
            let thread: Option<i64> = conn
                .query_row(
                    "SELECT thread_id FROM synced_snoozes WHERE sync_id = ?1",
                    params![sync_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            conn.execute(
                "DELETE FROM synced_snoozes WHERE sync_id = ?1",
                params![sync_id],
            )?;
            if let Some(thread) = thread {
                // Unsnoozed, archived or woken on another device.
                repo::snoozes::clear_local(conn, thread)?;
                let woke_at = previous
                    .body
                    .get("wakeAt")
                    .and_then(Value::as_i64)
                    .filter(|wake_at| *wake_at <= crate::models::now_ms() + 60_000);
                if let Some(wake_at) = woke_at {
                    repo::snoozes::mark_woken(conn, thread, wake_at)?;
                }
                applied.threads_changed.push(thread);
            }
        }
        SCHEDULED => {
            if let Some(thread) = super::scheduled::remove(conn, &sync_id_hex(id))? {
                applied.threads_changed.push(thread);
            }
        }
        // Dismissed, or answered, on another device.
        REMINDER => {
            if let Some(thread) = repo::reply_reminders::delete(conn, &sync_id_hex(id))? {
                applied.threads_changed.push(thread);
            }
        }
        // Turned off, or its account went. Credentials already stored here
        // stay: they are this device's sign-in now.
        SIGN_IN => {
            repo::sign_ins::drop_arrivals_of_record(conn, id)?;
            repo::sign_ins::forget_record(conn, id)?;
        }
        // Preferences are never deleted, and device records go with their
        // device.
        _ => {}
    }
    Ok(())
}

/// The other devices' own records, by device id.
pub(crate) fn device_records(
    mirror: &BTreeMap<RecordId, Entry>,
) -> HashMap<RecordId, Map<String, Value>> {
    mirror
        .iter()
        .filter(|(_, entry)| entry.collection == DEVICE)
        .filter_map(|(id, entry)| Some((*id, entry.body.as_object()?.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;
    use crate::models::{AuthKind, Provider};
    use flectar_mail_sync_client::Clock;

    fn account(conn: &Connection, email: &str, sync_id: &str) -> i64 {
        let id = repo::accounts::insert(
            conn,
            &repo::accounts::NewAccount {
                email,
                display_name: None,
                avatar_url: None,
                provider: Provider::Generic,
                auth_kind: AuthKind::Password,
                mail_protocol: MailProtocol::Imap,
                username: email,
                jmap_url: "",
                jmap_account_id: None,
                incoming_host: "imap.example.com",
                incoming_port: 993,
                smtp_host: "smtp.example.com",
                smtp_port: 465,
            },
            &testutil::account_settings(),
        )
        .unwrap();
        conn.execute(
            "UPDATE accounts SET sync_id = ?1 WHERE id = ?2",
            params![sync_id, id],
        )
        .unwrap();
        id
    }

    /// What the server has for `id`: a record, or a deletion.
    fn mirror_row(conn: &Connection, id: &RecordId, entry: Option<&Entry>) {
        conn.execute(
            "INSERT OR REPLACE INTO device_sync_mirror
               (record_id, seq, stamp_ms, stamp_counter, stamp_device, collection, schema, body)
             VALUES (?1, 1, 1, 0, ?2, ?3, ?4, ?5)",
            params![
                &id[..],
                &[9u8; 16][..],
                entry.map(|entry| entry.collection.clone()),
                entry.map(|entry| i64::from(entry.schema)),
                entry.map(|entry| entry.body.to_string())
            ],
        )
        .unwrap();
    }

    fn sync_id_of(conn: &Connection, account: i64) -> String {
        conn.query_row(
            "SELECT sync_id FROM accounts WHERE id = ?1",
            params![account],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn other_devices_records_are_never_this_devices_items() {
        let conn = testutil::conn();
        let phone = [7u8; 16];
        let mut waiting = BTreeMap::new();
        waiting.insert(
            phone,
            entry(DEVICE, json!({"name": "Phone", "platform": "android"})),
        );
        apply(
            &conn,
            &BTreeMap::new(),
            &waiting,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        // So this device never deletes it for being missing from its items.
        let clock = Clock::new([1; 16], None);
        assert_eq!(
            super::super::store::track(&conn, &BTreeMap::new(), &clock).unwrap(),
            0
        );
    }

    #[test]
    fn profiles_keep_accounts_this_device_does_not_have_yet() {
        let conn = testutil::conn();
        let known = "aa".repeat(16);
        let missing = "bb".repeat(16);
        let local = account(&conn, "me@example.com", &known);
        let mut settings = repo::settings::get(&conn).unwrap();
        settings.mail_profiles.push(MailProfile {
            id: "profile-1".into(),
            name: "Work".into(),
            color: "#8b5cf6".into(),
            account_ids: vec![local],
        });
        repo::settings::set(&conn, &settings).unwrap();
        let profile = [3u8; 16];
        let mut names = Names::default();
        names.insert(profile_name("profile-1"), profile);
        let mut mirror = BTreeMap::new();
        mirror.insert(
            profile,
            Mirrored {
                seq: 1,
                stamp: None,
                entry: Some(entry(
                    PROFILE,
                    json!({"id": "profile-1", "name": "Work", "color": "#8b5cf6",
                           "accounts": [missing, known]}),
                )),
            },
        );
        let records = snapshot(&conn, &names, &mirror, ([1; 16], json!({}))).unwrap();
        assert_eq!(records[&profile].body["accounts"], json!([missing, known]));
    }

    #[test]
    fn records_from_a_newer_version_are_left_alone() {
        let conn = testutil::conn();
        let id = [5u8; 16];
        let newer = Entry {
            collection: PREF.into(),
            schema: 2,
            body: json!({"key": "theme", "value": "dark", "since": "v2"}),
        };
        mirror_row(&conn, &id, Some(&newer));
        let mut local = BTreeMap::new();
        local.insert(id, entry(PREF, json!({"key": "theme", "value": "light"})));
        let clock = Clock::new([1; 16], None);
        assert_eq!(
            super::super::store::track(&conn, &local, &clock).unwrap(),
            0
        );
    }

    #[test]
    fn one_mailbox_added_on_two_devices_ends_under_the_smaller_id() {
        let smaller = "11".repeat(16);
        let larger = "ff".repeat(16);
        let remote = |conn: &Connection, local: i64| {
            let config = repo::accounts::get_config(conn, local).unwrap().unwrap();
            let settings = repo::settings::get(conn).unwrap();
            entry(ACCOUNT, account_body(&config, &settings, 0).unwrap())
        };

        // This device's id is the larger one: it adopts the other device's.
        let conn = testutil::conn();
        let local = account(&conn, "me@example.com", &larger);
        mirror_row(&conn, &sync_id_bytes(&larger).unwrap(), None);
        let mut waiting = BTreeMap::new();
        waiting.insert(sync_id_bytes(&smaller).unwrap(), remote(&conn, local));
        apply(
            &conn,
            &BTreeMap::new(),
            &waiting,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(sync_id_of(&conn, local), smaller);

        // This device's id is the smaller one: it keeps it, and the other
        // device adopts it.
        let conn = testutil::conn();
        let local = account(&conn, "me@example.com", &smaller);
        mirror_row(&conn, &sync_id_bytes(&smaller).unwrap(), None);
        let mut waiting = BTreeMap::new();
        waiting.insert(sync_id_bytes(&larger).unwrap(), remote(&conn, local));
        apply(
            &conn,
            &BTreeMap::new(),
            &waiting,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(sync_id_of(&conn, local), smaller);
    }

    fn template(conn: &Connection, name: &str, shortcut: Option<&str>, sync_id: &str) {
        conn.execute(
            "INSERT INTO snippets (name, shortcut, subject, body_text, sync_id)
             VALUES (?1, ?2, 'Hello', 'Thanks!', ?3)",
            params![name, shortcut, sync_id],
        )
        .unwrap();
    }

    fn shortcut_of(conn: &Connection, sync_id: &str) -> Option<String> {
        conn.query_row(
            "SELECT shortcut FROM snippets WHERE sync_id = ?1",
            params![sync_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn templates_travel_between_devices_and_go_when_deleted() {
        let laptop = testutil::conn();
        let sync_id = "ab".repeat(16);
        template(&laptop, "Thanks", Some("ty"), &sync_id);
        let records = snapshot(
            &laptop,
            &Names::default(),
            &BTreeMap::new(),
            ([1; 16], json!({})),
        )
        .unwrap();
        let id = sync_id_bytes(&sync_id).unwrap();
        assert_eq!(records[&id].collection, TEMPLATE);
        assert_eq!(
            records[&id].body,
            json!({"name": "Thanks", "shortcut": "ty", "subject": "Hello", "bodyText": "Thanks!"})
        );

        let phone = testutil::conn();
        let mut waiting = BTreeMap::new();
        waiting.insert(id, records[&id].clone());
        apply(
            &phone,
            &BTreeMap::new(),
            &waiting,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        let templates = repo::snippets::list(&phone).unwrap();
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].name, "Thanks");
        assert_eq!(shortcut_of(&phone, &sync_id).as_deref(), Some("ty"));

        let mut incoming = BTreeMap::new();
        incoming.insert(
            id,
            Incoming {
                previous: Some(records[&id].clone()),
                next: None,
            },
        );
        apply(
            &phone,
            &incoming,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(repo::snippets::list(&phone).unwrap().is_empty());
    }

    #[test]
    fn the_template_with_the_smaller_id_keeps_a_shared_shortcut() {
        let smaller = "11".repeat(16);
        let larger = "ff".repeat(16);
        let incoming = |sync_id: &str| {
            let mut waiting = BTreeMap::new();
            waiting.insert(
                sync_id_bytes(sync_id).unwrap(),
                entry(
                    TEMPLATE,
                    json!({"name": "Hi", "shortcut": "hi", "subject": null, "bodyText": "Hi!"}),
                ),
            );
            waiting
        };

        let conn = testutil::conn();
        template(&conn, "Hello", Some("hi"), &larger);
        apply(
            &conn,
            &BTreeMap::new(),
            &incoming(&smaller),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(shortcut_of(&conn, &smaller).as_deref(), Some("hi"));
        assert_eq!(shortcut_of(&conn, &larger), None);

        let conn = testutil::conn();
        template(&conn, "Hello", Some("hi"), &smaller);
        apply(
            &conn,
            &BTreeMap::new(),
            &incoming(&larger),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(shortcut_of(&conn, &smaller).as_deref(), Some("hi"));
        assert_eq!(shortcut_of(&conn, &larger), None);
    }

    /// A device with account 1 under `account` and, unless `None`, a
    /// conversation holding the message `message_id`. Returns the
    /// conversation's local id.
    fn device_with(
        account: &str,
        message_id: Option<&str>,
        local_ids_from: i64,
    ) -> (Connection, Option<i64>) {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute(
            "UPDATE accounts SET sync_id = ?1 WHERE id = 1",
            params![account],
        )
        .unwrap();
        // Local ids differ between devices.
        conn.execute(
            "UPDATE sqlite_sequence SET seq = ?1 WHERE name = 'threads'",
            params![local_ids_from],
        )
        .unwrap();
        let thread = message_id.map(|message_id| conversation(&conn, message_id, local_ids_from));
        (conn, thread)
    }

    fn conversation(conn: &Connection, message_id: &str, thread_id: i64) -> i64 {
        conn.execute(
            "INSERT INTO threads (id, account_id, subject_norm, unread_count, last_message_at)
             VALUES (?1, 1, 'plans', 0, 1000)",
            params![thread_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id, subject,
             from_addr, date, is_read, is_automated, is_draft, is_outgoing)
             VALUES (?1, 1, 1, ?1, ?2, 'Plans', 'ann@example.com', 1000, 1, 0, 0, 0)",
            params![thread_id, message_id],
        )
        .unwrap();
        thread_id
    }

    fn snoozed_until(conn: &Connection, thread: i64) -> Option<i64> {
        conn.query_row(
            "SELECT wake_at FROM snoozes WHERE thread_id = ?1",
            params![thread],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
    }

    fn snooze_records(conn: &Connection) -> Vec<(RecordId, Entry)> {
        snapshot(
            conn,
            &Names::default(),
            &BTreeMap::new(),
            ([1; 16], json!({})),
        )
        .unwrap()
        .into_iter()
        .filter(|(_, entry)| entry.collection == SNOOZE)
        .collect()
    }

    #[test]
    fn snoozes_follow_the_conversation_to_other_devices() {
        let account = "cc".repeat(16);
        let (laptop, laptop_thread) = device_with(&account, Some("plans@example.com"), 10);
        let laptop_thread = laptop_thread.unwrap();
        let wake_at = crate::models::now_ms() + 3_600_000;
        repo::snoozes::set(&laptop, laptop_thread, 1, wake_at, Some(1)).unwrap();
        let records = snooze_records(&laptop);
        assert_eq!(records.len(), 1);
        let (id, record) = records[0].clone();
        assert_eq!(record.body["anchor"]["messageId"], "plans@example.com");
        assert_eq!(record.body["wakeAt"], wake_at);

        // The phone has the conversation under another local id.
        let (phone, phone_thread) = device_with(&account, Some("plans@example.com"), 500);
        let phone_thread = phone_thread.unwrap();
        let mut waiting = BTreeMap::new();
        waiting.insert(id, record.clone());
        let applied = apply(
            &phone,
            &BTreeMap::new(),
            &waiting,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(snoozed_until(&phone, phone_thread), Some(wake_at));
        assert_eq!(applied.threads_changed, [phone_thread]);
        // It reproduces the record it took on, so nothing uploads again.
        assert_eq!(snooze_records(&phone), [(id, record.clone())]);

        // Unsnoozed on the laptop: it ends here too.
        let mut incoming = BTreeMap::new();
        incoming.insert(
            id,
            Incoming {
                previous: Some(record),
                next: None,
            },
        );
        apply(
            &phone,
            &incoming,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(snoozed_until(&phone, phone_thread), None);
        assert!(snooze_records(&phone).is_empty());
    }

    fn reminder_records(conn: &Connection) -> Vec<(RecordId, Entry)> {
        snapshot(
            conn,
            &Names::default(),
            &BTreeMap::new(),
            ([1; 16], json!({})),
        )
        .unwrap()
        .into_iter()
        .filter(|(_, entry)| entry.collection == REMINDER)
        .collect()
    }

    #[test]
    fn reply_reminders_reach_every_device_and_end_everywhere() {
        let account = "cc".repeat(16);
        let (laptop, _) = device_with(&account, Some("plans@example.com"), 10);
        let remind_at = crate::models::now_ms() + 3_600_000;
        repo::reply_reminders::set(&laptop, 1, "plans@example.com", remind_at).unwrap();
        let records = reminder_records(&laptop);
        assert_eq!(records.len(), 1);
        let (id, record) = records[0].clone();
        assert_eq!(
            record.body,
            json!({"account": account, "messageId": "plans@example.com", "remindAt": remind_at})
        );

        // The phone has no copy of the message yet: it keeps the reminder,
        // without re-uploading it, and finds the conversation once it comes.
        let (phone, _) = device_with(&account, None, 500);
        let mut waiting = BTreeMap::new();
        waiting.insert(id, record.clone());
        apply(
            &phone,
            &BTreeMap::new(),
            &waiting,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(reminder_records(&phone), [(id, record.clone())]);
        let thread = conversation(&phone, "plans@example.com", 600);
        let applied = apply(
            &phone,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(applied.threads_changed, [thread]);

        // Dismissed on the laptop: it ends on the phone too.
        let mut incoming = BTreeMap::new();
        incoming.insert(
            id,
            Incoming {
                previous: Some(record),
                next: None,
            },
        );
        let applied = apply(
            &phone,
            &incoming,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(applied.threads_changed, [thread]);
        assert!(reminder_records(&phone).is_empty());
    }

    #[test]
    fn a_fetched_message_stays_while_its_record_names_its_file() {
        let account = "cc".repeat(16);
        let (phone, _) = device_with(&account, None, 10);
        let id = [5u8; 16];
        let sync_id = sync_id_hex(&id);
        let record = |file: u8, claim: Value| {
            let file = super::super::scheduled::FileRef {
                id: [file; 16],
                key: [4; 32],
                size: 9000,
            };
            entry(
                SCHEDULED,
                json!({
                    "account": account, "messageId": "plans@example.com",
                    "sendAt": 4_000_000_000_000_i64, "origin": "aa", "draft": "v1",
                    "state": "scheduled", "error": null, "claim": claim,
                    "file": serde_json::from_str::<Value>(&file.to_json()).unwrap(),
                }),
            )
        };
        let change = |previous: Option<Entry>, next: Entry| {
            let mut incoming = BTreeMap::new();
            incoming.insert(
                id,
                Incoming {
                    previous,
                    next: Some(next),
                },
            );
            apply(
                &phone,
                &incoming,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .unwrap();
        };
        let fetched = || repo::synced_sends::mime(&phone, &sync_id).unwrap();

        let first = record(3, Value::Null);
        change(None, first.clone());
        assert_eq!(fetched(), None);
        repo::synced_sends::put_mime(&phone, &sync_id, Some(b"Subject: Plans")).unwrap();

        // Claimed: the record still names the file it came from.
        let claimed = record(3, json!({"device": "bb", "until": 1}));
        change(Some(first), claimed.clone());
        assert_eq!(fetched().as_deref(), Some(&b"Subject: Plans"[..]));

        // Stored again after an edit: the copy here is out of date.
        change(Some(claimed), record(8, Value::Null));
        assert_eq!(fetched(), None);
    }

    #[test]
    fn a_snooze_waits_for_its_conversations_mail() {
        let account = "cc".repeat(16);
        let (laptop, thread) = device_with(&account, Some("plans@example.com"), 10);
        let wake_at = crate::models::now_ms() + 3_600_000;
        repo::snoozes::set(&laptop, thread.unwrap(), 1, wake_at, Some(1)).unwrap();
        let (id, record) = snooze_records(&laptop).remove(0);

        let (phone, _) = device_with(&account, None, 500);
        let mut waiting = BTreeMap::new();
        waiting.insert(id, record.clone());
        apply(
            &phone,
            &BTreeMap::new(),
            &waiting,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        // Kept, so the phone neither drops nor re-uploads it.
        assert_eq!(snooze_records(&phone), [(id, record)]);

        // The mail arrives; the next run snoozes it.
        let thread = conversation(&phone, "plans@example.com", 600);
        let snoozed = repo::snoozes::attach_waiting(&phone, crate::models::now_ms()).unwrap();
        assert_eq!(snoozed, [thread]);
        assert_eq!(snoozed_until(&phone, thread), Some(wake_at));
    }

    #[test]
    fn a_snooze_that_already_ended_is_removed_everywhere() {
        let account = "cc".repeat(16);
        let (phone, thread) = device_with(&account, Some("plans@example.com"), 10);
        let id = [4u8; 16];
        let mut waiting = BTreeMap::new();
        waiting.insert(
            id,
            entry(
                SNOOZE,
                json!({"account": account, "anchor": {"messageId": "plans@example.com"},
                       "wakeAt": crate::models::now_ms() - 1_000}),
            ),
        );
        apply(
            &phone,
            &BTreeMap::new(),
            &waiting,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(snoozed_until(&phone, thread.unwrap()), None);
        // Taken on but not kept: the next check uploads its deletion.
        let clock = Clock::new([1; 16], None);
        let local = snapshot(
            &phone,
            &Names::default(),
            &BTreeMap::new(),
            ([1; 16], json!({})),
        )
        .unwrap();
        super::super::store::track(&phone, &local, &clock).unwrap();
        let deleted: i64 = phone
            .query_row(
                "SELECT COUNT(*) FROM device_sync_outbox WHERE record_id = ?1 AND body IS NULL",
                params![&id[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(deleted, 1);
    }

    #[test]
    fn two_snoozes_of_one_conversation_keep_the_smaller_id() {
        let account = "cc".repeat(16);
        let (phone, thread) = device_with(&account, Some("plans@example.com"), 10);
        let thread = thread.unwrap();
        let soon = crate::models::now_ms() + 3_600_000;
        let later = soon + 3_600_000;
        let record = |wake_at: i64| {
            entry(
                SNOOZE,
                json!({"account": account, "anchor": {"messageId": "plans@example.com"},
                       "wakeAt": wake_at}),
            )
        };
        let mut waiting = BTreeMap::new();
        waiting.insert([0xee; 16], record(later));
        waiting.insert([0x22; 16], record(soon));
        apply(
            &phone,
            &BTreeMap::new(),
            &waiting,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(snoozed_until(&phone, thread), Some(soon));
        let kept: Vec<RecordId> = snooze_records(&phone)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(kept, [[0x22; 16]]);
    }

    #[test]
    fn sign_ins_for_another_kind_of_device_are_left_alone() {
        let conn = testutil::conn();
        account(&conn, "me@example.com", &"a".repeat(32));
        let body = json!({
            "account": "a".repeat(32),
            "kind": "oauth",
            "family": "elsewhere",
            "sealed": {"epoch": 1, "data": "AAAA"},
        });
        let mut waiting = BTreeMap::new();
        waiting.insert([5; 16], entry(SIGN_IN, body));
        apply(
            &conn,
            &BTreeMap::new(),
            &waiting,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(repo::sign_ins::arrivals(&conn).unwrap().is_empty());
        // Nor does this device delete it for having no share of it.
        let clock = Clock::new([1; 16], None);
        assert_eq!(
            super::super::store::track(&conn, &BTreeMap::new(), &clock).unwrap(),
            0
        );
    }

    #[test]
    fn a_kept_sign_in_follows_what_the_group_has() {
        use base64::Engine as _;
        let conn = testutil::conn();
        let account = account(&conn, "me@example.com", &"a".repeat(32));
        let id = [6; 16];
        let share = repo::sign_ins::Share {
            account_id: account,
            kind: "password".into(),
            family: "any".into(),
            record_id: Some(id),
            sealed: Some(r#"{"epoch":1,"data":"AAAA"}"#.into()),
            fingerprint: Some(vec![1; 32]),
            ..Default::default()
        };
        repo::sign_ins::put_share(&conn, &share).unwrap();
        let local = |conn: &Connection| -> BTreeMap<RecordId, Entry> {
            let share = repo::sign_ins::share_of_record(conn, &id).unwrap().unwrap();
            super::super::sign_ins::body(&share, &"a".repeat(32))
                .map(|body| (id, entry(SIGN_IN, body)))
                .into_iter()
                .collect()
        };
        // In step with the server.
        let ours = local(&conn)[&id].clone();
        set_local_hash(&conn, &id, Some(body_hash(&ours))).unwrap();
        mirror_row(&conn, &id, Some(&ours));
        let clock = Clock::new([1; 16], None);
        let take = |previous: &Entry, next: &Entry| {
            let mut incoming = BTreeMap::new();
            incoming.insert(
                id,
                Incoming {
                    previous: Some(previous.clone()),
                    next: Some(next.clone()),
                },
            );
            let before = local(&conn);
            apply(&conn, &incoming, &BTreeMap::new(), &before, &before).unwrap();
            mirror_row(&conn, &id, Some(next));
        };

        // Another device found it stale: this one says so too, instead of
        // sending its own back.
        let rejected = base64::engine::general_purpose::STANDARD.encode([1u8; 32]);
        let stale = entry(
            SIGN_IN,
            json!({
                "account": "a".repeat(32), "kind": "password", "family": "any",
                "stale": true, "rejected": rejected,
            }),
        );
        take(&ours, &stale);
        assert_eq!(
            super::super::store::track(&conn, &local(&conn), &clock).unwrap(),
            0
        );

        // One that works arrives: this device keeps it, and installs it.
        let working = entry(
            SIGN_IN,
            json!({
                "account": "a".repeat(32), "kind": "password", "family": "any",
                "sealed": {"epoch": 1, "data": "BBBB"},
            }),
        );
        take(&stale, &working);
        assert_eq!(
            super::super::store::track(&conn, &local(&conn), &clock).unwrap(),
            0
        );
        assert_eq!(repo::sign_ins::arrivals(&conn).unwrap().len(), 1);
    }

    #[test]
    fn openpgp_settings_stay_on_their_device() {
        let conn = testutil::conn();
        let id = account(&conn, "me@example.com", &"a".repeat(32));
        let mut config = repo::accounts::get_config(&conn, id).unwrap().unwrap();
        config.settings.security.sign_by_default = true;
        config.settings.security.signing_fingerprint = "F".repeat(40);
        let body = account_body(&config, &Settings::default(), 0).unwrap();
        assert!(body["settings"].get("security").is_none(), "{body}");

        // Another device's settings leave this device's own alone.
        let mut synced = config.settings.clone();
        synced.security = Default::default();
        let mut current = config.settings.clone();
        current.security.require_encryption = true;
        assert_eq!(
            merged_account_settings(&current, &synced).security,
            current.security
        );
    }

    #[test]
    fn a_pop3_account_whose_mail_left_the_server_stays_when_removed_elsewhere() {
        let conn = testutil::conn();
        let removed = |sync: &str| {
            let mut incoming = BTreeMap::new();
            incoming.insert(
                sync_id_bytes(sync).unwrap(),
                Incoming {
                    previous: Some(entry(ACCOUNT, json!({"email": "me@example.com"}))),
                    next: None,
                },
            );
            apply(
                &conn,
                &incoming,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .unwrap()
        };

        let pop = account(&conn, "pop@example.com", &"b".repeat(32));
        conn.execute(
            "UPDATE accounts SET mail_protocol = 'pop3',
                    settings_json = json_set(settings_json, '$.keepOnServer', 'sevenDays')
             WHERE id = ?1",
            params![pop],
        )
        .unwrap();
        let applied = removed(&"b".repeat(32));
        assert!(applied.follow_ups.is_empty(), "its mail stays here");
        let sync_id: Option<String> = conn
            .query_row(
                "SELECT sync_id FROM accounts WHERE id = ?1",
                params![pop],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sync_id, None, "synced no more");

        // One whose mail is on the server goes.
        account(&conn, "imap@example.com", &"c".repeat(32));
        let applied = removed(&"c".repeat(32));
        assert!(matches!(
            applied.follow_ups.as_slice(),
            [FollowUp::RemoveAccount(..)]
        ));
    }

    #[test]
    fn sync_ids_round_trip() {
        let id = [0xab; 16];
        assert_eq!(sync_id_bytes(&sync_id_hex(&id)), Some(id));
        assert_eq!(sync_id_bytes("not hex"), None);
    }

    #[test]
    fn every_synced_preference_is_a_settings_field() {
        let settings = serde_json::to_value(Settings::default()).unwrap();
        for key in SYNCED_PREFERENCES {
            assert!(
                settings.get(*key).is_some(),
                "{key} is not a settings field"
            );
        }
    }
}
