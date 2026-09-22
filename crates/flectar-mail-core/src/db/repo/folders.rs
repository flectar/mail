use crate::error::Result;
use crate::models::FolderInfo;
use rusqlite::{Connection, OptionalExtension, Row, params};

#[derive(Debug, Clone)]
pub struct Folder {
    pub id: i64,
    pub account_id: i64,
    pub parent_id: Option<i64>,
    pub imap_name: String,
    pub delimiter: Option<String>,
    pub role: Option<String>,
    pub uidvalidity: Option<i64>,
    pub uidnext: Option<i64>,
    pub highestmodseq: Option<i64>,
    pub last_seen_uid: i64,
    pub backfill_cursor: Option<i64>,
    pub backfill_done: bool,
    pub jmap_id: Option<String>,
    pub selectable: bool,
    pub can_create_children: bool,
    pub can_rename: bool,
    pub can_delete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderPermissions {
    pub can_create_children: bool,
    pub can_rename: bool,
    pub can_delete: bool,
}

impl FolderPermissions {
    pub const FULL: Self = Self {
        can_create_children: true,
        can_rename: true,
        can_delete: true,
    };
}

fn from_row(row: &Row) -> rusqlite::Result<Folder> {
    Ok(Folder {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        parent_id: row.get("parent_id")?,
        imap_name: row.get("imap_name")?,
        delimiter: row.get("delimiter")?,
        role: row.get("role")?,
        uidvalidity: row.get("uidvalidity")?,
        uidnext: row.get("uidnext")?,
        highestmodseq: row.get("highestmodseq")?,
        last_seen_uid: row.get("last_seen_uid")?,
        backfill_cursor: row.get("backfill_cursor")?,
        backfill_done: row.get::<_, i64>("backfill_done")? != 0,
        jmap_id: row.get("jmap_id")?,
        selectable: row.get::<_, i64>("selectable")? != 0,
        can_create_children: row.get::<_, i64>("can_create_children")? != 0,
        can_rename: row.get::<_, i64>("can_rename")? != 0,
        can_delete: row.get::<_, i64>("can_delete")? != 0,
    })
}

pub fn upsert(
    conn: &Connection,
    account_id: i64,
    imap_name: &str,
    delimiter: Option<&str>,
    role: Option<&str>,
) -> Result<i64> {
    upsert_discovered(
        conn,
        account_id,
        imap_name,
        delimiter,
        role,
        true,
        FolderPermissions::FULL,
    )
}

pub fn upsert_discovered(
    conn: &Connection,
    account_id: i64,
    imap_name: &str,
    delimiter: Option<&str>,
    role: Option<&str>,
    selectable: bool,
    permissions: FolderPermissions,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO folders (account_id, imap_name, delimiter, role, selectable,
                              can_create_children, can_rename, can_delete)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(account_id, imap_name)
         DO UPDATE SET delimiter = excluded.delimiter, role = excluded.role,
                       selectable = excluded.selectable,
                       can_create_children = excluded.can_create_children,
                       can_rename = excluded.can_rename,
                       can_delete = excluded.can_delete, jmap_id = NULL",
        params![
            account_id,
            imap_name,
            delimiter,
            role,
            selectable,
            permissions.can_create_children,
            permissions.can_rename,
            permissions.can_delete,
        ],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM folders WHERE account_id = ?1 AND imap_name = ?2",
        params![account_id, imap_name],
        |r| r.get(0),
    )?;
    Ok(id)
}

