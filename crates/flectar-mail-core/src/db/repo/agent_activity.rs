//! The activity log of AI agents using mail through the local MCP server
//! (#237): what each did, never with message content.
use crate::error::Result;
use crate::models::{AgentActivity, AgentOutcome};
use rusqlite::{Connection, params};

/// Entries kept. The oldest go once there are more.
pub const KEPT: i64 = 500;

/// A new entry, before it has an id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAgentActivity {
    pub occurred_at: i64,
    pub agent: String,
    pub tool: String,
    pub account_id: Option<i64>,
    pub folder_id: Option<i64>,
    pub items: i64,
    pub outcome: AgentOutcome,
}

/// Add `entry`, keeping the newest `KEPT` entries. An account or folder that
/// no longer exists is left out, so the entry stays.
pub fn record(conn: &Connection, entry: &NewAgentActivity) -> Result<()> {
    conn.execute(
        "INSERT INTO agent_activity
           (occurred_at, agent, tool, account_id, folder_id, items, outcome)
         VALUES (?1, ?2, ?3,
                 (SELECT id FROM accounts WHERE id = ?4),
                 (SELECT id FROM folders WHERE id = ?5),
                 ?6, ?7)",
        params![
            entry.occurred_at,
            entry.agent,
            entry.tool,
            entry.account_id,
            entry.folder_id,
            entry.items,
            entry.outcome.as_str(),
        ],
    )?;
    conn.execute(
        "DELETE FROM agent_activity WHERE id NOT IN
           (SELECT id FROM agent_activity ORDER BY occurred_at DESC, id DESC LIMIT ?1)",
        params![KEPT],
    )?;
    Ok(())
}

/// The newest `limit` entries, newest first.
pub fn list(conn: &Connection, limit: i64) -> Result<Vec<AgentActivity>> {
    let mut stmt = conn.prepare(
        "SELECT id, occurred_at, agent, tool, account_id, folder_id, items, outcome
         FROM agent_activity ORDER BY occurred_at DESC, id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |r| {
        Ok(AgentActivity {
            id: r.get(0)?,
            occurred_at: r.get(1)?,
            agent: r.get(2)?,
            tool: r.get(3)?,
            account_id: r.get(4)?,
            folder_id: r.get(5)?,
            items: r.get(6)?,
            outcome: AgentOutcome::from_column(&r.get::<_, String>(7)?),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn clear(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM agent_activity", [])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    fn entry(occurred_at: i64, account_id: Option<i64>) -> NewAgentActivity {
        NewAgentActivity {
            occurred_at,
            agent: "Test agent".into(),
            tool: "search_messages".into(),
            account_id,
            folder_id: None,
            items: 3,
            outcome: AgentOutcome::Done,
        }
    }

    #[test]
    fn the_log_keeps_the_newest_entries() {
        let conn = testutil::conn();
        for at in 0..KEPT + 5 {
            record(&conn, &entry(at, None)).unwrap();
        }
        let entries = list(&conn, KEPT + 10).unwrap();
        assert_eq!(entries.len() as i64, KEPT);
        assert_eq!(entries[0].occurred_at, KEPT + 4);
        assert_eq!(entries.last().unwrap().occurred_at, 5);
        assert_eq!(entries[0].items, 3);
        assert_eq!(entries[0].outcome, AgentOutcome::Done);
        clear(&conn).unwrap();
        assert!(list(&conn, 10).unwrap().is_empty());
    }

    #[test]
    fn entries_outlive_their_account() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        record(&conn, &entry(1, Some(1))).unwrap();
        // An account that never existed is left out rather than failing.
        record(&conn, &entry(2, Some(4242))).unwrap();
        conn.execute("DELETE FROM accounts WHERE id = 1", [])
            .unwrap();
        let entries = list(&conn, 10).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.account_id.is_none()));
    }
}
