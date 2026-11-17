//! Where each account's rules run, as last checked (#238).
use crate::error::Result;
use crate::models::ServerRules;
use rusqlite::{Connection, OptionalExtension, Row, params};

const COLUMNS: &str = "account_id, state_json, on_server_json, here_json, checked_at";

fn server_rules(row: &Row<'_>) -> rusqlite::Result<Option<ServerRules>> {
    let json = |index: usize| row.get::<_, String>(index);
    let (Ok(state), Ok(on_server), Ok(here)) = (
        serde_json::from_str(&json(1)?),
        serde_json::from_str(&json(2)?),
        serde_json::from_str(&json(3)?),
    ) else {
        // Written by a later version: checked again soon.
        return Ok(None);
    };
    Ok(Some(ServerRules {
        account_id: row.get(0)?,
        state,
        on_server,
        here,
        checked_at: row.get(4)?,
    }))
}

/// Where account `account_id`'s rules run, once checked.
pub fn get(conn: &Connection, account_id: i64) -> Result<Option<ServerRules>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM server_rules WHERE account_id = ?1"),
            params![account_id],
            server_rules,
        )
        .optional()?
        .flatten())
}

/// Where each checked account's rules run, by account.
pub fn all(conn: &Connection) -> Result<Vec<ServerRules>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM server_rules ORDER BY account_id"
    ))?;
    let rows = stmt.query_map([], server_rules)?;
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

/// The digest of the rules this device last put on account `account_id`'s
/// server.
pub fn written(conn: &Connection, account_id: i64) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT written FROM server_rules WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

/// Keeps `checked` for its account, and `written` as the digest of the
/// rules this device put on the server, when it wrote them. Whether where
/// the rules run changed.
pub fn store(
    conn: &Connection,
    checked: &ServerRules,
    written: Option<Option<&str>>,
) -> Result<bool> {
    let before = get(conn, checked.account_id)?;
    conn.execute(
        "INSERT INTO server_rules
           (account_id, written, state_json, on_server_json, here_json, checked_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(account_id) DO UPDATE SET
           written = CASE WHEN ?7 THEN excluded.written ELSE written END,
           state_json = excluded.state_json,
           on_server_json = excluded.on_server_json,
           here_json = excluded.here_json,
           checked_at = excluded.checked_at",
        params![
            checked.account_id,
            written.flatten(),
            serde_json::to_string(&checked.state)?,
            serde_json::to_string(&checked.on_server)?,
            serde_json::to_string(&checked.here)?,
            checked.checked_at,
            written.is_some(),
        ],
    )?;
    Ok(before.is_none_or(|before| {
        before.state != checked.state
            || before.on_server != checked.on_server
            || before.here != checked.here
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;
    use crate::models::{RuleHere, RuleHereWhy, ServerRulesState};

    #[test]
    fn where_rules_run_is_kept_with_what_this_device_wrote() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let account_id = 1;
        assert_eq!(get(&conn, account_id).unwrap(), None);
        let checked = ServerRules {
            account_id,
            state: ServerRulesState::RunBefore("roundcube".into()),
            on_server: vec![],
            here: vec![RuleHere {
                rule_id: 4,
                why: RuleHereWhy::Pattern,
            }],
            checked_at: 10,
        };
        assert!(store(&conn, &checked, None).unwrap());
        assert_eq!(get(&conn, account_id).unwrap().as_ref(), Some(&checked));
        assert_eq!(written(&conn, account_id).unwrap(), None);
        // The same again, later, changes nothing that shows.
        let later = ServerRules {
            checked_at: 20,
            ..checked.clone()
        };
        assert!(!store(&conn, &later, Some(Some("abc"))).unwrap());
        assert_eq!(written(&conn, account_id).unwrap().as_deref(), Some("abc"));
        // Leaving the digest out keeps it.
        let runs = ServerRules {
            state: ServerRulesState::Runs,
            on_server: vec![4],
            here: vec![],
            ..later
        };
        assert!(store(&conn, &runs, None).unwrap());
        assert_eq!(written(&conn, account_id).unwrap().as_deref(), Some("abc"));
        assert_eq!(all(&conn).unwrap(), vec![runs.clone()]);
        assert!(!store(&conn, &runs, Some(None)).unwrap());
        assert_eq!(written(&conn, account_id).unwrap(), None);
    }
}