pub fn list(conn: &Connection, account_id: Option<i64>) -> Result<Vec<Folder>> {
    let mut stmt = conn.prepare(
        "SELECT f.id AS id, f.account_id AS account_id, f.parent_id AS parent_id,
                f.imap_name AS imap_name,
                f.delimiter AS delimiter, f.role AS role, f.uidvalidity AS uidvalidity,
                f.uidnext AS uidnext, f.highestmodseq AS highestmodseq,
                f.last_seen_uid AS last_seen_uid, f.backfill_cursor AS backfill_cursor,
                f.backfill_done AS backfill_done, f.jmap_id AS jmap_id,
                f.selectable AS selectable,
                f.can_create_children AS can_create_children,
                f.can_rename AS can_rename, f.can_delete AS can_delete
         FROM folders f
         JOIN accounts a ON a.id = f.account_id
         WHERE (?1 IS NULL OR f.account_id = ?1)
           AND ((a.mail_protocol = 'jmap' AND f.jmap_id IS NOT NULL)
             OR (a.mail_protocol <> 'jmap' AND f.jmap_id IS NULL))
         ORDER BY f.id",
    )?;
    Ok(stmt
        .query_map(params![account_id], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn list_info(conn: &Connection, account_id: Option<i64>) -> Result<Vec<FolderInfo>> {
    Ok(list(conn, account_id)?
        .into_iter()
        .map(|f| FolderInfo {
            id: f.id,
            account_id: f.account_id,
            parent_id: f.parent_id,
            display_name: if f.jmap_id.is_some() {
                f.imap_name.clone()
            } else {
                crate::imap::decode_mailbox_name(&f.imap_name)
            },
            is_jmap: f.jmap_id.is_some(),
            imap_name: f.imap_name,
            delimiter: f.delimiter,
            role: f.role,
            selectable: f.selectable,
            can_create_children: f.can_create_children,
            can_rename: f.can_rename,
            can_delete: f.can_delete,
        })
        .collect())
}

pub fn rename_tree(
    conn: &Connection,
    account_id: i64,
    old_prefix: &str,
    new_prefix: &str,
    delimiter: &str,
) -> Result<()> {
    let descendant_prefix = format!("{old_prefix}{delimiter}");
    let mut stmt = conn.prepare(
        "SELECT id, imap_name FROM folders
         WHERE account_id = ?1 AND (imap_name = ?2 OR imap_name LIKE ?3 ESCAPE '\\')
         ORDER BY LENGTH(imap_name)",
    )?;
    let escaped = descendant_prefix
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let rows = stmt
        .query_map(
            params![account_id, old_prefix, format!("{escaped}%")],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (id, old_name) in rows {
        let suffix = old_name.strip_prefix(old_prefix).unwrap_or_default();
        conn.execute(
            "UPDATE folders SET imap_name = ?2 WHERE id = ?1",
            params![id, format!("{new_prefix}{suffix}")],
        )?;
    }
    Ok(())
}

pub fn delete_tree(
    conn: &Connection,
    account_id: i64,
    prefix: &str,
    delimiter: &str,
) -> Result<()> {
    let descendant_prefix = format!("{prefix}{delimiter}");
    let escaped = descendant_prefix
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("{escaped}%");
    let mut stmt = conn.prepare(
        "SELECT DISTINCT m.thread_id
         FROM messages m JOIN folders f ON f.id = m.folder_id
         WHERE f.account_id = ?1 AND (f.imap_name = ?2 OR f.imap_name LIKE ?3 ESCAPE '\\')
           AND m.thread_id IS NOT NULL",
    )?;
    let thread_ids = stmt
        .query_map(params![account_id, prefix, pattern], |row| {
            row.get::<_, i64>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    conn.execute(
        "DELETE FROM messages WHERE folder_id IN (
           SELECT id FROM folders
           WHERE account_id = ?1 AND (imap_name = ?2 OR imap_name LIKE ?3 ESCAPE '\\')
         )",
        params![account_id, prefix, pattern],
    )?;
    conn.execute(
        "DELETE FROM folders
         WHERE account_id = ?1 AND (imap_name = ?2 OR imap_name LIKE ?3 ESCAPE '\\')",
        params![account_id, prefix, pattern],
    )?;
    for thread_id in thread_ids {
        super::threads::recompute(conn, thread_id)?;
    }
    Ok(())
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Folder>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, parent_id, imap_name, delimiter, role, uidvalidity, uidnext,
                highestmodseq, last_seen_uid, backfill_cursor, backfill_done, jmap_id, selectable,
                can_create_children, can_rename, can_delete
         FROM folders WHERE id = ?1",
    )?;
    Ok(stmt.query_row(params![id], from_row).optional()?)
}

pub fn by_role(conn: &Connection, account_id: i64, role: &str) -> Result<Option<Folder>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, parent_id, imap_name, delimiter, role, uidvalidity, uidnext,
                highestmodseq, last_seen_uid, backfill_cursor, backfill_done, jmap_id, selectable,
                can_create_children, can_rename, can_delete
         FROM folders
         WHERE account_id = ?1 AND role = ?2 AND selectable = 1
         LIMIT 1",
    )?;
    Ok(stmt
        .query_row(params![account_id, role], from_row)
        .optional()?)
}

pub fn by_jmap_role(conn: &Connection, account_id: i64, role: &str) -> Result<Option<Folder>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, parent_id, imap_name, delimiter, role, uidvalidity, uidnext,
                highestmodseq, last_seen_uid, backfill_cursor, backfill_done, jmap_id, selectable,
                can_create_children, can_rename, can_delete
         FROM folders
         WHERE account_id=?1 AND role=?2 AND jmap_id IS NOT NULL AND selectable = 1
         LIMIT 1",
    )?;
    Ok(stmt
        .query_row(params![account_id, role], from_row)
        .optional()?)
}

