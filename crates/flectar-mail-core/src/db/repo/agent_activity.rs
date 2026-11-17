//! The activity log of AI agents using mail through the local MCP server
//! (#237): what each did, never with message content.
use crate::error::Result;
use crate::models::{AgentActivity, AgentOutcome};
use rusqlite::{Connection, params};

/// Entries kept. The oldest go once there are more.
pub const KEPT: i64 = 500;

/// How long an entry's changes can be undone, as long as done actions are
/// kept.
pub const UNDO_FOR_MS: i64 = 7 * 24 * 60 * 60 * 1000;

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
    /// The queued actions that carried out its changes, for undo.
    pub action_ids: Vec<i64>,
    /// The labels it added or removed, for undo.
    pub label_changes: Vec<LabelChange>,
}

/// A label an agent added to a message or removed from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelChange {
    pub message_id: i64,
    pub label_id: i64,
    pub added: bool,
}

/// What undoing an entry takes back.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Undo {
    pub action_ids: Vec<i64>,
    pub label_changes: Vec<LabelChange>,
}

impl Undo {
    pub fn is_empty(&self) -> bool {
        self.action_ids.is_empty() && self.label_changes.is_empty()
    }
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
    let activity_id = conn.last_insert_rowid();
    for action_id in &entry.action_ids {
        conn.execute(
            "INSERT OR IGNORE INTO agent_activity_actions (activity_id, action_id)
             SELECT ?1, id FROM pending_actions WHERE id = ?2",
            params![activity_id, action_id],
        )?;
    }
    for change in &entry.label_changes {
        conn.execute(
            "INSERT OR IGNORE INTO agent_activity_labels (activity_id, message_id, label_id, added)
             SELECT ?1, m.id, l.id, ?4 FROM messages m, labels l WHERE m.id = ?2 AND l.id = ?3",
            params![
                activity_id,
                change.message_id,
                change.label_id,
                change.added
            ],
        )?;
    }
    conn.execute(
        "DELETE FROM agent_activity WHERE id NOT IN
           (SELECT id FROM agent_activity ORDER BY occurred_at DESC, id DESC LIMIT ?1)",
        params![KEPT],
    )?;
    Ok(())
}

