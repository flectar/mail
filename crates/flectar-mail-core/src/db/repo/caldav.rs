//! Calendar connections (each server an account syncs with) and the
//! collections they discovered. Credentials never live here: CalDAV app
//! passwords go to the keyring under their connection, Google and Microsoft
//! ride the account's OAuth tokens.

use crate::error::Result;
use crate::models::{Calendar, CalendarConnection, CalendarConnectionKind};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::{HashMap, HashSet};

macro_rules! connection_select {
    ($tail:literal) => {
        concat!(
            "SELECT id, account_id, kind, base_url, username, principal_url, ",
            "home_set_url, enabled, last_error FROM calendar_connections ",
            $tail
        )
    };
}

macro_rules! calendar_select {
    ($tail:literal) => {
        concat!(
            "SELECT c.id, c.account_id, c.connection_id, k.kind, c.url, ",
            "c.display_name, c.color, c.read_only, c.enabled, c.is_default, ",
            "c.last_synced_at FROM calendars c ",
            "JOIN calendar_connections k ON k.id = c.connection_id ",
            $tail
        )
    };
}

/// One connection's stored configuration.
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    pub id: i64,
    pub account_id: i64,
    pub kind: CalendarConnectionKind,
    /// What the user entered for CalDAV; the provider's endpoint otherwise.
    pub base_url: String,
    pub username: String,
    pub principal_url: Option<String>,
    pub home_set_url: Option<String>,
    pub enabled: bool,
    pub last_error: Option<String>,
}

fn connection_row(row: &Row) -> rusqlite::Result<ConnectionConfig> {
    Ok(ConnectionConfig {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        kind: row.get("kind")?,
        base_url: row.get("base_url")?,
        username: row.get("username")?,
        principal_url: row.get("principal_url")?,
        home_set_url: row.get("home_set_url")?,
        enabled: row.get::<_, i64>("enabled")? != 0,
        last_error: row.get("last_error")?,
    })
}

fn calendar_row(row: &Row) -> rusqlite::Result<Calendar> {
    Ok(Calendar {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        connection_id: row.get("connection_id")?,
        kind: row.get("kind")?,
        url: row.get("url")?,
        display_name: row.get("display_name")?,
        color: row.get("color")?,
        read_only: row.get::<_, i64>("read_only")? != 0,
        enabled: row.get::<_, i64>("enabled")? != 0,
        is_default: row.get::<_, i64>("is_default")? != 0,
        last_synced_at: row.get("last_synced_at")?,
    })
}

