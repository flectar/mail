//! POP3 accounts: their local folders, what the server announced, and the
//! UIDL ledger of every message the account has seen on the server.

use crate::error::Result;
use crate::models::{KeepOnServer, roles};
use crate::pop3::Expire;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, ToSql, params};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// A POP3 server only delivers into an inbox. Every folder, including the
/// one mail arrives in, exists on this device alone.
const LOCAL_FOLDERS: [(&str, &str); 6] = [
    ("Inbox", roles::INBOX),
    ("Drafts", roles::DRAFTS),
    ("Sent", roles::SENT),
    ("Archive", roles::ARCHIVE),
    ("Junk", roles::SPAM),
    ("Trash", roles::TRASH),
];

/// Local folder paths separate their levels with this.
pub const FOLDER_DELIMITER: &str = "/";

/// How long a message the server stopped listing stays recorded, in case the
/// server lists it again.
const MISSING_RETENTION_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// Create what a new POP3 account starts with, in the account's own
/// transaction.
pub fn create_account_state(conn: &Connection, account_id: i64) -> Result<()> {
    for (path, role) in LOCAL_FOLDERS {
        super::folders::upsert(conn, account_id, path, Some(FOLDER_DELIMITER), Some(role))?;
    }
    conn.execute(
        "INSERT INTO pop3_sync_state (account_id) VALUES (?1)",
        [account_id],
    )?;
    Ok(())
}

/// What the account knows about its server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncState {
    /// When the first complete download finished.
    pub first_synced_at: Option<i64>,
    pub signed_in_at: Option<i64>,
    pub login_delay: Option<Duration>,
    pub expire: Option<Expire>,
}

impl SyncState {
    /// The earliest time the server accepts another sign-in.
    pub fn next_sign_in_at(&self) -> Option<i64> {
        let delay = i64::try_from(self.login_delay?.as_millis()).unwrap_or(i64::MAX);
        Some(self.signed_in_at?.saturating_add(delay))
    }
}

