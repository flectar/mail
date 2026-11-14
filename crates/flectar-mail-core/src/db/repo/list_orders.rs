//! The order each mail list is shown in. Lists are newest first unless the
//! user turned one around, so only those are kept, by the app's key for the
//! list.
use crate::error::Result;
use crate::models::{ListOrder, ListOwner};
use rusqlite::{Connection, params};

/// The keys of the lists shown oldest first.
pub fn oldest_first(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT list_key FROM oldest_first_lists ORDER BY list_key")?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Show the list with `key` in `order`. `owner` is what the list belongs to,
/// which takes the saved order with it when it goes.
pub fn set(conn: &Connection, key: &str, owner: ListOwner, order: ListOrder) -> Result<()> {
    match order {
        ListOrder::NewestFirst => {
            conn.execute(
                "DELETE FROM oldest_first_lists WHERE list_key = ?1",
                params![key],
            )?;
        }
        ListOrder::OldestFirst => {
            let (account_id, folder_id, label_id) = match owner {
                ListOwner::Unified => (None, None, None),
                ListOwner::Account(id) => (Some(id), None, None),
                ListOwner::Folder(id) => (None, Some(id), None),
                ListOwner::Label(id) => (None, None, Some(id)),
            };
            conn.execute(
                "INSERT INTO oldest_first_lists (list_key, account_id, folder_id, label_id)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(list_key) DO UPDATE SET
                   account_id = excluded.account_id,
                   folder_id = excluded.folder_id,
                   label_id = excluded.label_id",
                params![key, account_id, folder_id, label_id],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    fn oldest_first_now(conn: &Connection, key: &str, owner: ListOwner) {
        set(conn, key, owner, ListOrder::OldestFirst).unwrap();
    }

    fn newest_first_now(conn: &Connection, key: &str) {
        set(conn, key, ListOwner::Unified, ListOrder::NewestFirst).unwrap();
    }

    #[test]
    fn lists_turn_around_and_back() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        oldest_first_now(&conn, "Unified Inbox", ListOwner::Unified);
        oldest_first_now(&conn, "Folder:1", ListOwner::Folder(1));
        // Choosing the same order again keeps one entry.
        oldest_first_now(&conn, "Folder:1", ListOwner::Folder(1));
        assert_eq!(oldest_first(&conn).unwrap(), ["Folder:1", "Unified Inbox"]);

        newest_first_now(&conn, "Unified Inbox");
        // A list that was newest first already stays that way.
        newest_first_now(&conn, "Other");
        assert_eq!(oldest_first(&conn).unwrap(), ["Folder:1"]);
    }

    #[test]
    fn an_order_goes_with_the_folder_label_or_account_it_belongs_to() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, 'Projects', NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO labels (id, name, color, keyword, position, is_auto)
             VALUES (50, 'Travel', '#000000', 'travel', 50, 0)",
            [],
        )
        .unwrap();
        oldest_first_now(&conn, "Folder:2", ListOwner::Folder(2));
        oldest_first_now(&conn, "GlobalLabel:50", ListOwner::Label(50));
        oldest_first_now(&conn, "Account:1:Sent", ListOwner::Account(1));
        oldest_first_now(&conn, "Unified Sent", ListOwner::Unified);

        conn.execute("DELETE FROM folders WHERE id = 2", [])
            .unwrap();
        conn.execute("DELETE FROM labels WHERE id = 50", [])
            .unwrap();
        assert_eq!(
            oldest_first(&conn).unwrap(),
            ["Account:1:Sent", "Unified Sent"]
        );
        conn.execute("DELETE FROM accounts WHERE id = 1", [])
            .unwrap();
        assert_eq!(oldest_first(&conn).unwrap(), ["Unified Sent"]);
    }

    #[test]
    fn a_list_whose_owner_is_gone_is_not_saved() {
        let conn = testutil::conn();
        let saved = set(
            &conn,
            "Folder:9",
            ListOwner::Folder(9),
            ListOrder::OldestFirst,
        );
        assert!(saved.is_err());
        assert!(oldest_first(&conn).unwrap().is_empty());
    }
}