/// The newest `limit` entries, newest first, as of `now`.
pub fn list(conn: &Connection, limit: i64, now: i64) -> Result<Vec<AgentActivity>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT a.id, a.occurred_at, a.agent, a.tool, a.account_id, a.folder_id, a.items,
                a.outcome, a.undone_at IS NOT NULL,
                a.undone_at IS NULL AND (
                  EXISTS (SELECT 1 FROM agent_activity_actions l
                          JOIN pending_actions p ON p.id = l.action_id
                          WHERE l.activity_id = a.id AND {UNDOABLE})
                  OR (a.occurred_at > ?2 - {UNDO_FOR_MS}
                      AND EXISTS (SELECT 1 FROM agent_activity_labels c
                                  WHERE c.activity_id = a.id)))
         FROM agent_activity a ORDER BY a.occurred_at DESC, a.id DESC LIMIT ?1"
    ))?;
    let rows = stmt.query_map(params![limit, now], |r| {
        Ok(AgentActivity {
            id: r.get(0)?,
            occurred_at: r.get(1)?,
            agent: r.get(2)?,
            tool: r.get(3)?,
            account_id: r.get(4)?,
            folder_id: r.get(5)?,
            items: r.get(6)?,
            outcome: AgentOutcome::from_column(&r.get::<_, String>(7)?),
            undone: r.get(8)?,
            undoable: r.get(9)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// An action `p` that can still be taken back: queued or done, and not a
/// send. Done ones are kept for a week.
const UNDOABLE: &str = "p.kind != 'send' AND p.state IN ('pending', 'inflight', 'done')";

/// What undoing entry `activity_id` at `now` takes back: nothing once it's
/// undone, or once its changes are too old.
pub fn undoable(conn: &Connection, activity_id: i64, now: i64) -> Result<Undo> {
    let mut stmt = conn.prepare(&format!(
        "SELECT p.id FROM agent_activity a
         JOIN agent_activity_actions l ON l.activity_id = a.id
         JOIN pending_actions p ON p.id = l.action_id
         WHERE a.id = ?1 AND a.undone_at IS NULL AND {UNDOABLE}
         ORDER BY p.id"
    ))?;
    let action_ids = stmt
        .query_map(params![activity_id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut stmt = conn.prepare(&format!(
        "SELECT c.message_id, c.label_id, c.added FROM agent_activity a
         JOIN agent_activity_labels c ON c.activity_id = a.id
         WHERE a.id = ?1 AND a.undone_at IS NULL AND a.occurred_at > ?2 - {UNDO_FOR_MS}
         ORDER BY c.label_id, c.message_id"
    ))?;
    let label_changes = stmt
        .query_map(params![activity_id, now], |r| {
            Ok(LabelChange {
                message_id: r.get(0)?,
                label_id: r.get(1)?,
                added: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Undo {
        action_ids,
        label_changes,
    })
}

/// Marks entry `activity_id` undone at `now`, and gives what to take back:
/// nothing when there's nothing left to undo, so an entry is undone once.
pub fn claim_undo(conn: &Connection, activity_id: i64, now: i64) -> Result<Undo> {
    let undo = undoable(conn, activity_id, now)?;
    if !undo.is_empty() {
        conn.execute(
            "UPDATE agent_activity SET undone_at = ?2 WHERE id = ?1",
            params![activity_id, now],
        )?;
    }
    Ok(undo)
}

/// Gives back a claim whose undo didn't happen.
pub fn release_undo(conn: &Connection, activity_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE agent_activity SET undone_at = NULL WHERE id = ?1",
        params![activity_id],
    )?;
    Ok(())
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
            action_ids: Vec::new(),
            label_changes: Vec::new(),
        }
    }

    #[test]
    fn the_log_keeps_the_newest_entries() {
        let conn = testutil::conn();
        for at in 0..KEPT + 5 {
            record(&conn, &entry(at, None)).unwrap();
        }
        let entries = list(&conn, KEPT + 10, 10).unwrap();
        assert_eq!(entries.len() as i64, KEPT);
        assert_eq!(entries[0].occurred_at, KEPT + 4);
        assert_eq!(entries.last().unwrap().occurred_at, 5);
        assert_eq!(entries[0].items, 3);
        assert_eq!(entries[0].outcome, AgentOutcome::Done);
        clear(&conn).unwrap();
        assert!(list(&conn, 10, 10).unwrap().is_empty());
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
        let entries = list(&conn, 10, 10).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.account_id.is_none()));
    }

    #[test]
    fn an_entry_is_undone_through_the_actions_it_queued() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let action = |kind: &str, state: &str| {
            conn.execute(
                "INSERT INTO pending_actions (account_id, kind, state, created_at, finished_at)
                 VALUES (1, ?1, ?2, 0, 0)",
                params![kind, state],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let archived = action("archive", "done");
        let failed = action("archive", "failed");
        let mut archive = entry(1, Some(1));
        // An action that doesn't exist is left out.
        archive.action_ids = vec![archived, failed, 4242];
        record(&conn, &archive).unwrap();
        record(&conn, &entry(2, Some(1))).unwrap();

        let entries = list(&conn, 10, 10).unwrap();
        assert!(!entries[0].undoable, "a read changes nothing");
        assert!(entries[1].undoable && !entries[1].undone);
        let id = entries[1].id;
        assert_eq!(undoable(&conn, id, 10).unwrap().action_ids, [archived]);

        assert_eq!(claim_undo(&conn, id, 10).unwrap().action_ids, [archived]);
        let undone = &list(&conn, 10, 10).unwrap()[1];
        assert!(undone.undone && !undone.undoable);
        assert!(claim_undo(&conn, id, 10).unwrap().is_empty(), "undone once");
        release_undo(&conn, id).unwrap();
        assert!(list(&conn, 10, 10).unwrap()[1].undoable);

        // Done actions are cleared after a week, and their entry with them.
        let mut trash = entry(3, Some(1));
        trash.action_ids = vec![action("trash", "done")];
        record(&conn, &trash).unwrap();
        assert!(list(&conn, 10, 10).unwrap()[0].undoable);
        crate::db::repo::actions::gc(&conn, i64::MAX).unwrap();
        assert!(!list(&conn, 10, 10).unwrap()[0].undoable);
    }

    #[test]
    fn label_changes_are_undone_for_a_week() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let (_, message) = testutil::seed_message(&conn, "a@example.com", "Hello", false);
        conn.execute(
            "INSERT INTO labels (id, name, keyword) VALUES (7, 'Later', 'later')",
            [],
        )
        .unwrap();
        let mut labelled = entry(100, Some(1));
        labelled.label_changes = vec![
            LabelChange {
                message_id: message,
                label_id: 7,
                added: true,
            },
            // A label or message that doesn't exist is left out.
            LabelChange {
                message_id: message,
                label_id: 4242,
                added: false,
            },
        ];
        record(&conn, &labelled).unwrap();
        let id = list(&conn, 1, 100).unwrap()[0].id;

        let week_later = 100 + UNDO_FOR_MS;
        assert!(list(&conn, 1, week_later - 1).unwrap()[0].undoable);
        assert!(!list(&conn, 1, week_later).unwrap()[0].undoable);
        assert!(claim_undo(&conn, id, week_later).unwrap().is_empty());
        let undo = claim_undo(&conn, id, 200).unwrap();
        assert_eq!(
            undo.label_changes,
            [LabelChange {
                message_id: message,
                label_id: 7,
                added: true,
            }]
        );
        assert!(list(&conn, 1, 200).unwrap()[0].undone);
    }
}