pub fn by_jmap_id(conn: &Connection, account_id: i64, jmap_id: &str) -> Result<Option<Folder>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, parent_id, imap_name, delimiter, role, uidvalidity, uidnext,
                highestmodseq, last_seen_uid, backfill_cursor, backfill_done, jmap_id, selectable,
                can_create_children, can_rename, can_delete
         FROM folders WHERE account_id = ?1 AND jmap_id = ?2 LIMIT 1",
    )?;
    Ok(stmt
        .query_row(params![account_id, jmap_id], from_row)
        .optional()?)
}

pub fn set_parent(conn: &Connection, folder_id: i64, parent_id: Option<i64>) -> Result<()> {
    conn.execute(
        "UPDATE folders SET parent_id = ?2 WHERE id = ?1",
        params![folder_id, parent_id],
    )?;
    Ok(())
}

pub fn set_permissions(
    conn: &Connection,
    folder_id: i64,
    permissions: FolderPermissions,
) -> Result<()> {
    conn.execute(
        "UPDATE folders
         SET can_create_children=?2, can_rename=?3, can_delete=?4
         WHERE id=?1",
        params![
            folder_id,
            permissions.can_create_children,
            permissions.can_rename,
            permissions.can_delete,
        ],
    )?;
    Ok(())
}

pub fn upsert_jmap(
    conn: &Connection,
    account_id: i64,
    jmap_id: &str,
    name: &str,
    role: Option<&str>,
) -> Result<i64> {
    if let Some(folder) = by_jmap_id(conn, account_id, jmap_id)? {
        if let Some(conflict_id) = conn
            .query_row(
                "SELECT id FROM folders
                 WHERE account_id=?1 AND imap_name=?2 AND id<>?3 AND jmap_id IS NULL",
                params![account_id, name, folder.id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            conn.execute(
                "UPDATE folders SET imap_name = imap_name || ' [removed ' || id || ']'
                 WHERE id=?1",
                params![conflict_id],
            )?;
        }
        let name = unique_jmap_name(conn, account_id, name, jmap_id, Some(folder.id))?;
        conn.execute(
            "UPDATE folders
             SET imap_name=?2, role=?3, selectable=1,
                 can_create_children=0, can_rename=0, can_delete=0
             WHERE id=?1",
            params![folder.id, name, role],
        )?;
        return Ok(folder.id);
    }
    let name = unique_jmap_name(conn, account_id, name, jmap_id, None)?;
    conn.execute(
        "INSERT INTO folders (account_id, imap_name, delimiter, role, jmap_id,
                              backfill_done, can_create_children, can_rename, can_delete)
         VALUES (?1, ?2, '/', ?3, ?4, 1, 0, 0, 0)
         ON CONFLICT(account_id, imap_name) DO UPDATE SET
           role = excluded.role, jmap_id = excluded.jmap_id,
           backfill_done = 1, selectable = 1,
           can_create_children = 0, can_rename = 0, can_delete = 0",
        params![account_id, name, role, jmap_id],
    )?;
    Ok(conn.query_row(
        "SELECT id FROM folders WHERE account_id = ?1 AND jmap_id = ?2",
        params![account_id, jmap_id],
        |row| row.get(0),
    )?)
}

