//! Passwords and sign-ins Flectar Mail Sync carries between the user's
//! devices (migration 039). Every secret here stays sealed for the group;
//! only the system credential store holds one in the clear.

use crate::error::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};

/// What this device shares for one of its accounts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Share {
    pub account_id: i64,
    /// `password` or `oauth`.
    pub kind: String,
    /// The kind of device it works on: `any` for a password.
    pub family: String,
    pub record_id: Option<[u8; 16]>,
    /// The secret sealed for the group, if this device shares one.
    pub sealed: Option<String>,
    /// A keyed digest of the secret, to see when it changes.
    pub fingerprint: Option<Vec<u8>>,
    /// Signed in on this device since: the shared sign-in gives way to it.
    pub fresh: bool,
    /// The shared sign-in failed here; the record says so.
    pub stale: bool,
    pub checked_at: i64,
}

fn share_row(row: &Row<'_>) -> rusqlite::Result<Share> {
    Ok(Share {
        account_id: row.get(0)?,
        kind: row.get(1)?,
        family: row.get(2)?,
        record_id: row.get(3)?,
        sealed: row.get(4)?,
        fingerprint: row.get(5)?,
        fresh: row.get(6)?,
        stale: row.get(7)?,
        checked_at: row.get(8)?,
    })
}

const SHARE_COLUMNS: &str =
    "account_id, kind, family, record_id, sealed, fingerprint, fresh, stale, checked_at";

pub fn shares(conn: &Connection) -> Result<Vec<Share>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {SHARE_COLUMNS} FROM sign_in_shares ORDER BY account_id, kind, family"
    ))?;
    let shares = statement
        .query_map([], share_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(shares)
}

pub fn share(
    conn: &Connection,
    account_id: i64,
    kind: &str,
    family: &str,
) -> Result<Option<Share>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {SHARE_COLUMNS} FROM sign_in_shares
                 WHERE account_id = ?1 AND kind = ?2 AND family = ?3"
            ),
            params![account_id, kind, family],
            share_row,
        )
        .optional()?)
}

pub fn put_share(conn: &Connection, share: &Share) -> Result<()> {
    conn.execute(
        "INSERT INTO sign_in_shares
           (account_id, kind, family, record_id, sealed, fingerprint, fresh, stale, checked_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (account_id, kind, family) DO UPDATE SET
           record_id = excluded.record_id, sealed = excluded.sealed,
           fingerprint = excluded.fingerprint, fresh = excluded.fresh,
           stale = excluded.stale, checked_at = excluded.checked_at",
        params![
            share.account_id,
            share.kind,
            share.family,
            share.record_id,
            share.sealed,
            share.fingerprint,
            share.fresh,
            share.stale,
            share.checked_at
        ],
    )?;
    Ok(())
}

/// Stop sharing it: its record goes for every device.
pub fn forget_share(conn: &Connection, account_id: i64, kind: &str, family: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM sign_in_shares WHERE account_id = ?1 AND kind = ?2 AND family = ?3",
        params![account_id, kind, family],
    )?;
    Ok(())
}

/// A password or sign-in was entered on this device: read it again, and
/// share it in place of the one the group has.
pub fn mark_fresh(conn: &Connection, account_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE sign_in_shares SET fresh = 1, checked_at = 0 WHERE account_id = ?1",
        params![account_id],
    )?;
    Ok(())
}

/// The share this device keeps under `record_id`, if it took that record
/// on: shared it, or adopted what another device shared.
pub fn share_of_record(conn: &Connection, record_id: &[u8; 16]) -> Result<Option<Share>> {
    Ok(conn
        .query_row(
            &format!("SELECT {SHARE_COLUMNS} FROM sign_in_shares WHERE record_id = ?1"),
            params![&record_id[..]],
            share_row,
        )
        .optional()?)
}

/// Another device shared `sealed` under a record this device keeps: keep
/// what the group has, so this device never sends back an older one.
pub fn follow_record(conn: &Connection, record_id: &[u8; 16], sealed: &str) -> Result<()> {
    conn.execute(
        "UPDATE sign_in_shares SET sealed = ?2, stale = 0 WHERE record_id = ?1",
        params![&record_id[..], sealed],
    )?;
    Ok(())
}

/// Another device found the shared sign-in stale: this device says so too,
/// and reads its own again, which may work and go up in its place.
pub fn follow_stale(
    conn: &Connection,
    record_id: &[u8; 16],
    rejected: Option<&[u8]>,
) -> Result<()> {
    conn.execute(
        "UPDATE sign_in_shares
         SET sealed = NULL, stale = 1, fingerprint = ?2, checked_at = 0
         WHERE record_id = ?1",
        params![&record_id[..], rejected],
    )?;
    Ok(())
}

