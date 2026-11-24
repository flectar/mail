//! App keys Flectar Mail Sync carries between the user's devices of one kind
//! (migration 070): the Google and Microsoft app registrations entered in
//! Sign-in settings. They travel sealed for the group; this device's
//! settings hold the keys it uses.

use crate::error::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};

/// What this device shares, or took on, for one provider's keys.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Share {
    /// `gmail` or `microsoft`.
    pub provider: String,
    /// The kind of device the keys are for.
    pub family: String,
    pub record_id: Option<[u8; 16]>,
    /// The keys as the group has them, sealed.
    pub sealed: Option<String>,
    /// A keyed digest of the keys this device last shared or took on.
    pub fingerprint: Option<Vec<u8>>,
    /// Saved on this device since: they replace the group's.
    pub fresh: bool,
    /// Keys another device shared, waiting for this device's settings.
    pub arrived: Option<String>,
}

fn share_row(row: &Row<'_>) -> rusqlite::Result<Share> {
    Ok(Share {
        provider: row.get(0)?,
        family: row.get(1)?,
        record_id: row.get(2)?,
        sealed: row.get(3)?,
        fingerprint: row.get(4)?,
        fresh: row.get(5)?,
        arrived: row.get(6)?,
    })
}

const COLUMNS: &str = "provider, family, record_id, sealed, fingerprint, fresh, arrived";

pub fn all(conn: &Connection) -> Result<Vec<Share>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM app_key_shares ORDER BY provider, family"
    ))?;
    let shares = statement
        .query_map([], share_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(shares)
}

pub fn get(conn: &Connection, provider: &str, family: &str) -> Result<Option<Share>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM app_key_shares WHERE provider = ?1 AND family = ?2"),
            params![provider, family],
            share_row,
        )
        .optional()?)
}

/// Keys were saved on this device: they go up in place of the group's.
pub fn mark_fresh(conn: &Connection, provider: &str, family: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO app_key_shares (provider, family, fresh) VALUES (?1, ?2, 1)
         ON CONFLICT (provider, family) DO UPDATE SET fresh = 1",
        params![provider, family],
    )?;
    Ok(())
}

/// This device shares `sealed`, the keys with fingerprint `fingerprint`,
/// under `record_id`.
pub fn shared(
    conn: &Connection,
    provider: &str,
    family: &str,
    record_id: &[u8; 16],
    sealed: &str,
    fingerprint: &[u8],
) -> Result<()> {
    conn.execute(
        "INSERT INTO app_key_shares (provider, family, record_id, sealed, fingerprint)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (provider, family) DO UPDATE SET
           record_id = excluded.record_id, sealed = excluded.sealed,
           fingerprint = excluded.fingerprint, fresh = 0",
        params![provider, family, &record_id[..], sealed, fingerprint],
    )?;
    Ok(())
}

/// Saved again as the group has them: nothing new to share.
pub fn unmark_fresh(conn: &Connection, provider: &str, family: &str) -> Result<()> {
    conn.execute(
        "UPDATE app_key_shares SET fresh = 0 WHERE provider = ?1 AND family = ?2",
        params![provider, family],
    )?;
    Ok(())
}

/// Another device shared `sealed` under `record_id`: it waits for this
/// device's settings, and unless keys saved here go up in its place, this
/// device keeps what the group has, so it never sends back older keys.
pub fn arrive(
    conn: &Connection,
    provider: &str,
    family: &str,
    record_id: &[u8; 16],
    sealed: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO app_key_shares (provider, family, record_id, sealed, arrived)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT (provider, family) DO UPDATE SET
           record_id = excluded.record_id, arrived = excluded.arrived,
           sealed = CASE WHEN fresh = 1 THEN sealed ELSE excluded.sealed END",
        params![provider, family, &record_id[..], sealed],
    )?;
    Ok(())
}

/// The keys that arrived as `sealed` are in this device's settings now.
/// A newer arrival stays.
pub fn settle_arrival(
    conn: &Connection,
    provider: &str,
    family: &str,
    sealed: &str,
    fingerprint: &[u8],
) -> Result<()> {
    conn.execute(
        "UPDATE app_key_shares SET arrived = NULL, fingerprint = ?4
         WHERE provider = ?1 AND family = ?2 AND arrived = ?3",
        params![provider, family, sealed, fingerprint],
    )?;
    Ok(())
}

/// Keys that arrived as `sealed` that this device leaves out. A newer
/// arrival stays.
pub fn drop_arrival(conn: &Connection, provider: &str, family: &str, sealed: &str) -> Result<()> {
    conn.execute(
        "UPDATE app_key_shares SET arrived = NULL
         WHERE provider = ?1 AND family = ?2 AND arrived = ?3",
        params![provider, family, sealed],
    )?;
    Ok(())
}

