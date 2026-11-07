//! What syncs, and how the app's items become records and back.
//!
//! Each item is one record. Accounts carry their per-account preferences;
//! preferences, signatures and profiles are named records (their record ids
//! derive from their key), and each device describes itself in a `device`
//! record under its own device id.

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

/// The schema this version writes and reads for `collection`, or `None` for
/// a collection it does not know. A version that changes what a record
/// holds raises its collection's schema: records with a newer schema stay
/// unread here, and this device leaves them alone until it updates.
pub(crate) fn schema(collection: &str) -> Option<u32> {
    match collection {
        PREF | ACCOUNT | SIGNATURE | PROFILE | DEVICE => Some(1),
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

fn sync_id_bytes(sync_id: &str) -> Option<RecordId> {
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
/// keeps and, for POP3, what it leaves on the server.
fn account_body(
    config: &crate::models::AccountConfig,
    settings: &Settings,
    sort_order: i64,
) -> Result<Value> {
    let mut body = serde_json::to_value(PortableAccountConfig::from(config))?;
    if let Some(account_settings) = body.get_mut("settings").and_then(Value::as_object_mut) {
        account_settings.remove("mailHistory");
        account_settings.remove("keepOnServer");
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

    records.insert(device.0, entry(DEVICE, device.1));
    Ok(records)
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
        PREF | SIGNATURE | PROFILE => 1,
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
            (None, None) => Outcome::Applied,
        };
        match outcome {
            Outcome::Applied => set_local_hash(conn, &id, next.map(body_hash))?,
            // Not this device's item: it never uploads or deletes it.
            Outcome::Ignored => set_local_hash(conn, &id, None)?,
            Outcome::Waiting => {}
        }
    }

    if serde_json::to_value(&settings)? != settings_before {
        repo::settings::set(conn, &settings)?;
        applied.settings_changed = true;
    }
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
                || current.settings.connection != config.settings.connection
                || current.settings.security != config.settings.security;
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
                // Credentials stay on the device that has them: this one
                // asks the user to sign in.
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

/// The synced account settings over this device's own mail history and POP3
/// retention.
fn merged_account_settings(current: &AccountSettings, synced: &AccountSettings) -> AccountSettings {
    AccountSettings {
        mail_history: current.mail_history,
        keep_on_server: current.keep_on_server,
        ..synced.clone()
    }
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
                applied
                    .follow_ups
                    .push(FollowUp::RemoveAccount(local_id, sync_id_hex(id)));
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