/// Sign-in sync was turned on again: read every share's credential store.
pub fn recheck_all(conn: &Connection) -> Result<()> {
    conn.execute("UPDATE sign_in_shares SET checked_at = 0", [])?;
    Ok(())
}

/// The record went: what this device shared with it is shared no more,
/// until it reads the credential store again.
pub fn forget_record(conn: &Connection, record_id: &[u8; 16]) -> Result<()> {
    conn.execute(
        "UPDATE sign_in_shares
         SET sealed = NULL, fingerprint = NULL, stale = 0, checked_at = 0
         WHERE record_id = ?1",
        params![&record_id[..]],
    )?;
    Ok(())
}

/// A secret another device shared, waiting for this device's credential
/// store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrival {
    pub account_id: i64,
    pub kind: String,
    pub family: String,
    pub record_id: [u8; 16],
    pub sealed: String,
}

pub fn put_arrival(conn: &Connection, arrival: &Arrival) -> Result<()> {
    conn.execute(
        "INSERT INTO sign_in_arrivals (account_id, kind, family, record_id, sealed)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (account_id, kind, family) DO UPDATE SET
           record_id = excluded.record_id, sealed = excluded.sealed",
        params![
            arrival.account_id,
            arrival.kind,
            arrival.family,
            arrival.record_id,
            arrival.sealed
        ],
    )?;
    Ok(())
}

pub fn arrivals(conn: &Connection) -> Result<Vec<Arrival>> {
    let mut statement = conn.prepare(
        "SELECT account_id, kind, family, record_id, sealed FROM sign_in_arrivals
         ORDER BY account_id, kind, family",
    )?;
    let arrivals = statement
        .query_map([], |row| {
            Ok(Arrival {
                account_id: row.get(0)?,
                kind: row.get(1)?,
                family: row.get(2)?,
                record_id: row.get(3)?,
                sealed: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(arrivals)
}

/// Handled: installed, or not for this device.
pub fn drop_arrival(conn: &Connection, arrival: &Arrival) -> Result<()> {
    conn.execute(
        "DELETE FROM sign_in_arrivals
         WHERE account_id = ?1 AND kind = ?2 AND family = ?3 AND sealed = ?4",
        params![
            arrival.account_id,
            arrival.kind,
            arrival.family,
            arrival.sealed
        ],
    )?;
    Ok(())
}

/// Another device took the record away.
pub fn drop_arrivals_of_record(conn: &Connection, record_id: &[u8; 16]) -> Result<()> {
    conn.execute(
        "DELETE FROM sign_in_arrivals WHERE record_id = ?1",
        params![&record_id[..]],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    #[test]
    fn shares_and_arrivals_round_trip() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let share = Share {
            account_id: 1,
            kind: "password".into(),
            family: "any".into(),
            record_id: Some([7; 16]),
            sealed: Some("{\"epoch\":1,\"data\":\"x\"}".into()),
            fingerprint: Some(vec![1, 2, 3]),
            fresh: false,
            stale: false,
            checked_at: 10,
        };
        put_share(&conn, &share).unwrap();
        assert_eq!(shares(&conn).unwrap(), std::slice::from_ref(&share));
        mark_fresh(&conn, 1).unwrap();
        let marked = super::share(&conn, 1, "password", "any").unwrap().unwrap();
        assert!(marked.fresh);
        assert_eq!(marked.checked_at, 0);
        forget_share(&conn, 1, "password", "any").unwrap();
        assert!(shares(&conn).unwrap().is_empty());

        let arrival = Arrival {
            account_id: 1,
            kind: "oauth".into(),
            family: "desktop".into(),
            record_id: [9; 16],
            sealed: "sealed".into(),
        };
        put_arrival(&conn, &arrival).unwrap();
        assert_eq!(arrivals(&conn).unwrap(), std::slice::from_ref(&arrival));
        // A newer one replaces it; dropping the older one leaves it.
        let newer = Arrival {
            sealed: "newer".into(),
            ..arrival.clone()
        };
        put_arrival(&conn, &newer).unwrap();
        drop_arrival(&conn, &arrival).unwrap();
        assert_eq!(arrivals(&conn).unwrap(), std::slice::from_ref(&newer));
        drop_arrivals_of_record(&conn, &[9; 16]).unwrap();
        assert!(arrivals(&conn).unwrap().is_empty());
    }
}