/// Store a new connection and return its id. `cfg.id` is ignored.
pub fn insert_connection(conn: &Connection, cfg: &ConnectionConfig) -> Result<i64> {
    conn.execute(
        "INSERT INTO calendar_connections
            (account_id, kind, base_url, username, principal_url, home_set_url, enabled)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            cfg.account_id,
            cfg.kind,
            cfg.base_url,
            cfg.username,
            cfg.principal_url,
            cfg.home_set_url,
            cfg.enabled as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Replace a connection's server details after a successful discovery. The
/// last sync error no longer applies.
pub fn update_connection(conn: &Connection, cfg: &ConnectionConfig) -> Result<()> {
    conn.execute(
        "UPDATE calendar_connections SET
            base_url = ?2, username = ?3, principal_url = ?4, home_set_url = ?5,
            enabled = ?6, last_error = NULL
         WHERE id = ?1",
        params![
            cfg.id,
            cfg.base_url,
            cfg.username,
            cfg.principal_url,
            cfg.home_set_url,
            cfg.enabled as i64,
        ],
    )?;
    Ok(())
}

pub fn get_connection(conn: &Connection, connection_id: i64) -> Result<Option<ConnectionConfig>> {
    conn.query_row(
        connection_select!("WHERE id = ?1"),
        params![connection_id],
        connection_row,
    )
    .optional()
    .map_err(Into::into)
}

/// The account's Google or Microsoft connection, which exists at most once.
pub fn provider_connection(
    conn: &Connection,
    account_id: i64,
    kind: CalendarConnectionKind,
) -> Result<Option<ConnectionConfig>> {
    conn.query_row(
        connection_select!("WHERE account_id = ?1 AND kind = ?2"),
        params![account_id, kind],
        connection_row,
    )
    .optional()
    .map_err(Into::into)
}

/// Every connection of one account: the provider's first, then CalDAV
/// servers in the order they were added.
pub fn account_connections(conn: &Connection, account_id: i64) -> Result<Vec<ConnectionConfig>> {
    let mut stmt = conn.prepare(connection_select!(
        "WHERE account_id = ?1 ORDER BY kind = 'generic', id"
    ))?;
    let rows = stmt
        .query_map(params![account_id], connection_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn account_has_connections(conn: &Connection, account_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM calendar_connections WHERE account_id = ?1)",
        params![account_id],
        |row| row.get(0),
    )?)
}

/// Accounts with at least one connection that syncs.
pub fn accounts_with_enabled_connections(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT account_id FROM calendar_connections
         WHERE enabled = 1 ORDER BY account_id",
    )?;
    let rows = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Every connection for account settings, with its calendars' names.
pub fn list_connections(conn: &Connection) -> Result<Vec<CalendarConnection>> {
    let mut names: HashMap<i64, Vec<String>> = HashMap::new();
    for calendar in list_calendars(conn, None)? {
        names
            .entry(calendar.connection_id)
            .or_default()
            .push(calendar.display_name.unwrap_or_default());
    }
    let mut stmt = conn.prepare(connection_select!(
        "ORDER BY account_id, kind = 'generic', id"
    ))?;
    let rows = stmt
        .query_map([], connection_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|cfg| CalendarConnection {
            calendars: names.remove(&cfg.id).unwrap_or_default(),
            id: cfg.id,
            account_id: cfg.account_id,
            kind: cfg.kind,
            base_url: cfg.base_url,
            username: cfg.username,
            enabled: cfg.enabled,
            last_error: cfg.last_error,
        })
        .collect())
}

pub fn set_connection_enabled(
    conn: &Connection,
    connection_id: i64,
    enabled: bool,
) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE calendar_connections SET enabled = ?2, last_error = NULL WHERE id = ?1",
        params![connection_id, enabled as i64],
    )? > 0)
}

pub fn set_connection_error(
    conn: &Connection,
    connection_id: i64,
    error: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE calendar_connections SET last_error = ?2 WHERE id = ?1",
        params![connection_id, error],
    )?;
    Ok(())
}

/// Drop a connection with its collections; see [`forget_calendar`] for what
/// happens to their events. A pending secret move goes with it.
pub fn remove_connection(conn: &Connection, connection_id: i64) -> Result<()> {
    let calendars = connection_calendar_ids(conn, connection_id)?;
    for calendar_id in calendars {
        forget_calendar(conn, calendar_id)?;
    }
    conn.execute(
        "DELETE FROM calendar_connections WHERE id = ?1",
        params![connection_id],
    )?;
    Ok(())
}

/// Connections whose CalDAV password still waits under its account's
/// pre-connection keyring entry, as (connection id, account id).
pub fn pending_secret_moves(conn: &Connection) -> Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT m.connection_id, k.account_id
         FROM calendar_secret_moves m
         JOIN calendar_connections k ON k.id = m.connection_id
         ORDER BY m.connection_id",
    )?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn secret_move_pending(conn: &Connection, connection_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM calendar_secret_moves WHERE connection_id = ?1)",
        params![connection_id],
        |row| row.get(0),
    )?)
}

pub fn finish_secret_move(conn: &Connection, connection_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM calendar_secret_moves WHERE connection_id = ?1",
        params![connection_id],
    )?;
    Ok(())
}

/// Upsert a discovered collection of `connection`; returns its id. Existing
/// sync state (ctag/sync_token/enabled) survives re-discovery.
pub fn upsert_calendar(
    conn: &Connection,
    connection: &ConnectionConfig,
    url: &str,
    display_name: Option<&str>,
    color: Option<&str>,
    read_only: bool,
) -> Result<i64> {
    upsert_calendar_with_initial_enabled(
        conn,
        connection,
        url,
        display_name,
        color,
        read_only,
        true,
    )
}