fn unique_jmap_name(
    conn: &Connection,
    account_id: i64,
    proposed: &str,
    jmap_id: &str,
    exclude_id: Option<i64>,
) -> Result<String> {
    let conflict: Option<Option<String>> = conn
        .query_row(
            "SELECT jmap_id FROM folders
             WHERE account_id=?1 AND imap_name=?2 AND (?3 IS NULL OR id<>?3)",
            params![account_id, proposed, exclude_id],
            |row| row.get(0),
        )
        .optional()?;
    if conflict.is_none() || conflict == Some(None) {
        return Ok(proposed.to_owned());
    }
    let short_id = jmap_id.chars().take(8).collect::<String>();
    let base = format!("{proposed} [{short_id}]");
    let mut candidate = base.clone();
    let mut suffix = 2;
    while conn
        .query_row(
            "SELECT 1 FROM folders
             WHERE account_id=?1 AND imap_name=?2 AND (?3 IS NULL OR id<>?3)",
            params![account_id, candidate, exclude_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        candidate = format!("{base} {suffix}");
        suffix += 1;
    }
    Ok(candidate)
}

pub fn set_uid_state(
    conn: &Connection,
    id: i64,
    uidvalidity: Option<i64>,
    uidnext: Option<i64>,
    highestmodseq: Option<i64>,
) -> Result<()> {
    conn.execute(
        "UPDATE folders SET uidvalidity = ?2, uidnext = ?3, highestmodseq = ?4 WHERE id = ?1",
        params![id, uidvalidity, uidnext, highestmodseq],
    )?;
    Ok(())
}

pub fn set_last_seen_uid(conn: &Connection, id: i64, uid: i64) -> Result<()> {
    conn.execute(
        "UPDATE folders SET last_seen_uid = MAX(last_seen_uid, ?2) WHERE id = ?1",
        params![id, uid],
    )?;
    Ok(())
}

pub fn set_backfill(conn: &Connection, id: i64, cursor: Option<i64>, done: bool) -> Result<()> {
    conn.execute(
        "UPDATE folders SET backfill_cursor = ?2, backfill_done = ?3 WHERE id = ?1",
        params![id, cursor, done as i64],
    )?;
    Ok(())
}

/// Re-open historical work after an account's download window changes. Keep
/// the cursor so widening to All can continue below the oldest known UID.
pub fn reopen_backfill_for_account(conn: &Connection, account_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE folders SET backfill_done = 0 WHERE account_id = ?1",
        params![account_id],
    )?;
    Ok(())
}

