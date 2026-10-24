use crate::error::Result;
use crate::models::{FolderInfo, MailProtocol, now_ms};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct Folder {
    pub id: i64,
    pub account_id: i64,
    pub parent_id: Option<i64>,
    pub path: String,
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
        path: row.get("path")?,
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
    path: &str,
    delimiter: Option<&str>,
    role: Option<&str>,
) -> Result<i64> {
    upsert_discovered(
        conn,
        account_id,
        path,
        delimiter,
        role,
        true,
        FolderPermissions::FULL,
    )
}

pub fn upsert_discovered(
    conn: &Connection,
    account_id: i64,
    path: &str,
    delimiter: Option<&str>,
    role: Option<&str>,
    selectable: bool,
    permissions: FolderPermissions,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO folders (account_id, path, delimiter, role, selectable,
                              can_create_children, can_rename, can_delete)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(account_id, path)
         DO UPDATE SET delimiter = excluded.delimiter, role = excluded.role,
                       selectable = excluded.selectable,
                       can_create_children = excluded.can_create_children,
                       can_rename = excluded.can_rename,
                       can_delete = excluded.can_delete, jmap_id = NULL,
                       missing_on_server = 0",
        params![
            account_id,
            path,
            delimiter,
            role,
            selectable,
            permissions.can_create_children,
            permissions.can_rename,
            permissions.can_delete,
        ],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM folders WHERE account_id = ?1 AND path = ?2",
        params![account_id, path],
        |r| r.get(0),
    )?;
    Ok(id)
}