pub fn sync_state(conn: &Connection, account_id: i64) -> Result<SyncState> {
    Ok(conn
        .query_row(
            "SELECT first_synced_at, signed_in_at, login_delay_secs, expire
             FROM pop3_sync_state WHERE account_id = ?1",
            [account_id],
            |row| {
                Ok(SyncState {
                    first_synced_at: row.get(0)?,
                    signed_in_at: row.get(1)?,
                    login_delay: row
                        .get::<_, Option<i64>>(2)?
                        .and_then(|seconds| u64::try_from(seconds).ok())
                        .map(Duration::from_secs),
                    expire: row
                        .get::<_, Option<String>>(3)?
                        .and_then(|value| parse_expire(&value)),
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

/// Record an accepted sign-in and what the server announced in that session.
pub fn record_sign_in(
    conn: &Connection,
    account_id: i64,
    at: i64,
    login_delay: Option<Duration>,
    expire: Option<Expire>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pop3_sync_state (account_id, signed_in_at, login_delay_secs, expire)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(account_id) DO UPDATE SET
           signed_in_at = excluded.signed_in_at,
           login_delay_secs = excluded.login_delay_secs,
           expire = excluded.expire",
        params![
            account_id,
            at,
            login_delay.map(|delay| i64::try_from(delay.as_secs()).unwrap_or(i64::MAX)),
            expire.map(expire_text),
        ],
    )?;
    Ok(())
}

pub fn record_first_sync(conn: &Connection, account_id: i64, at: i64) -> Result<()> {
    conn.execute(
        "UPDATE pop3_sync_state SET first_synced_at = ?2
         WHERE account_id = ?1 AND first_synced_at IS NULL",
        params![account_id, at],
    )?;
    Ok(())
}

/// Forget what the account learned from its server, for a change of server
/// or user. Downloaded mail stays; messages the new server also holds are
/// recognized by their Message-ID.
pub fn forget_server(conn: &Connection, account_id: i64) -> Result<()> {
    conn.execute("DELETE FROM pop3_uidls WHERE account_id = ?1", [account_id])?;
    conn.execute(
        "UPDATE pop3_sync_state
         SET first_synced_at = NULL, signed_in_at = NULL, login_delay_secs = NULL, expire = NULL
         WHERE account_id = ?1",
        [account_id],
    )?;
    Ok(())
}

fn expire_text(expire: Expire) -> String {
    match expire {
        Expire::Never => "never".into(),
        Expire::Days(days) => days.to_string(),
    }
}

pub(super) fn parse_expire(value: &str) -> Option<Expire> {
    if value == "never" {
        Some(Expire::Never)
    } else {
        value.parse().ok().map(Expire::Days)
    }
}

/// What became of a message the server listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UidlState {
    Stored,
    /// The account already had the message, so the server's copy wasn't
    /// kept.
    Matched,
    /// Older than the account's mail history.
    Skipped,
    Failed,
}

impl UidlState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stored => "stored",
            Self::Matched => "matched",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "stored" => Some(Self::Stored),
            "matched" => Some(Self::Matched),
            "skipped" => Some(Self::Skipped),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

impl FromSql for UidlState {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let name = value.as_str()?;
        Self::parse(name)
            .ok_or_else(|| FromSqlError::Other(format!("unknown UIDL state {name:?}").into()))
    }
}

impl ToSql for UidlState {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownUidl {
    pub state: UidlState,
    pub sent_at: Option<i64>,
    pub missing_since: Option<i64>,
    pub attempts: i64,
    pub retry_at: Option<i64>,
}

pub fn known_uidls(conn: &Connection, account_id: i64) -> Result<HashMap<String, KnownUidl>> {
    let mut statement = conn.prepare(
        "SELECT uidl, state, sent_at, missing_since, attempts, retry_at
         FROM pop3_uidls WHERE account_id = ?1",
    )?;
    let rows = statement.query_map([account_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            KnownUidl {
                state: row.get(1)?,
                sent_at: row.get(2)?,
                missing_since: row.get(3)?,
                attempts: row.get(4)?,
                retry_at: row.get(5)?,
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Bring the ledger in line with a complete listing: recorded messages it
/// lacks start their retention period, ones it lists again end it, and ones
/// missing for longer than the retention period are forgotten.
pub fn reconcile_listing(
    conn: &Connection,
    account_id: i64,
    known: &HashMap<String, KnownUidl>,
    listed: &HashSet<&str>,
    now: i64,
) -> Result<()> {
    let mut missing = conn
        .prepare("UPDATE pop3_uidls SET missing_since = ?3 WHERE account_id = ?1 AND uidl = ?2")?;
    let mut returned = conn.prepare(
        "UPDATE pop3_uidls SET missing_since = NULL WHERE account_id = ?1 AND uidl = ?2",
    )?;
    for (uidl, entry) in known {
        match (listed.contains(uidl.as_str()), entry.missing_since) {
            (false, None) => {
                missing.execute(params![account_id, uidl, now])?;
            }
            (true, Some(_)) => {
                returned.execute(params![account_id, uidl])?;
            }
            _ => {}
        }
    }
    conn.execute(
        "DELETE FROM pop3_uidls WHERE account_id = ?1 AND missing_since < ?2",
        params![account_id, now.saturating_sub(MISSING_RETENTION_MS)],
    )?;
    Ok(())
}

/// A downloaded message, stored as `message_id`.
pub fn record_stored(
    conn: &Connection,
    account_id: i64,
    uidl: &str,
    size: u64,
    sent_at: Option<i64>,
    message_id: i64,
    at: i64,
) -> Result<()> {
    record_on_device(
        conn,
        account_id,
        uidl,
        UidlState::Stored,
        size,
        sent_at,
        message_id,
        at,
    )
}

/// A message the account already had as `message_id`.
pub fn record_matched(
    conn: &Connection,
    account_id: i64,
    uidl: &str,
    size: u64,
    sent_at: Option<i64>,
    message_id: i64,
    at: i64,
) -> Result<()> {
    record_on_device(
        conn,
        account_id,
        uidl,
        UidlState::Matched,
        size,
        sent_at,
        message_id,
        at,
    )
}

#[allow(clippy::too_many_arguments)]
fn record_on_device(
    conn: &Connection,
    account_id: i64,
    uidl: &str,
    state: UidlState,
    size: u64,
    sent_at: Option<i64>,
    message_id: i64,
    at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pop3_uidls (account_id, uidl, state, message_id, size, sent_at, stored_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(account_id, uidl) DO UPDATE SET
           state = excluded.state, message_id = excluded.message_id, size = excluded.size,
           sent_at = excluded.sent_at, stored_at = excluded.stored_at,
           missing_since = NULL, retry_at = NULL, last_error = NULL",
        params![
            account_id,
            uidl,
            state,
            message_id,
            size_column(size),
            sent_at,
            at
        ],
    )?;
    Ok(())
}

/// A message older than the account's mail history.
pub fn record_skipped(
    conn: &Connection,
    account_id: i64,
    uidl: &str,
    size: u64,
    sent_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pop3_uidls (account_id, uidl, state, size, sent_at)
         VALUES (?1, ?2, 'skipped', ?3, ?4)
         ON CONFLICT(account_id, uidl) DO UPDATE SET
           state = 'skipped', size = excluded.size, sent_at = excluded.sent_at,
           retry_at = NULL, last_error = NULL",
        params![account_id, uidl, size_column(size), sent_at],
    )?;
    Ok(())
}

/// A message that couldn't be downloaded, tried again from `retry_at`.
pub fn record_failed(
    conn: &Connection,
    account_id: i64,
    uidl: &str,
    size: u64,
    error: &str,
    retry_at: Option<i64>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pop3_uidls (account_id, uidl, state, size, attempts, retry_at, last_error)
         VALUES (?1, ?2, 'failed', ?3, 1, ?4, ?5)
         ON CONFLICT(account_id, uidl) DO UPDATE SET
           state = 'failed', size = excluded.size, attempts = attempts + 1,
           retry_at = excluded.retry_at, last_error = excluded.last_error",
        params![account_id, uidl, size_column(size), retry_at, error],
    )?;
    Ok(())
}

fn size_column(size: u64) -> i64 {
    i64::try_from(size).unwrap_or(i64::MAX)
}

/// A downloaded message the account's setting removes from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removable {
    pub uidl: String,
    /// The size the server listed when the message was downloaded.
    pub size: u64,
    /// The message was deleted here. Otherwise it is still on this device,
    /// and its server copy goes only while its original exists.
    pub deleted: bool,
    pub original: Option<String>,
}

/// The messages `keep` removes from the server at `now`, oldest first. Only
/// mail downloaded here qualifies, never mail the account already had.
pub fn removable(
    conn: &Connection,
    account_id: i64,
    keep: KeepOnServer,
    now: i64,
) -> Result<Vec<Removable>> {
    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    let downloaded_before = keep
        .days()
        .map(|days| now.saturating_sub(i64::from(days) * DAY_MS));
    let mut statement = conn.prepare(
        "SELECT u.uidl, u.size, u.message_id IS NULL, m.raw_path
         FROM pop3_uidls u LEFT JOIN messages m ON m.id = u.message_id
         WHERE u.account_id = ?1 AND u.state = 'stored' AND u.missing_since IS NULL
           AND CASE WHEN u.message_id IS NULL THEN ?2 ELSE u.stored_at <= ?3 END
         ORDER BY u.stored_at, u.uidl",
    )?;
    let rows = statement.query_map(
        params![account_id, keep.removes_deleted(), downloaded_before],
        |row| {
            Ok(Removable {
                uidl: row.get(0)?,
                size: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                deleted: row.get(2)?,
                original: row.get(3)?,
            })
        },
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The UIDL a stored message was downloaded under.
pub fn uidl_of_message(conn: &Connection, message_id: i64) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT uidl FROM pop3_uidls
             WHERE message_id = ?1 AND state = 'stored' LIMIT 1",
            [message_id],
            |row| row.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::account_settings;
    use crate::models::{AuthKind, MailProtocol, Provider};

    fn account(conn: &Connection) -> i64 {
        crate::db::repo::accounts::insert(
            conn,
            &crate::db::repo::accounts::NewAccount {
                email: "me@example.com",
                display_name: None,
                avatar_url: None,
                provider: Provider::Generic,
                auth_kind: AuthKind::Password,
                mail_protocol: MailProtocol::Pop3,
                username: "me",
                jmap_url: "",
                jmap_account_id: None,
                incoming_host: "pop.example.com",
                incoming_port: 995,
                smtp_host: "smtp.example.com",
                smtp_port: 465,
            },
            &account_settings(),
        )
        .unwrap()
    }

    #[test]
    fn new_accounts_get_every_local_folder() {
        let conn = crate::db::testutil::conn();
        let id = account(&conn);
        let folders = crate::db::repo::folders::list(&conn, Some(id)).unwrap();
        let mut roles: Vec<_> = folders.iter().filter_map(|f| f.role.clone()).collect();
        roles.sort();
        assert_eq!(
            roles,
            ["archive", "drafts", "inbox", "sent", "spam", "trash"]
        );
        assert!(
            folders
                .iter()
                .all(|f| f.delimiter.as_deref() == Some(FOLDER_DELIMITER))
        );
        assert_eq!(sync_state(&conn, id).unwrap(), SyncState::default());
    }

    #[test]
    fn sign_ins_record_the_server_rules() {
        let conn = crate::db::testutil::conn();
        let id = account(&conn);
        record_sign_in(
            &conn,
            id,
            1_000,
            Some(Duration::from_secs(300)),
            Some(Expire::Days(0)),
        )
        .unwrap();
        let state = sync_state(&conn, id).unwrap();
        assert_eq!(state.next_sign_in_at(), Some(301_000));
        assert_eq!(state.expire, Some(Expire::Days(0)));
        record_sign_in(&conn, id, 2_000, None, Some(Expire::Never)).unwrap();
        let state = sync_state(&conn, id).unwrap();
        assert_eq!(state.next_sign_in_at(), None);
        assert_eq!(state.expire, Some(Expire::Never));

        record_first_sync(&conn, id, 5).unwrap();
        record_first_sync(&conn, id, 9).unwrap();
        assert_eq!(sync_state(&conn, id).unwrap().first_synced_at, Some(5));
        forget_server(&conn, id).unwrap();
        assert_eq!(sync_state(&conn, id).unwrap(), SyncState::default());
    }

    #[test]
    fn the_ledger_rejects_values_it_cannot_hold() {
        let conn = crate::db::testutil::conn();
        let id = account(&conn);
        let insert = |uidl: &str, state: &str, stored_at: Option<i64>| {
            conn.execute(
                "INSERT INTO pop3_uidls (account_id, uidl, state, size, stored_at)
                 VALUES (?1, ?2, ?3, 1, ?4)",
                params![id, uidl, state, stored_at],
            )
        };
        assert!(insert("ok", "skipped", None).is_ok());
        assert!(insert("", "skipped", None).is_err());
        assert!(insert(&"x".repeat(71), "skipped", None).is_err());
        assert!(insert("state", "pending", None).is_err());
        assert!(insert("unstamped", "stored", None).is_err());
        assert!(insert("unstamped match", "matched", None).is_err());
        assert!(insert("match", "matched", Some(1)).is_ok());
        let expire = |value: &str| {
            conn.execute(
                "UPDATE pop3_sync_state SET expire = ?2 WHERE account_id = ?1",
                params![id, value],
            )
        };
        assert!(expire("never").is_ok() && expire("30").is_ok());
        assert!(expire("-1").is_err() && expire("soon").is_err() && expire("").is_err());
    }

    #[test]
    fn accounts_show_how_long_their_server_keeps_mail() {
        let conn = crate::db::testutil::conn();
        let id = account(&conn);
        let keeps = |conn: &Connection| {
            crate::db::repo::accounts::get(conn, id)
                .unwrap()
                .unwrap()
                .server_keeps_mail_days
        };
        assert_eq!(keeps(&conn), None);
        record_sign_in(&conn, id, 1, None, Some(Expire::Days(30))).unwrap();
        assert_eq!(keeps(&conn), Some(30));
        record_sign_in(&conn, id, 2, None, Some(Expire::Never)).unwrap();
        assert_eq!(keeps(&conn), None);
    }

    #[test]
    fn only_mail_downloaded_here_is_removable() {
        let conn = crate::db::testutil::conn();
        let id = account(&conn);
        let day = 24 * 60 * 60 * 1000;
        let message = |subject: &str| {
            let (_, message) =
                crate::db::testutil::seed_message(&conn, "alice@example.com", subject, false);
            conn.execute(
                "UPDATE messages SET account_id = ?2, raw_path = ?3 WHERE id = ?1",
                params![message, id, format!("/mail/{subject}.pop3.eml")],
            )
            .unwrap();
            message
        };
        let (old, recent, deleted, matched) = (
            message("old"),
            message("recent"),
            message("deleted"),
            message("matched"),
        );
        record_stored(&conn, id, "old", 1, None, old, day).unwrap();
        record_stored(&conn, id, "recent", 2, None, recent, 9 * day).unwrap();
        record_stored(&conn, id, "deleted", 3, None, deleted, 9 * day).unwrap();
        record_matched(&conn, id, "matched", 4, None, matched, day).unwrap();
        record_stored(&conn, id, "gone", 5, None, old, day).unwrap();
        record_skipped(&conn, id, "skipped", 6, 0).unwrap();
        record_failed(&conn, id, "failed", 7, "refused", None).unwrap();
        crate::db::repo::messages::delete(&conn, deleted).unwrap();
        crate::db::repo::messages::delete(&conn, matched).unwrap();
        conn.execute(
            "UPDATE pop3_uidls SET missing_since = 1 WHERE uidl = 'gone'",
            [],
        )
        .unwrap();

        let uidls = |keep| {
            removable(&conn, id, keep, 10 * day)
                .unwrap()
                .into_iter()
                .map(|removable| removable.uidl)
                .collect::<Vec<_>>()
        };
        assert!(uidls(KeepOnServer::Always).is_empty());
        assert_eq!(uidls(KeepOnServer::UntilDeleted), ["deleted"]);
        assert_eq!(uidls(KeepOnServer::SevenDays), ["old", "deleted"]);
        assert_eq!(
            uidls(KeepOnServer::UntilDownloaded),
            ["old", "deleted", "recent"]
        );
        let removable = removable(&conn, id, KeepOnServer::UntilDownloaded, 10 * day).unwrap();
        assert_eq!(
            removable[0],
            Removable {
                uidl: "old".into(),
                size: 1,
                deleted: false,
                original: Some("/mail/old.pop3.eml".into()),
            }
        );
        assert!(removable[1].deleted && removable[1].original.is_none());
    }

    #[test]
    fn the_ledger_outlives_deleted_messages_and_ages_out_missing_ones() {
        let conn = crate::db::testutil::conn();
        let id = account(&conn);
        let (_, message) =
            crate::db::testutil::seed_message(&conn, "alice@example.com", "Hello", false);
        record_matched(&conn, id, "m", 10, Some(1), message, 100).unwrap();
        record_stored(&conn, id, "a", 10, Some(1), message, 100).unwrap();
        record_skipped(&conn, id, "b", 20, 2).unwrap();
        record_failed(&conn, id, "c", 30, "refused", Some(500)).unwrap();
        record_failed(&conn, id, "c", 30, "refused again", None).unwrap();
        assert_eq!(
            uidl_of_message(&conn, message).unwrap().as_deref(),
            Some("a")
        );

        // Deleting the message here keeps it from being downloaded again.
        crate::db::repo::messages::delete(&conn, message).unwrap();
        let known = known_uidls(&conn, id).unwrap();
        assert_eq!(known["a"].state, UidlState::Stored);
        assert_eq!(known["c"].attempts, 2);
        assert_eq!(known["c"].retry_at, None);

        let day = 24 * 60 * 60 * 1000;
        reconcile_listing(&conn, id, &known, &HashSet::from(["a", "c"]), day).unwrap();
        let known = known_uidls(&conn, id).unwrap();
        assert_eq!(known["b"].missing_since, Some(day));
        assert_eq!(known["a"].missing_since, None);
        // Listed again before the retention period ends.
        reconcile_listing(&conn, id, &known, &HashSet::from(["b"]), 2 * day).unwrap();
        let known = known_uidls(&conn, id).unwrap();
        assert_eq!(known["b"].missing_since, None);
        assert_eq!(known["a"].missing_since, Some(2 * day));
        reconcile_listing(&conn, id, &known, &HashSet::from(["b"]), 93 * day).unwrap();
        let known = known_uidls(&conn, id).unwrap();
        assert_eq!(known.keys().collect::<Vec<_>>(), ["b"]);
    }
}