/// Insert a newly discovered collection with the provider's initial
/// visibility. Existing local visibility and sync state always survive
/// re-discovery. A URL another connection of the account already syncs is an
/// error: callers keep each collection with one connection.
pub fn upsert_calendar_with_initial_enabled(
    conn: &Connection,
    connection: &ConnectionConfig,
    url: &str,
    display_name: Option<&str>,
    color: Option<&str>,
    read_only: bool,
    enabled: bool,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO calendars
            (account_id, connection_id, url, display_name, color, read_only, enabled)
         VALUES (?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(account_id, url) DO UPDATE SET
            display_name = excluded.display_name,
            color = COALESCE(calendars.color, excluded.color),
            read_only = excluded.read_only
         WHERE calendars.connection_id = excluded.connection_id",
        params![
            connection.account_id,
            connection.id,
            url,
            display_name,
            color,
            read_only as i64,
            enabled as i64
        ],
    )?;
    conn.query_row(
        "SELECT id FROM calendars WHERE connection_id = ?1 AND url = ?2",
        params![connection.id, url],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

/// Forget the connection's collections that are no longer advertised.
pub fn retain_calendars(
    conn: &Connection,
    connection_id: i64,
    urls: &HashSet<String>,
) -> Result<()> {
    let mut statement = conn.prepare("SELECT id, url FROM calendars WHERE connection_id = ?1")?;
    let calendars = statement
        .query_map(params![connection_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (id, url) in calendars {
        if !urls.contains(&url) {
            forget_calendar(conn, id)?;
        }
    }
    Ok(())
}

/// Stop syncing a collection. Its server copies go with it; what exists only
/// here stays as local events: mail invitations (which keep their
/// invitation's state, tombstones included) and edits that never reached
/// the server. Detached rows are no longer pending, so they cannot surface
/// in another calendar.
pub fn forget_calendar(conn: &Connection, calendar_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE calendar_events
         SET calendar_id = NULL, caldav_href = NULL, etag = NULL, dirty = 0
         WHERE calendar_id = ?1
           AND (message_id IS NOT NULL OR (dirty = 1 AND deleted = 0))",
        params![calendar_id],
    )?;
    conn.execute(
        "DELETE FROM calendar_events WHERE calendar_id = ?1",
        params![calendar_id],
    )?;
    conn.execute("DELETE FROM calendars WHERE id = ?1", params![calendar_id])?;
    Ok(())
}

fn connection_calendar_ids(conn: &Connection, connection_id: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT id FROM calendars WHERE connection_id = ?1")?;
    let rows = stmt
        .query_map(params![connection_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn list_calendars(conn: &Connection, account_id: Option<i64>) -> Result<Vec<Calendar>> {
    let mut out = Vec::new();
    match account_id {
        Some(id) => {
            let mut stmt = conn.prepare(calendar_select!(
                "WHERE c.account_id = ?1
                 ORDER BY c.is_default DESC, c.display_name COLLATE NOCASE, c.id"
            ))?;
            for row in stmt.query_map(params![id], calendar_row)? {
                out.push(row?);
            }
        }
        None => {
            let mut stmt = conn.prepare(calendar_select!(
                "ORDER BY c.account_id, c.is_default DESC, c.display_name COLLATE NOCASE, c.id"
            ))?;
            for row in stmt.query_map([], calendar_row)? {
                out.push(row?);
            }
        }
    }
    Ok(out)
}

pub fn connection_calendars(conn: &Connection, connection_id: i64) -> Result<Vec<Calendar>> {
    let mut stmt = conn.prepare(calendar_select!(
        "WHERE c.connection_id = ?1
         ORDER BY c.is_default DESC, c.display_name COLLATE NOCASE, c.id"
    ))?;
    let rows = stmt
        .query_map(params![connection_id], calendar_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_calendar(conn: &Connection, calendar_id: i64) -> Result<Option<Calendar>> {
    conn.query_row(
        calendar_select!("WHERE c.id = ?1"),
        params![calendar_id],
        calendar_row,
    )
    .optional()
    .map_err(Into::into)
}

pub fn set_calendar_enabled(conn: &Connection, calendar_id: i64, enabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE calendars SET enabled = ?2 WHERE id = ?1",
        params![calendar_id, enabled as i64],
    )?;
    Ok(())
}

pub fn set_calendar_color(conn: &Connection, calendar_id: i64, color: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE calendars SET color = ?2 WHERE id = ?1",
        params![calendar_id, color],
    )?;
    Ok(())
}

/// Exactly one default (new-event target) per account, whichever of its
/// connections holds it.
pub fn set_default_calendar(conn: &Connection, account_id: i64, calendar_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE calendars SET is_default = (id = ?2) WHERE account_id = ?1",
        params![account_id, calendar_id],
    )?;
    Ok(())
}

/// Where a new event without a chosen calendar goes: the account's default,
/// else a writable calendar, preferring connections that sync.
pub fn default_calendar(conn: &Connection, account_id: i64) -> Result<Option<Calendar>> {
    conn.query_row(
        calendar_select!(
            "WHERE c.account_id = ?1 AND c.enabled = 1 AND c.read_only = 0
             ORDER BY c.is_default DESC, k.enabled DESC, c.id ASC LIMIT 1"
        ),
        params![account_id],
        calendar_row,
    )
    .optional()
    .map_err(Into::into)
}

/// Give the account a default calendar when it has none: `preferred` when it
/// is one of the account's writable calendars, else its first writable one.
pub fn ensure_default_calendar(
    conn: &Connection,
    account_id: i64,
    preferred: Option<i64>,
) -> Result<()> {
    let has_default: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM calendars WHERE account_id = ?1 AND is_default = 1)",
        params![account_id],
        |row| row.get(0),
    )?;
    if has_default {
        return Ok(());
    }
    let chosen: Option<i64> = conn
        .query_row(
            "SELECT c.id FROM calendars c
             JOIN calendar_connections k ON k.id = c.connection_id
             WHERE c.account_id = ?1 AND c.read_only = 0
             ORDER BY c.id = ?2 DESC, k.enabled DESC, c.enabled DESC, c.id
             LIMIT 1",
            params![account_id, preferred],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = chosen {
        set_default_calendar(conn, account_id, id)?;
    }
    Ok(())
}

pub fn set_sync_state(
    conn: &Connection,
    calendar_id: i64,
    ctag: Option<&str>,
    sync_token: Option<&str>,
    synced_at: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE calendars SET ctag = ?2, sync_token = ?3, last_synced_at = ?4 WHERE id = ?1",
        params![calendar_id, ctag, sync_token, synced_at],
    )?;
    Ok(())
}

/// (ctag, sync_token) as last stored.
pub fn sync_state(conn: &Connection, calendar_id: i64) -> Result<(Option<String>, Option<String>)> {
    conn.query_row(
        "SELECT ctag, sync_token FROM calendars WHERE id = ?1",
        params![calendar_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .map_err(Into::into)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::testutil;

    pub(crate) fn connection(account_id: i64, kind: CalendarConnectionKind) -> ConnectionConfig {
        ConnectionConfig {
            id: 0,
            account_id,
            kind,
            base_url: "https://dav.example.com/".into(),
            username: "me".into(),
            principal_url: None,
            home_set_url: None,
            enabled: true,
            last_error: None,
        }
    }

    /// Store a connection and return it with its id.
    pub(crate) fn add_connection(
        c: &Connection,
        account_id: i64,
        kind: CalendarConnectionKind,
    ) -> ConnectionConfig {
        let mut cfg = connection(account_id, kind);
        cfg.id = insert_connection(c, &cfg).unwrap();
        cfg
    }

    #[test]
    fn config_and_calendar_lifecycle() {
        let c = testutil::calendar_conn();
        let cfg = add_connection(&c, 1, CalendarConnectionKind::Generic);
        assert_eq!(accounts_with_enabled_connections(&c).unwrap(), vec![1]);

        let initially_hidden = upsert_calendar_with_initial_enabled(
            &c,
            &cfg,
            "https://dav.example.com/cal/hidden/",
            Some("Hidden"),
            None,
            true,
            false,
        )
        .unwrap();
        assert!(!get_calendar(&c, initially_hidden).unwrap().unwrap().enabled);
        // Provider re-discovery never overwrites the user's local choice.
        set_calendar_enabled(&c, initially_hidden, true).unwrap();
        upsert_calendar_with_initial_enabled(
            &c,
            &cfg,
            "https://dav.example.com/cal/hidden/",
            Some("Hidden renamed"),
            None,
            true,
            false,
        )
        .unwrap();
        assert!(get_calendar(&c, initially_hidden).unwrap().unwrap().enabled);
        set_calendar_enabled(&c, initially_hidden, false).unwrap();

        let a = upsert_calendar(
            &c,
            &cfg,
            "https://dav.example.com/cal/a/",
            Some("A"),
            None,
            false,
        )
        .unwrap();
        let b = upsert_calendar(
            &c,
            &cfg,
            "https://dav.example.com/cal/b/",
            Some("B"),
            None,
            false,
        )
        .unwrap();
        c.execute(
            "INSERT INTO calendar_events
             (account_id, ical_uid, starts_at, calendar_id, caldav_href, etag, dirty)
             VALUES (1, 'removed@test', 1000, ?1, '/cal/b/removed.ics', 'etag', 1)",
            params![b],
        )
        .unwrap();
        // Re-discovery keeps ids and sync state.
        set_sync_state(&c, a, Some("c1"), Some("t1"), 42).unwrap();
        let a2 = upsert_calendar(
            &c,
            &cfg,
            "https://dav.example.com/cal/a/",
            Some("A2"),
            None,
            false,
        )
        .unwrap();
        assert_eq!(a, a2);
        assert_eq!(
            sync_state(&c, a).unwrap(),
            (Some("c1".into()), Some("t1".into()))
        );
        let stored = get_calendar(&c, a).unwrap().unwrap();
        assert_eq!(
            (stored.connection_id, stored.kind),
            (cfg.id, CalendarConnectionKind::Generic)
        );

        // User-set color survives re-discovery.
        set_calendar_color(&c, a, Some("#dc2626")).unwrap();
        upsert_calendar(
            &c,
            &cfg,
            "https://dav.example.com/cal/a/",
            Some("A3"),
            Some("#123456"),
            false,
        )
        .unwrap();
        assert_eq!(
            get_calendar(&c, a).unwrap().unwrap().color.as_deref(),
            Some("#dc2626")
        );

        set_default_calendar(&c, 1, b).unwrap();
        assert_eq!(default_calendar(&c, 1).unwrap().unwrap().id, b);
        set_calendar_enabled(&c, b, false).unwrap();
        // Default falls back to an enabled, writable calendar.
        assert_eq!(default_calendar(&c, 1).unwrap().unwrap().id, a);

        retain_calendars(
            &c,
            cfg.id,
            &HashSet::from(["https://dav.example.com/cal/a/".into()]),
        )
        .unwrap();
        assert_eq!(list_calendars(&c, Some(1)).unwrap().len(), 1);
        let detached: (Option<i64>, Option<String>, Option<String>, bool) = c
            .query_row(
                "SELECT calendar_id, caldav_href, etag, dirty
                 FROM calendar_events WHERE ical_uid = 'removed@test'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, i64>(3)? != 0,
                    ))
                },
            )
            .unwrap();
        assert_eq!(detached, (None, None, None, false));

        remove_connection(&c, cfg.id).unwrap();
        assert!(list_connections(&c).unwrap().is_empty());
        assert!(list_calendars(&c, Some(1)).unwrap().is_empty());
    }

    #[test]
    fn one_account_syncs_several_servers() {
        let c = testutil::calendar_conn();
        let google = add_connection(&c, 1, CalendarConnectionKind::Google);
        let shared = add_connection(&c, 1, CalendarConnectionKind::Generic);
        let other = add_connection(&c, 1, CalendarConnectionKind::Generic);
        assert!(
            insert_connection(&c, &connection(1, CalendarConnectionKind::Google)).is_err(),
            "an account signs in to Google once"
        );
        let mine = upsert_calendar(&c, &google, "https://g/me", Some("Me"), None, false).unwrap();
        upsert_calendar(
            &c,
            &shared,
            "https://dav/shared/",
            Some("Team"),
            None,
            false,
        )
        .unwrap();
        // The same collection stays with the connection that synced it first.
        assert!(upsert_calendar(&c, &other, "https://g/me", Some("Me"), None, false).is_err());
        assert_eq!(
            get_calendar(&c, mine).unwrap().unwrap().connection_id,
            google.id
        );

        let listed = list_connections(&c).unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|connection| (connection.id, connection.calendars.clone()))
                .collect::<Vec<_>>(),
            vec![
                (google.id, vec!["Me".to_owned()]),
                (shared.id, vec!["Team".to_owned()]),
                (other.id, Vec::new()),
            ]
        );
        assert_eq!(
            account_connections(&c, 1)
                .unwrap()
                .iter()
                .map(|connection| connection.id)
                .collect::<Vec<_>>(),
            vec![google.id, shared.id, other.id]
        );

        ensure_default_calendar(&c, 1, None).unwrap();
        assert_eq!(default_calendar(&c, 1).unwrap().unwrap().id, mine);
        remove_connection(&c, google.id).unwrap();
        assert!(default_calendar(&c, 1).unwrap().is_some());
        ensure_default_calendar(&c, 1, None).unwrap();
        let calendars = list_calendars(&c, Some(1)).unwrap();
        assert_eq!(calendars.len(), 1);
        assert!(calendars[0].is_default);
        assert_eq!(calendars[0].connection_id, shared.id);
    }

    #[test]
    fn forgetting_a_calendar_keeps_only_what_exists_here() {
        let c = testutil::calendar_conn();
        let cfg = add_connection(&c, 1, CalendarConnectionKind::Generic);
        let calendar =
            upsert_calendar(&c, &cfg, "https://dav/cal/", Some("Cal"), None, false).unwrap();
        for (uid, message_id, dirty, deleted) in [
            ("synced", None, 0, 0),
            ("edited", None, 1, 0),
            ("deleting", None, 0, 1),
            ("invite", Some(7), 0, 0),
            ("declined-invite", Some(8), 1, 1),
        ] {
            c.execute(
                "INSERT INTO calendar_events
                    (account_id, ical_uid, starts_at, calendar_id, caldav_href, etag,
                     message_id, dirty, deleted)
                 VALUES (1, ?1, 1000, ?2, '/cal/' || ?1, 'etag', ?3, ?4, ?5)",
                params![uid, calendar, message_id, dirty, deleted],
            )
            .unwrap();
        }

        remove_connection(&c, cfg.id).unwrap();

        // (uid, calendar, href, dirty, deleted)
        type EventState = (String, Option<i64>, Option<String>, bool, bool);
        let rows: Vec<EventState> = c
            .prepare(
                "SELECT ical_uid, calendar_id, caldav_href, dirty, deleted
                 FROM calendar_events ORDER BY ical_uid",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, i64>(4)? != 0,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("declined-invite".into(), None, None, false, true),
                ("edited".into(), None, None, false, false),
                ("invite".into(), None, None, false, false),
            ]
        );
        assert!(get_connection(&c, cfg.id).unwrap().is_none());
    }

    #[test]
    fn secret_moves_finish_once_and_leave_with_their_connection() {
        let c = testutil::calendar_conn();
        let first = add_connection(&c, 1, CalendarConnectionKind::Generic);
        let second = add_connection(&c, 2, CalendarConnectionKind::Generic);
        c.execute(
            "INSERT INTO calendar_secret_moves (connection_id) VALUES (?1), (?2)",
            params![first.id, second.id],
        )
        .unwrap();
        assert_eq!(
            pending_secret_moves(&c).unwrap(),
            vec![(first.id, 1), (second.id, 2)]
        );
        finish_secret_move(&c, first.id).unwrap();
        assert!(!secret_move_pending(&c, first.id).unwrap());
        remove_connection(&c, second.id).unwrap();
        assert!(pending_secret_moves(&c).unwrap().is_empty());
    }
}