/// UIDVALIDITY changed: drop all UID mappings for the folder (messages stay,
/// re-linked on next sync by Message-ID).
pub fn reset_uid_mappings(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE messages SET uid = NULL WHERE folder_id = ?1",
        params![id],
    )?;
    conn.execute(
        "UPDATE folders SET last_seen_uid = 0, uidnext = NULL, highestmodseq = NULL,
                            backfill_cursor = NULL, backfill_done = 0
         WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    #[test]
    fn folder_listing_exposes_only_the_active_transport_projection() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO folders(id,account_id,imap_name,role,jmap_id,backfill_done)
             VALUES(2,1,'JMAP Inbox','inbox','remote-inbox',1)",
            [],
        )
        .unwrap();

        assert_eq!(
            list(&conn, Some(1))
                .unwrap()
                .into_iter()
                .map(|folder| folder.id)
                .collect::<Vec<_>>(),
            vec![1]
        );
        conn.execute("UPDATE accounts SET mail_protocol='jmap' WHERE id=1", [])
            .unwrap();
        assert_eq!(
            list(&conn, Some(1))
                .unwrap()
                .into_iter()
                .map(|folder| folder.id)
                .collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn imap_upsert_reclaims_a_same_name_folder_from_jmap() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute("UPDATE folders SET jmap_id='remote-inbox' WHERE id=1", [])
            .unwrap();
        assert_eq!(
            upsert(&conn, 1, "INBOX", Some("/"), Some("inbox")).unwrap(),
            1
        );
        assert!(get(&conn, 1).unwrap().unwrap().jmap_id.is_none());
    }

    #[test]
    fn hierarchy_containers_are_listed_but_not_role_destinations() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let archive = upsert_discovered(
            &conn,
            1,
            "Archive",
            Some("/"),
            Some("archive"),
            false,
            FolderPermissions::FULL,
        )
        .unwrap();
        let year = upsert_discovered(
            &conn,
            1,
            "Archive/2025",
            Some("/"),
            None,
            false,
            FolderPermissions::FULL,
        )
        .unwrap();
        let leaf = upsert_discovered(
            &conn,
            1,
            "Archive/2025/GitHub",
            Some("/"),
            None,
            true,
            FolderPermissions::FULL,
        )
        .unwrap();
        set_parent(&conn, year, Some(archive)).unwrap();
        set_parent(&conn, leaf, Some(year)).unwrap();

        let listed = list_info(&conn, Some(1)).unwrap();
        let year_info = listed.iter().find(|folder| folder.id == year).unwrap();
        let leaf_info = listed.iter().find(|folder| folder.id == leaf).unwrap();
        assert!(!year_info.selectable);
        assert!(year_info.can_create_children);
        assert_eq!(year_info.parent_id, Some(archive));
        assert!(leaf_info.selectable);
        assert_eq!(leaf_info.parent_id, Some(year));
        assert!(by_role(&conn, 1, "archive").unwrap().is_none());
    }

    #[test]
    fn explicit_jmap_parent_is_exposed_to_the_sidebar_projection() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute("UPDATE accounts SET mail_protocol='jmap' WHERE id=1", [])
            .unwrap();
        let parent = upsert_jmap(&conn, 1, "parent", "Archive", Some("archive")).unwrap();
        let child = upsert_jmap(&conn, 1, "child", "Archive / 2025", None).unwrap();
        set_parent(&conn, child, Some(parent)).unwrap();

        let listed = list_info(&conn, Some(1)).unwrap();
        let child = listed.iter().find(|folder| folder.id == child).unwrap();
        assert_eq!(child.parent_id, Some(parent));
        assert!(!child.can_create_children);
        assert!(!child.can_rename);
        assert!(!child.can_delete);
    }

    #[test]
    fn distinct_jmap_mailboxes_with_the_same_display_path_are_not_merged() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let first = upsert_jmap(&conn, 1, "remote-one", "Projects / Release", None).unwrap();
        let second = upsert_jmap(&conn, 1, "remote-two", "Projects / Release", None).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            by_jmap_id(&conn, 1, "remote-one").unwrap().unwrap().id,
            first
        );
        assert_eq!(
            by_jmap_id(&conn, 1, "remote-two").unwrap().unwrap().id,
            second
        );
        assert_ne!(
            get(&conn, first).unwrap().unwrap().imap_name,
            get(&conn, second).unwrap().unwrap().imap_name
        );
    }

    #[test]
    fn folder_info_decodes_imap_names_but_keeps_jmap_unicode_verbatim() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let imap = upsert(&conn, 1, "&Jjo- Projects", Some("/"), None).unwrap();
        let jmap = upsert_jmap(&conn, 1, "remote", "A&-B", None).unwrap();

        let all = list_info(&conn, Some(1)).unwrap();
        assert_eq!(
            all.iter()
                .find(|folder| folder.id == imap)
                .unwrap()
                .display_name,
            "☺ Projects"
        );
        conn.execute("UPDATE accounts SET mail_protocol='jmap' WHERE id=1", [])
            .unwrap();
        let all = list_info(&conn, Some(1)).unwrap();
        assert_eq!(
            all.iter()
                .find(|folder| folder.id == jmap)
                .unwrap()
                .display_name,
            "A&-B"
        );
    }

    #[test]
    fn renaming_a_folder_updates_its_descendant_paths() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let parent = upsert(&conn, 1, "Projects", Some("/"), None).unwrap();
        let child = upsert(&conn, 1, "Projects/2026", Some("/"), None).unwrap();

        rename_tree(&conn, 1, "Projects", "Work", "/").unwrap();

        assert_eq!(get(&conn, parent).unwrap().unwrap().imap_name, "Work");
        assert_eq!(get(&conn, child).unwrap().unwrap().imap_name, "Work/2026");
    }
}
