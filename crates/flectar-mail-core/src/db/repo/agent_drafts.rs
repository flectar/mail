//! Drafts AI agents wrote through the local MCP server (#237), so an agent
//! can change its own draft but never one the user wrote or edited since.
//! `Core::save_draft_as` records and forgets them in its save transaction.
use crate::error::Result;
use rusqlite::{Connection, params};

/// Remembers `draft_id` as an agent's, as it stands now.
pub fn record(conn: &Connection, draft_id: i64, agent: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO agent_drafts (draft_id, agent, saved_at)
         SELECT id, ?2, date FROM messages WHERE id = ?1 AND is_draft = 1
         ON CONFLICT(draft_id) DO UPDATE SET agent = excluded.agent,
                                             saved_at = excluded.saved_at",
        params![draft_id, agent],
    )?;
    Ok(())
}

/// Forgets `draft_id` as an agent's, once someone else saved it.
pub fn forget(conn: &Connection, draft_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM agent_drafts WHERE draft_id = ?1",
        params![draft_id],
    )?;
    Ok(())
}

/// Whether `draft_id` is still a draft an agent wrote, unchanged since.
pub fn unchanged(conn: &Connection, draft_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM agent_drafts d
                        JOIN messages m ON m.id = d.draft_id
                        WHERE d.draft_id = ?1 AND m.is_draft = 1
                          AND m.date = d.saved_at)",
        params![draft_id],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    #[test]
    fn an_agent_keeps_its_draft_only_until_someone_else_saves_it() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let (_, draft) = testutil::seed_message(&conn, "me@test.dev", "Plans", false);
        let (_, mail) = testutil::seed_message(&conn, "a@example.com", "Hello", false);
        conn.execute("UPDATE messages SET is_draft = 1 WHERE id = ?1", [draft])
            .unwrap();

        assert!(!unchanged(&conn, draft).unwrap());
        record(&conn, draft, "Test agent").unwrap();
        assert!(unchanged(&conn, draft).unwrap());
        // Received mail is never recorded as a draft.
        record(&conn, mail, "Test agent").unwrap();
        assert!(!unchanged(&conn, mail).unwrap());

        // The user saved it again, from the composer.
        forget(&conn, draft).unwrap();
        assert!(!unchanged(&conn, draft).unwrap());
        record(&conn, draft, "Test agent").unwrap();
        // It changed some other way, such as in another app.
        conn.execute("UPDATE messages SET date = date + 1 WHERE id = ?1", [draft])
            .unwrap();
        assert!(!unchanged(&conn, draft).unwrap());
        // The agent's next save takes it back.
        record(&conn, draft, "Test agent").unwrap();
        assert!(unchanged(&conn, draft).unwrap());
        // Sent, it's no draft anymore.
        conn.execute("UPDATE messages SET is_draft = 0 WHERE id = ?1", [draft])
            .unwrap();
        assert!(!unchanged(&conn, draft).unwrap());
    }
}