/// The record went, as when app keys stop syncing: the keys already in this
/// device's settings stay, and the group's are no longer followed.
pub fn forget_record(conn: &Connection, record_id: &[u8; 16]) -> Result<()> {
    conn.execute(
        "UPDATE app_key_shares SET sealed = NULL, fingerprint = NULL, arrived = NULL
         WHERE record_id = ?1",
        params![&record_id[..]],
    )?;
    Ok(())
}

/// App keys sync again: every device offers its keys anew, and the first
/// ones the group has are the ones every device takes on.
pub fn offer_again(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE app_key_shares SET sealed = NULL, fingerprint = NULL, arrived = NULL",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    const SEALED: &str = r#"{"epoch":1,"data":"AAAA"}"#;
    const NEWER: &str = r#"{"epoch":1,"data":"BBBB"}"#;

    #[test]
    fn keys_saved_here_go_up_until_shared() {
        let conn = testutil::conn();
        mark_fresh(&conn, "gmail", "android").unwrap();
        let share = get(&conn, "gmail", "android").unwrap().unwrap();
        assert!(share.fresh);
        assert_eq!(share.sealed, None);

        shared(&conn, "gmail", "android", &[1; 16], SEALED, &[9; 32]).unwrap();
        let share = get(&conn, "gmail", "android").unwrap().unwrap();
        assert!(!share.fresh);
        assert_eq!(share.record_id, Some([1; 16]));
        assert_eq!(share.sealed.as_deref(), Some(SEALED));
        assert_eq!(share.fingerprint.as_deref(), Some(&[9; 32][..]));
        assert_eq!(get(&conn, "microsoft", "android").unwrap(), None);
    }

    #[test]
    fn arrivals_are_followed_unless_keys_saved_here_go_up() {
        let conn = testutil::conn();
        arrive(&conn, "microsoft", "android", &[2; 16], SEALED).unwrap();
        let share = get(&conn, "microsoft", "android").unwrap().unwrap();
        assert_eq!(share.sealed.as_deref(), Some(SEALED), "follows the group");
        assert_eq!(share.arrived.as_deref(), Some(SEALED));

        // Keys saved here meanwhile keep what this device shares.
        mark_fresh(&conn, "microsoft", "android").unwrap();
        arrive(&conn, "microsoft", "android", &[2; 16], NEWER).unwrap();
        let share = get(&conn, "microsoft", "android").unwrap().unwrap();
        assert_eq!(share.sealed.as_deref(), Some(SEALED));
        assert_eq!(share.arrived.as_deref(), Some(NEWER));
    }

    #[test]
    fn an_arrival_settles_only_as_it_was_installed() {
        let conn = testutil::conn();
        arrive(&conn, "gmail", "desktop", &[3; 16], SEALED).unwrap();
        // A newer one came in while the first was installed.
        arrive(&conn, "gmail", "desktop", &[3; 16], NEWER).unwrap();
        settle_arrival(&conn, "gmail", "desktop", SEALED, &[4; 32]).unwrap();
        drop_arrival(&conn, "gmail", "desktop", SEALED).unwrap();
        let share = get(&conn, "gmail", "desktop").unwrap().unwrap();
        assert_eq!(share.arrived.as_deref(), Some(NEWER));
        assert_eq!(share.fingerprint, None);

        settle_arrival(&conn, "gmail", "desktop", NEWER, &[5; 32]).unwrap();
        let share = get(&conn, "gmail", "desktop").unwrap().unwrap();
        assert_eq!(share.arrived, None);
        assert_eq!(share.fingerprint.as_deref(), Some(&[5; 32][..]));
    }

    #[test]
    fn a_record_that_went_is_no_longer_followed() {
        let conn = testutil::conn();
        shared(&conn, "gmail", "ios", &[6; 16], SEALED, &[7; 32]).unwrap();
        arrive(&conn, "microsoft", "ios", &[8; 16], NEWER).unwrap();
        forget_record(&conn, &[6; 16]).unwrap();
        let gone = get(&conn, "gmail", "ios").unwrap().unwrap();
        assert_eq!((gone.sealed, gone.fingerprint), (None, None));
        assert_eq!(gone.record_id, Some([6; 16]));
        let kept = get(&conn, "microsoft", "ios").unwrap().unwrap();
        assert_eq!(kept.arrived.as_deref(), Some(NEWER));

        offer_again(&conn).unwrap();
        assert!(
            all(&conn)
                .unwrap()
                .iter()
                .all(|share| share.sealed.is_none()
                    && share.arrived.is_none()
                    && share.fingerprint.is_none())
        );
    }
}
