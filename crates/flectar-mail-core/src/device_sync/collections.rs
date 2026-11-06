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
use flectar_mail_sync_client::Entry;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};

pub(crate) const PREF: &str = "pref";
pub(crate) const ACCOUNT: &str = "account";
pub(crate) const SIGNATURE: &str = "signature";
pub(crate) const PROFILE: &str = "profile";
pub(crate) const DEVICE: &str = "device";

/// The schema this version writes and reads, for every collection. Records
/// with a newer schema stay unread until this device updates.
pub(crate) const SCHEMA: u32 = 1;

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
        schema: SCHEMA,
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
pub(crate) fn snapshot(
    conn: &Connection,
    names: &Names,
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
        let accounts: Vec<&String> = profile
            .account_ids
            .iter()
            .filter_map(|account| ids.by_local.get(account))
            .collect();
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

/// Work the app must do after the transaction that applied records.
#[derive(Debug)]
pub(crate) enum FollowUp {
    /// Another device removed the account.
    RemoveAccount(i64),
    /// Another device changed how the account connects.
    RestartAccount(i64),
}

/// What applying records changed.
#[derive(Debug, Default)]
pub(crate) struct Applied {
    pub settings_changed: bool,
    pub accounts_changed: bool,
    pub follow_ups: Vec<FollowUp>,
}

fn readable(entry: &Entry) -> bool {
    entry.schema <= SCHEMA
}

/// Collections that other records refer to come first.
fn order(collection: &str) -> u8 {
    match collection {
        ACCOUNT => 0,
        PREF | SIGNATURE | PROFILE => 1,
        _ => 2,
    }
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
        )
    });

    for (id, previous, next) in work {
        // Edited here during the run: keep the edit.
        if local_before.get(&id) != local_now.get(&id) {
            continue;
        }
        let done = match (previous, next) {
            (_, Some(next)) if !readable(next) => false,
            (_, Some(next)) => apply_put(conn, &mut settings, &id, next, &mut applied)?,
            (Some(previous), None) => {
                apply_delete(conn, &mut settings, &id, previous, &mut applied)?;
                true
            }
            (None, None) => true,
        };
        if done {
            // Taken on: a later change of it here is a local edit.
            set_local_hash(conn, &id, next.map(body_hash))?;
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
) -> Result<bool> {
    match entry.collection.as_str() {
        PREF => {
            let (Some(key), Some(value)) = (
                entry.body.get("key").and_then(Value::as_str),
                entry.body.get("value"),
            ) else {
                return Ok(false);
            };
            if !SYNCED_PREFERENCES.contains(&key) {
                return Ok(false);
            }
            let mut object = serde_json::to_value(&*settings)?;
            if let Some(fields) = object.as_object_mut() {
                fields.insert(key.to_owned(), value.clone());
            }
            match serde_json::from_value::<Settings>(object) {
                Ok(updated) => {
                    *settings = updated;
                    Ok(true)
                }
                Err(error) => {
                    tracing::warn!(%error, key, "a synced preference does not fit this version");
                    Ok(false)
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
                return Ok(false);
            };
            // Its account has not arrived yet; a later run applies it.
            let Some(account_id) = ids.by_sync.get(account).copied() else {
                return Ok(false);
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
            Ok(true)
        }
        PROFILE => {
            let ids = account_ids(conn)?;
            let (Some(profile_id), Some(name), Some(color)) = (
                entry.body.get("id").and_then(Value::as_str),
                entry.body.get("name").and_then(Value::as_str),
                entry.body.get("color").and_then(Value::as_str),
            ) else {
                return Ok(false);
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
            Ok(true)
        }
        // Other devices describe themselves; the app reads them from the
        // mirror.
        DEVICE => Ok(true),
        _ => Ok(false),
    }
}

fn apply_account(
    conn: &Connection,
    settings: &mut Settings,
    id: &RecordId,
    body: &Value,
    applied: &mut Applied,
) -> Result<bool> {
    let config: PortableAccountConfig = match serde_json::from_value(body.clone()) {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(%error, "a synced account does not fit this version");
            return Ok(false);
        }
    };
    if crate::validate_portable_account(&config).is_err() {
        return Ok(false);
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
        // become one account.
        if let Some(existing) = repo::accounts::find_by_mailbox(conn, config.email.trim())? {
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
                return Ok(false);
            };
            if (current.provider, current.auth_kind, current.mail_protocol)
                != (config.provider, config.auth_kind, config.mail_protocol)
            {
                tracing::warn!("a synced account changed protocol; keeping this device's");
                return Ok(false);
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
    Ok(true)
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
                applied.follow_ups.push(FollowUp::RemoveAccount(local_id));
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
