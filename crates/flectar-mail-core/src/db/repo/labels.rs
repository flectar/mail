use crate::error::{CoreError, Result};
use crate::models::Label;
use rusqlite::{Connection, OptionalExtension, Row, params};

const DEFAULT_AUTO_LABELS: [(&str, &str, &str, i64); 4] = [
    ("Marketing", "#e0708a", "FlectarMailAutoMarketing", 1000),
    ("News", "#5b9dd9", "FlectarMailAutoNews", 1001),
    ("Social", "#7bc47f", "FlectarMailAutoSocial", 1002),
    ("Pitch", "#c9a04e", "FlectarMailAutoPitch", 1003),
];

fn from_row(row: &Row) -> rusqlite::Result<Label> {
    Ok(Label {
        id: row.get("id")?,
        name: row.get("name")?,
        color: row.get("color")?,
        keyword: row.get("keyword")?,
        position: row.get("position")?,
        owner_account_id: row.get("owner_account_id")?,
        is_auto: row.get::<_, i64>("is_auto")? != 0,
    })
}

/// Derive a valid IMAP keyword atom from a label name. IMAP flag-keywords are
/// atoms, so strip the characters an atom may not contain (RFC 3501) and fall
/// back to a stable placeholder when nothing usable remains.
pub fn keyword_for(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| match c {
            '(' | ')' | '{' | ' ' | '%' | '*' | '"' | '\\' | ']' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    out = out.trim_matches('_').to_string();
    if out.is_empty() {
        "Label".to_string()
    } else {
        out
    }
}

pub fn list(conn: &Connection) -> Result<Vec<Label>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, color, keyword, position, owner_account_id, is_auto
         FROM labels
         ORDER BY CASE scope WHEN 'automatic' THEN 0 WHEN 'global' THEN 1 ELSE 2 END,
                  owner_account_id, position, name",
    )?;
    let rows = stmt
        .query_map([], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Label>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, color, keyword, position, owner_account_id, is_auto
         FROM labels WHERE id = ?1",
    )?;
    Ok(stmt.query_row(params![id], from_row).optional()?)
}

pub fn save(
    conn: &Connection,
    id: Option<i64>,
    name: &str,
    color: &str,
    position: i64,
    owner_account_id: Option<i64>,
) -> Result<Label> {
    let owner_account_id = match id {
        Some(id) => {
            get(conn, id)?
                .ok_or_else(|| CoreError::NotFound(format!("label {id}")))?
                .owner_account_id
        }
        None => owner_account_id,
    };
    let duplicate: bool = match owner_account_id {
        Some(account_id) => conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM labels
                WHERE name = ?1 COLLATE NOCASE
                  AND scope = 'account' AND owner_account_id = ?2
                  AND (?3 IS NULL OR id != ?3)
             )",
            params![name, account_id, id],
            |row| row.get(0),
        )?,
        None => conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM labels
                WHERE name = ?1 COLLATE NOCASE AND scope = 'global'
                  AND (?2 IS NULL OR id != ?2)
             )",
            params![name, id],
            |row| row.get(0),
        )?,
    };
    if duplicate {
        return Err(CoreError::Other(format!(
            "a label named '{name}' already exists in this scope"
        )));
    }
    let id = match id {
        // Rename/recolor: keep the existing keyword so the server mapping is
        // stable even when the display name changes.
        Some(id) => {
            conn.execute(
                "UPDATE labels SET name=?2, color=?3, position=?4 WHERE id=?1",
                params![id, name, color, position],
            )?;
            id
        }
        None => {
            conn.execute(
                "INSERT INTO labels (
                   name, color, keyword, position, scope, owner_account_id, origin
                 ) VALUES (?1,?2,?3,?4,?5,?6,'user')",
                params![
                    name,
                    color,
                    keyword_for(name),
                    position,
                    if owner_account_id.is_some() {
                        "account"
                    } else {
                        "global"
                    },
                    owner_account_id,
                ],
            )?;
            conn.last_insert_rowid()
        }
    };
    let mut stmt = conn.prepare(
        "SELECT id, name, color, keyword, position, owner_account_id, is_auto
         FROM labels WHERE id = ?1",
    )?;
    Ok(stmt.query_row(params![id], from_row)?)
}

pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    let key = format!("label:{id}");
    // Any thread routed into this label (an auto category) must fall back to
    // Important/Other, so clear its routed tab before the row goes. Chip
    // memberships in message_labels cascade via the foreign key.
    conn.execute(
        "UPDATE threads SET routed_tab = NULL WHERE routed_tab = ?1",
        params![key],
    )?;
    // A rule that routed matches into this category would dangle (and fail the
    // message_labels foreign key on the next route); revert those rules to their
    // own tab.
    conn.execute(
        "UPDATE split_rules SET target = NULL WHERE target = ?1",
        params![key],
    )?;
    conn.execute("DELETE FROM labels WHERE id = ?1", params![id])?;
    Ok(())
}

/// Recreate any missing built-in auto categories. Existing categories and
/// their user-defined order are left untouched. A conflicting user label with
/// the same display name is also left untouched.
pub fn restore_auto_defaults(conn: &Connection) -> Result<i64> {
    let mut restored = 0;
    for (name, color, keyword, position) in DEFAULT_AUTO_LABELS {
        let exists: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM labels WHERE keyword = ?1 AND is_auto = 1)",
            params![keyword],
            |row| row.get::<_, i64>(0).map(|value| value != 0),
        )?;
        if exists {
            continue;
        }
        restored += conn.execute(
            "INSERT OR IGNORE INTO labels (
               name, color, keyword, position, is_auto, scope, origin
             ) VALUES (?1, ?2, ?3, ?4, 1, 'automatic', 'system')",
            params![name, color, keyword, position],
        )? as i64;
    }
    Ok(restored)
}

/// Label ids applied to any message in a thread.
pub fn for_thread(conn: &Connection, thread_id: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT ml.label_id FROM message_labels ml
         JOIN messages m ON m.id = ml.message_id
         WHERE m.thread_id = ?1 ORDER BY ml.label_id",
    )?;
    let rows = stmt
        .query_map(params![thread_id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn add_to_message(conn: &Connection, message_id: i64, label_id: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO message_labels (message_id, label_id) VALUES (?1, ?2)",
        params![message_id, label_id],
    )?;
    Ok(())
}

pub fn remove_from_message(conn: &Connection, message_id: i64, label_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM message_labels WHERE message_id = ?1 AND label_id = ?2",
        params![message_id, label_id],
    )?;
    Ok(())
}