pub fn list(conn: &Connection, account_id: Option<i64>) -> Result<Vec<Folder>> {
    let mut stmt = conn.prepare(
        "SELECT f.id AS id, f.account_id AS account_id, f.parent_id AS parent_id,
                f.path AS path,
                f.delimiter AS delimiter, f.role AS role, f.uidvalidity AS uidvalidity,
                f.uidnext AS uidnext, f.highestmodseq AS highestmodseq,
                f.last_seen_uid AS last_seen_uid, f.backfill_cursor AS backfill_cursor,
                f.backfill_done AS backfill_done, f.jmap_id AS jmap_id,
                f.selectable AS selectable,
                f.can_create_children AS can_create_children,
                f.can_rename AS can_rename, f.can_delete AS can_delete
         FROM folders f
         JOIN accounts a ON a.id = f.account_id
         WHERE (?1 IS NULL OR f.account_id = ?1) AND f.missing_on_server = 0
           AND ((a.mail_protocol = 'jmap' AND f.jmap_id IS NOT NULL)
             OR (a.mail_protocol <> 'jmap' AND f.jmap_id IS NULL))
         ORDER BY f.id",
    )?;
    Ok(stmt
        .query_map(params![account_id], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn list_info(conn: &Connection, account_id: Option<i64>) -> Result<Vec<FolderInfo>> {
    let protocols = conn
        .prepare("SELECT id, mail_protocol FROM accounts")?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, MailProtocol>(1)?))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(list(conn, account_id)?
        .into_iter()
        .map(|f| FolderInfo {
            id: f.id,
            account_id: f.account_id,
            parent_id: f.parent_id,
            // IMAP names are modified UTF-7 on the wire; JMAP names and local
            // POP3 folders are stored as shown.
            display_name: match protocols.get(&f.account_id) {
                Some(MailProtocol::Gmail | MailProtocol::Imap) => {
                    crate::imap::decode_mailbox_name(&f.path)
                }
                Some(MailProtocol::Jmap | MailProtocol::Pop3) | None => f.path.clone(),
            },
            is_jmap: f.jmap_id.is_some(),
            path: f.path,
            delimiter: f.delimiter,
            role: f.role,
            selectable: f.selectable,
            can_create_children: f.can_create_children,
            can_rename: f.can_rename,
            can_delete: f.can_delete,
        })
        .collect())
}

fn subtree_pattern(prefix: &str, delimiter: &str) -> String {
    let extra = prefix
        .bytes()
        .chain(delimiter.bytes())
        .filter(|byte| matches!(byte, b'\\' | b'%' | b'_'))
        .count();
    let mut pattern = String::with_capacity(prefix.len() + delimiter.len() + extra + 1);
    for character in prefix.chars().chain(delimiter.chars()) {
        if matches!(character, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

pub fn rename_tree(
    conn: &Connection,
    account_id: i64,
    old_prefix: &str,
    new_prefix: &str,
    delimiter: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, path FROM folders
         WHERE account_id = ?1 AND (path = ?2 OR path LIKE ?3 ESCAPE '\\')
         ORDER BY LENGTH(path)",
    )?;
    let pattern = subtree_pattern(old_prefix, delimiter);
    let rows = stmt
        .query_map(params![account_id, old_prefix, pattern], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (id, old_name) in rows {
        let suffix = old_name.strip_prefix(old_prefix).unwrap_or_default();
        conn.execute(
            "UPDATE folders SET path = ?2 WHERE id = ?1",
            params![id, format!("{new_prefix}{suffix}")],
        )?;
    }
    Ok(())
}

/// A message in a folder tree.
pub struct TreeMessage {
    pub id: i64,
    pub thread_id: Option<i64>,
    pub raw_path: Option<String>,
}

/// Every message in a folder and its subfolders.
pub fn messages_in_tree(
    conn: &Connection,
    account_id: i64,
    prefix: &str,
    delimiter: &str,
) -> Result<Vec<TreeMessage>> {
    let pattern = subtree_pattern(prefix, delimiter);
    let mut stmt = conn.prepare(
        "SELECT m.id, m.thread_id, m.raw_path
         FROM messages m JOIN folders f ON f.id = m.folder_id
         WHERE f.account_id = ?1 AND (f.path = ?2 OR f.path LIKE ?3 ESCAPE '\\')",
    )?;
    Ok(stmt
        .query_map(params![account_id, prefix, pattern], |row| {
            Ok(TreeMessage {
                id: row.get(0)?,
                thread_id: row.get(1)?,
                raw_path: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Delete a folder and its subfolders. Their messages go first, with their
/// search entries and files (see `crate::sync::delete_folder_tree`).
pub fn delete_tree(
    conn: &Connection,
    account_id: i64,
    prefix: &str,
    delimiter: &str,
) -> Result<()> {
    let pattern = subtree_pattern(prefix, delimiter);
    conn.execute(
        "DELETE FROM folders
         WHERE account_id = ?1 AND (path = ?2 OR path LIKE ?3 ESCAPE '\\')",
        params![account_id, prefix, pattern],
    )?;
    Ok(())
}

pub fn path_exists(conn: &Connection, account_id: i64, path: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM folders WHERE account_id = ?1 AND path = ?2)",
        params![account_id, path],
        |row| row.get(0),
    )?)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Folder>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, parent_id, path, delimiter, role, uidvalidity, uidnext,
                highestmodseq, last_seen_uid, backfill_cursor, backfill_done, jmap_id, selectable,
                can_create_children, can_rename, can_delete
         FROM folders WHERE id = ?1",
    )?;
    Ok(stmt.query_row(params![id], from_row).optional()?)
}

/// The account's folder for a purpose, among those `list` shows.
pub fn by_role(conn: &Connection, account_id: i64, role: &str) -> Result<Option<Folder>> {
    let mut stmt = conn.prepare(
        "SELECT f.id AS id, f.account_id AS account_id, f.parent_id AS parent_id,
                f.path AS path, f.delimiter AS delimiter, f.role AS role,
                f.uidvalidity AS uidvalidity, f.uidnext AS uidnext,
                f.highestmodseq AS highestmodseq, f.last_seen_uid AS last_seen_uid,
                f.backfill_cursor AS backfill_cursor, f.backfill_done AS backfill_done,
                f.jmap_id AS jmap_id, f.selectable AS selectable,
                f.can_create_children AS can_create_children,
                f.can_rename AS can_rename, f.can_delete AS can_delete
         FROM folders f
         JOIN accounts a ON a.id = f.account_id
         WHERE f.account_id = ?1 AND f.role = ?2 AND f.selectable = 1
           AND f.missing_on_server = 0
           AND (a.mail_protocol = 'jmap') = (f.jmap_id IS NOT NULL)
         ORDER BY f.id
         LIMIT 1",
    )?;
    Ok(stmt
        .query_row(params![account_id, role], from_row)
        .optional()?)
}

pub fn by_jmap_role(conn: &Connection, account_id: i64, role: &str) -> Result<Option<Folder>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, parent_id, path, delimiter, role, uidvalidity, uidnext,
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
        "SELECT id, account_id, parent_id, path, delimiter, role, uidvalidity, uidnext,
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
                 WHERE account_id=?1 AND path=?2 AND id<>?3 AND jmap_id IS NULL",
                params![account_id, name, folder.id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            conn.execute(
                "UPDATE folders SET path = path || ' [removed ' || id || ']'
                 WHERE id=?1",
                params![conflict_id],
            )?;
        }
        let name = unique_jmap_name(conn, account_id, name, jmap_id, Some(folder.id))?;
        conn.execute(
            "UPDATE folders
             SET path=?2, role=?3, selectable=1,
                 can_create_children=0, can_rename=0, can_delete=0
             WHERE id=?1",
            params![folder.id, name, role],
        )?;
        return Ok(folder.id);
    }
    let name = unique_jmap_name(conn, account_id, name, jmap_id, None)?;
    conn.execute(
        "INSERT INTO folders (account_id, path, delimiter, role, jmap_id,
                              backfill_done, can_create_children, can_rename, can_delete)
         VALUES (?1, ?2, '/', ?3, ?4, 1, 0, 0, 0)
         ON CONFLICT(account_id, path) DO UPDATE SET
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

/// Drops the folders of a JMAP account that no server mailbox stands for,
/// left by an earlier server or protocol, once nothing is in them. Mail that
/// stays there, such as the account's drafts and the sent mail only this
/// device holds, first moves to the account's folder for the same purpose.
/// Returns the threads of mail that moved.
pub fn drop_jmap_leftovers(conn: &Connection, account_id: i64) -> Result<Vec<i64>> {
    let moved = super::messages::rehome_mail(conn, account_id, "f.jmap_id IS NULL")?;
    conn.execute(
        "DELETE FROM folders WHERE account_id = ?1 AND jmap_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM messages WHERE messages.folder_id = folders.id)",
        params![account_id],
    )?;
    Ok(moved)
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
             WHERE account_id=?1 AND path=?2 AND (?3 IS NULL OR id<>?3)",
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
             WHERE account_id=?1 AND path=?2 AND (?3 IS NULL OR id<>?3)",
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

/// Marks the folders of an IMAP account that the server no longer lists,
/// except INBOX, which always exists. Their mail waits to be matched by
/// Message-ID with a copy elsewhere on the server, such as under the folder's
/// new name. Actions queued for it can no longer run, except sending and
/// filing what was sent, which don't need the folder, and headers that failed
/// in it can't be fetched again.
pub fn mark_missing_on_server(
    conn: &Connection,
    account_id: i64,
    listed: &HashSet<String>,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, path FROM folders
         WHERE account_id = ?1 AND jmap_id IS NULL AND missing_on_server = 0",
    )?;
    let missing = stmt
        .query_map(params![account_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(_, path)| !listed.contains(path) && !path.eq_ignore_ascii_case("INBOX"))
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    drop(stmt);
    for folder_id in missing {
        conn.execute(
            "UPDATE pending_actions SET state = 'cancelled', finished_at = ?2,
               last_error = 'the folder is no longer on the server'
             WHERE state IN ('pending', 'inflight')
               AND kind NOT IN ('send', 'append_sent')
               AND message_id IN (SELECT id FROM messages WHERE folder_id = ?1)",
            params![folder_id, now_ms()],
        )?;
        super::messages::await_relink(conn, account_id, Some(folder_id))?;
        conn.execute(
            "DELETE FROM sync_failures WHERE folder_id = ?1",
            params![folder_id],
        )?;
        conn.execute(
            "UPDATE folders SET missing_on_server = 1, uidvalidity = NULL, uidnext = NULL,
                    highestmodseq = NULL, last_seen_uid = 0, backfill_cursor = NULL,
                    backfill_done = 0
             WHERE id = ?1",
            params![folder_id],
        )?;
    }
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

/// UIDVALIDITY changed: drop all UID mappings for the folder. Its mail waits
/// to be re-linked by Message-ID, and headers that failed under the old UIDs
/// are no longer worth retrying.
pub fn reset_uid_mappings(conn: &Connection, id: i64) -> Result<()> {
    let account_id: i64 = conn.query_row(
        "SELECT account_id FROM folders WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    super::messages::await_relink(conn, account_id, Some(id))?;
    conn.execute(
        "DELETE FROM sync_failures WHERE folder_id = ?1 AND stage = 'header'",
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
            "INSERT INTO folders(id,account_id,path,role,jmap_id,backfill_done)
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
            get(&conn, first).unwrap().unwrap().path,
            get(&conn, second).unwrap().unwrap().path
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

        assert_eq!(get(&conn, parent).unwrap().unwrap().path, "Work");
        assert_eq!(get(&conn, child).unwrap().unwrap().path, "Work/2026");
    }

    fn relink_pending(conn: &Connection, id: i64) -> bool {
        conn.query_row(
            "SELECT relink_pending FROM messages WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn header_failures(conn: &Connection, folder_id: i64) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM sync_failures WHERE folder_id = ?1",
            params![folder_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn folders_the_server_stops_listing_hide_until_it_lists_them_again() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let projects = upsert(&conn, 1, "Projects", Some("/"), None).unwrap();
        set_uid_state(&conn, projects, Some(5), Some(10), None).unwrap();
        set_last_seen_uid(&conn, projects, 9).unwrap();
        set_backfill(&conn, projects, Some(1), true).unwrap();
        let notes = upsert_jmap(&conn, 1, "notes", "Notes", None).unwrap();
        let (_, received) = testutil::seed_message(&conn, "sender@example.com", "Plan", false);
        let (thread_id, sent) = testutil::seed_message(&conn, "me@test.dev", "Re: Plan", false);
        conn.execute(
            "UPDATE messages SET folder_id = ?1 WHERE id IN (?2, ?3)",
            params![projects, received, sent],
        )
        .unwrap();
        conn.execute(
            "UPDATE messages SET uid = NULL, is_outgoing = 1 WHERE id = ?1",
            params![sent],
        )
        .unwrap();
        let actions = [("star", received), ("append_sent", sent)].map(|(kind, id)| {
            crate::db::repo::actions::enqueue(
                &conn,
                1,
                kind,
                Some(id),
                Some(thread_id),
                &serde_json::json!({}),
                None,
            )
            .unwrap()
        });
        crate::db::repo::sync_failures::record_header(&conn, projects, 11, None, "bad header")
            .unwrap();

        // The server spells INBOX its own way, and JMAP folders aren't IMAP's.
        let listed = HashSet::from(["Inbox".to_owned()]);
        mark_missing_on_server(&conn, 1, &listed).unwrap();
        assert_eq!(
            list(&conn, Some(1))
                .unwrap()
                .into_iter()
                .map(|folder| folder.id)
                .collect::<Vec<_>>(),
            [1]
        );
        let folder = get(&conn, projects).unwrap().unwrap();
        assert_eq!(
            (
                folder.uidvalidity,
                folder.last_seen_uid,
                folder.backfill_cursor
            ),
            (None, 0, None)
        );
        assert!(!folder.backfill_done);
        assert!(get(&conn, notes).unwrap().is_some());
        let missing = |id: i64| {
            conn.query_row(
                "SELECT missing_on_server FROM folders WHERE id = ?1",
                params![id],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
        };
        assert!(missing(projects));
        assert!(!missing(notes));
        assert!(!missing(1));
        assert!(relink_pending(&conn, received));
        assert_eq!(
            crate::db::repo::messages::get_row(&conn, received)
                .unwrap()
                .unwrap()
                .uid,
            None
        );
        // Only this device holds the reply.
        assert!(!relink_pending(&conn, sent));
        let states = actions.map(|id| {
            crate::db::repo::actions::get(&conn, id)
                .unwrap()
                .unwrap()
                .state
        });
        assert_eq!(states, ["cancelled", "pending"]);
        assert_eq!(header_failures(&conn, projects), 0);

        upsert_discovered(
            &conn,
            1,
            "Projects",
            Some("/"),
            None,
            true,
            FolderPermissions::FULL,
        )
        .unwrap();
        assert!(!missing(projects));
        assert_eq!(list(&conn, Some(1)).unwrap().len(), 2);
    }

    #[test]
    fn a_role_is_looked_up_among_the_folders_the_account_shows() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        upsert_jmap(&conn, 1, "sent-mailbox", "Sent", Some("sent")).unwrap();
        let old = upsert(&conn, 1, "Sent Messages", Some("/"), Some("sent")).unwrap();
        conn.execute(
            "UPDATE folders SET missing_on_server = 1 WHERE id = ?1",
            params![old],
        )
        .unwrap();
        let current = upsert(&conn, 1, "Sent Items", Some("/"), Some("sent")).unwrap();
        assert_eq!(by_role(&conn, 1, "sent").unwrap().unwrap().id, current);

        conn.execute(
            "UPDATE accounts SET mail_protocol = 'jmap' WHERE id = 1",
            [],
        )
        .unwrap();
        assert_eq!(
            by_role(&conn, 1, "sent")
                .unwrap()
                .unwrap()
                .jmap_id
                .as_deref(),
            Some("sent-mailbox")
        );
    }

    #[test]
    fn a_new_uidvalidity_unlinks_the_mail_that_came_from_the_server() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        set_uid_state(&conn, 1, Some(5), Some(10), None).unwrap();
        set_last_seen_uid(&conn, 1, 9).unwrap();
        set_backfill(&conn, 1, Some(1), true).unwrap();
        let (_, received) = testutil::seed_message(&conn, "sender@example.com", "Hi", false);
        let (_, filed) = testutil::seed_message(&conn, "me@test.dev", "Re: Hi", false);
        let (_, local) = testutil::seed_message(&conn, "me@test.dev", "Re: Hi again", false);
        conn.execute(
            "UPDATE messages SET is_outgoing = 1 WHERE id IN (?1, ?2)",
            params![filed, local],
        )
        .unwrap();
        conn.execute(
            "UPDATE messages SET uid = NULL WHERE id = ?1",
            params![local],
        )
        .unwrap();
        crate::db::repo::sync_failures::record_header(&conn, 1, 12, None, "bad header").unwrap();
        crate::db::repo::sync_failures::record_content(&conn, received, None, "no body").unwrap();

        reset_uid_mappings(&conn, 1).unwrap();
        assert!(relink_pending(&conn, received));
        assert!(relink_pending(&conn, filed));
        assert!(!relink_pending(&conn, local));
        let uids = conn
            .prepare("SELECT uid FROM messages ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get::<_, Option<i64>>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(uids, [None, None, None]);
        assert_eq!(header_failures(&conn, 1), 0);
        let content_failures: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_failures WHERE message_id = ?1",
                params![received],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(content_failures, 1);
        let folder = get(&conn, 1).unwrap().unwrap();
        assert_eq!((folder.last_seen_uid, folder.backfill_done), (0, false));
    }
}