/// Reconcile a message's label memberships against the IMAP keywords the server
/// reports. Only labels we know about are touched - unknown server keywords are
/// left alone so foreign keywords never masquerade as labels. Returns true if
/// anything changed.
pub fn reconcile_keywords(conn: &Connection, message_id: i64, keywords: &[String]) -> Result<bool> {
    let account_id: i64 = conn.query_row(
        "SELECT account_id FROM messages WHERE id = ?1",
        params![message_id],
        |row| row.get(0),
    )?;
    let labels = list(conn)?;
    if labels.is_empty() {
        return Ok(false);
    }
    let mut changed = false;
    // Auto labels are local-only (classified at sync, never pushed to IMAP):
    // reconciling them against server keywords would strip their memberships
    // on every flag pass, so they are excluded here.
    for label in labels
        .into_iter()
        .filter(|label| label.owner_account_id == Some(account_id))
    {
        let present = keywords.iter().any(|k| k == &label.keyword);
        let has: bool = conn.query_row(
            "SELECT COUNT(*) FROM message_labels WHERE message_id = ?1 AND label_id = ?2",
            params![message_id, label.id],
            |r| r.get::<_, i64>(0),
        )? > 0;
        if present && !has {
            add_to_message(conn, message_id, label.id)?;
            changed = true;
        } else if !present && has {
            remove_from_message(conn, message_id, label.id)?;
            changed = true;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    #[test]
    fn keyword_for_strips_forbidden_atom_chars() {
        assert_eq!(keyword_for("Follow up"), "Follow_up");
        // "{" is an atom-special and is stripped; "}" is a legal atom char.
        assert_eq!(keyword_for("(a) {b} \"c\""), "a___b}__c");
        assert_eq!(keyword_for("   "), "Label");
        assert_eq!(keyword_for("Déjà"), "Déjà"); // non-ASCII survives
    }

    #[test]
    fn migration_seeds_four_auto_labels() {
        let c = testutil::conn();
        let auto: Vec<Label> = list(&c)
            .unwrap()
            .into_iter()
            .filter(|l| l.is_auto)
            .collect();
        let keywords: Vec<&str> = auto.iter().map(|l| l.keyword.as_str()).collect();
        assert_eq!(auto.len(), 4);
        for k in [
            "FlectarMailAutoMarketing",
            "FlectarMailAutoNews",
            "FlectarMailAutoSocial",
            "FlectarMailAutoPitch",
        ] {
            assert!(keywords.contains(&k), "missing seed {k}");
        }
    }

    #[test]
    fn save_keeps_keyword_on_rename() {
        let c = testutil::conn();
        let l = save(&c, None, "Follow up", "#fff", 0, None).unwrap();
        assert_eq!(l.keyword, "Follow_up");
        let renamed = save(&c, Some(l.id), "Chase later", "#000", 1, None).unwrap();
        assert_eq!(renamed.keyword, "Follow_up");
        assert_eq!(renamed.name, "Chase later");
    }

    #[test]
    fn delete_removes_auto_rows_and_clears_routed_tab() {
        use crate::db::repo::threads;
        let c = testutil::conn();
        // Match production (db::open sets this): chip memberships cascade on delete.
        c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        testutil::seed_account(&c);
        let auto_id: i64 = c
            .query_row("SELECT id FROM labels WHERE is_auto = 1 LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        // A thread routed into that auto category.
        let (thread, _msg) = testutil::seed_message(&c, "x@shop.example", "sale", true);
        crate::route::apply_tab(&c, thread, Some(&format!("label:{auto_id}"))).unwrap();

        delete(&c, auto_id).unwrap();
        assert!(
            get(&c, auto_id).unwrap().is_none(),
            "auto label must delete"
        );
        // The orphaned thread falls back to Important/Other (routed_tab cleared).
        let routed: Option<String> = c
            .query_row(
                "SELECT routed_tab FROM threads WHERE id = ?1",
                params![thread],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(routed, None);
        assert!(
            threads::get_summary(&c, thread)
                .unwrap()
                .unwrap()
                .labels
                .is_empty()
        );

        let manual = save(&c, None, "Temp", "#fff", 0, None).unwrap();
        delete(&c, manual.id).unwrap();
        assert!(get(&c, manual.id).unwrap().is_none());
    }

    #[test]
    fn restore_auto_defaults_recreates_only_missing_categories() {
        let c = testutil::conn();
        let social_id = c
            .query_row(
                "SELECT id FROM labels WHERE keyword = 'FlectarMailAutoSocial'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        delete(&c, social_id).unwrap();

        assert_eq!(restore_auto_defaults(&c).unwrap(), 1);
        assert_eq!(restore_auto_defaults(&c).unwrap(), 0);
        let auto = list(&c)
            .unwrap()
            .into_iter()
            .filter(|label| label.is_auto)
            .collect::<Vec<_>>();
        assert_eq!(auto.len(), 4);
        assert!(
            auto.iter()
                .any(|label| label.keyword == "FlectarMailAutoSocial")
        );
    }

    #[test]
    fn delete_reverts_rules_that_targeted_the_category() {
        use crate::db::repo::splits;
        let c = testutil::conn();
        let auto_id = c
            .query_row(
                "SELECT id FROM labels WHERE keyword = 'FlectarMailAutoMarketing'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        // A rule that routes matches into that category.
        let rule = splits::save(
            &c,
            None,
            "promos",
            0,
            &crate::models::SplitRuleQuery {
                senders: Some(vec!["shop.example".into()]),
                ..Default::default()
            },
            Some(&format!("label:{auto_id}")),
        )
        .unwrap();

        delete(&c, auto_id).unwrap();

        // The rule survives but reverts to its own tab (no dangling target).
        let got = splits::get(&c, rule.id).unwrap().unwrap();
        assert_eq!(got.target, None);
    }

    /// Regression: server reconcile must never strip local-only auto labels.
    #[test]
    fn reconcile_skips_auto_labels() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_t, msg) = testutil::seed_message(&c, "a@b.c", "hello", false);

        let manual = save(&c, None, "Work", "#fff", 0, Some(1)).unwrap();
        let auto_id: i64 = c
            .query_row(
                "SELECT id FROM labels WHERE keyword = 'FlectarMailAutoNews'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        add_to_message(&c, msg, manual.id).unwrap();
        add_to_message(&c, msg, auto_id).unwrap();

        // Server reports no keywords at all: manual membership is removed,
        // auto membership survives.
        let changed = reconcile_keywords(&c, msg, &[]).unwrap();
        assert!(changed);
        let remaining = for_thread(&c, 1).unwrap();
        assert!(
            remaining.contains(&auto_id),
            "auto label was stripped by reconcile"
        );
        assert!(!remaining.contains(&manual.id));

        // Server later reports the manual keyword: membership comes back.
        reconcile_keywords(&c, msg, &["Work".to_string()]).unwrap();
        assert!(for_thread(&c, 1).unwrap().contains(&manual.id));
    }
}
