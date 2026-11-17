//! Rules that sort new mail as it arrives (#238), kept in the order the user
//! gave them. See `crate::rules` for how they run.
use crate::error::Result;
use crate::models::MailRule;
use rusqlite::{Connection, OptionalExtension, Row, params};

const COLUMNS: &str =
    "id, account_id, name, enabled, match_all, conditions_json, actions_json, stop";

/// None for a rule a newer version of the app saved with conditions or
/// actions this one doesn't know. It is left alone.
fn from_row(row: &Row) -> rusqlite::Result<Option<MailRule>> {
    let id: i64 = row.get("id")?;
    let conditions = serde_json::from_str(&row.get::<_, String>("conditions_json")?);
    let actions = serde_json::from_str(&row.get::<_, String>("actions_json")?);
    let (Ok(conditions), Ok(actions)) = (conditions, actions) else {
        tracing::warn!(rule = id, "skipping a rule this version can't read");
        return Ok(None);
    };
    Ok(Some(MailRule {
        id,
        account_id: row.get("account_id")?,
        name: row.get("name")?,
        enabled: row.get("enabled")?,
        match_all: row.get("match_all")?,
        conditions,
        actions,
        stop: row.get("stop")?,
    }))
}

fn query(conn: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<Vec<MailRule>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(args, from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().flatten().collect())
}

/// Every rule, in order.
pub fn list(conn: &Connection) -> Result<Vec<MailRule>> {
    query(
        conn,
        &format!("SELECT {COLUMNS} FROM mail_rules ORDER BY position, id"),
        [],
    )
}

/// The rules that sort `account_id`'s mail, in order: its own and those
/// for every account, when they are on.
pub fn enabled_for(conn: &Connection, account_id: i64) -> Result<Vec<MailRule>> {
    query(
        conn,
        &format!(
            "SELECT {COLUMNS} FROM mail_rules
             WHERE enabled = 1 AND (account_id IS NULL OR account_id = ?1)
             ORDER BY position, id"
        ),
        params![account_id],
    )
}

/// The folders `account_id`'s rules move or copy mail into, on or off.
pub fn target_folders(conn: &Connection, account_id: i64) -> Result<Vec<i64>> {
    use crate::models::RuleAction;
    Ok(list(conn)?
        .into_iter()
        .filter(|rule| rule.account_id == Some(account_id))
        .flat_map(|rule| rule.actions)
        .filter_map(|action| match action {
            RuleAction::Move { folder } | RuleAction::Copy { folder } => Some(folder),
            _ => None,
        })
        .collect())
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<MailRule>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM mail_rules WHERE id = ?1"),
            params![id],
            from_row,
        )
        .optional()?
        .flatten())
}

/// Save `rule`: a new one (id 0) goes last, a saved one keeps its place.
/// Returns it with its id.
pub fn save(conn: &Connection, rule: &MailRule, now: i64) -> Result<MailRule> {
    let conditions = serde_json::to_string(&rule.conditions)?;
    let actions = serde_json::to_string(&rule.actions)?;
    let id = if rule.id == 0 {
        conn.execute(
            "INSERT INTO mail_rules (account_id, position, name, enabled, match_all,
                 conditions_json, actions_json, stop, created_at, updated_at)
             VALUES (?1, (SELECT COALESCE(MAX(position), -1) + 1 FROM mail_rules),
                 ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                rule.account_id,
                rule.name,
                rule.enabled,
                rule.match_all,
                conditions,
                actions,
                rule.stop,
                now
            ],
        )?;
        conn.last_insert_rowid()
    } else {
        let changed = conn.execute(
            "UPDATE mail_rules SET account_id = ?2, name = ?3, enabled = ?4, match_all = ?5,
                 conditions_json = ?6, actions_json = ?7, stop = ?8, updated_at = ?9
             WHERE id = ?1",
            params![
                rule.id,
                rule.account_id,
                rule.name,
                rule.enabled,
                rule.match_all,
                conditions,
                actions,
                rule.stop,
                now
            ],
        )?;
        if changed == 0 {
            return Err(crate::error::CoreError::NotFound(format!(
                "rule {}",
                rule.id
            )));
        }
        rule.id
    };
    Ok(MailRule { id, ..rule.clone() })
}

pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM mail_rules WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn set_enabled(conn: &Connection, id: i64, enabled: bool, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE mail_rules SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, enabled, now],
    )?;
    Ok(())
}

/// Put the rules in the order of `ids`. Rules it leaves out keep their
/// order, after those it names.
pub fn reorder(conn: &Connection, ids: &[i64]) -> Result<()> {
    let mut rest = list(conn)?
        .into_iter()
        .map(|rule| rule.id)
        .filter(|id| !ids.contains(id))
        .collect::<Vec<_>>();
    let mut order = ids.to_vec();
    order.append(&mut rest);
    let mut stmt = conn.prepare("UPDATE mail_rules SET position = ?2 WHERE id = ?1")?;
    for (position, id) in order.iter().enumerate() {
        stmt.execute(params![id, position as i64])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;
    use crate::models::{RuleAction, RuleCondition, RuleField, RuleOp};

    fn rule(name: &str, account_id: Option<i64>) -> MailRule {
        MailRule {
            id: 0,
            account_id,
            name: name.into(),
            enabled: true,
            match_all: true,
            conditions: vec![RuleCondition {
                field: RuleField::From,
                header: String::new(),
                op: RuleOp::Contains,
                value: "news".into(),
                negate: false,
            }],
            actions: vec![RuleAction::MarkRead],
            stop: false,
        }
    }

    #[test]
    fn rules_keep_their_order_and_their_account() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username, incoming_host,
                 incoming_port, smtp_host, smtp_port, created_at, mail_protocol, settings_json)
             SELECT 2, 'b@test.dev', provider, auth_kind, 'b', incoming_host, incoming_port,
                 smtp_host, smtp_port, created_at, mail_protocol, settings_json
             FROM accounts WHERE id = 1",
            [],
        )
        .unwrap();
        let first = save(&conn, &rule("First", None), 1).unwrap();
        let second = save(&conn, &rule("Second", Some(2)), 1).unwrap();
        let third = save(&conn, &rule("Third", Some(1)), 1).unwrap();
        let names = |rules: Vec<MailRule>| rules.into_iter().map(|r| r.name).collect::<Vec<_>>();
        assert_eq!(names(list(&conn).unwrap()), ["First", "Second", "Third"]);
        assert_eq!(names(enabled_for(&conn, 1).unwrap()), ["First", "Third"]);

        reorder(&conn, &[third.id, first.id]).unwrap();
        assert_eq!(names(list(&conn).unwrap()), ["Third", "First", "Second"]);

        set_enabled(&conn, third.id, false, 2).unwrap();
        assert_eq!(names(enabled_for(&conn, 1).unwrap()), ["First"]);

        // Saving a rule again keeps its place.
        let renamed = save(
            &conn,
            &MailRule {
                name: "Second, renamed".into(),
                ..second.clone()
            },
            3,
        )
        .unwrap();
        assert_eq!(renamed.id, second.id);
        assert_eq!(
            names(list(&conn).unwrap()),
            ["Third", "First", "Second, renamed"]
        );

        // Deleting an account deletes its rules.
        conn.execute("DELETE FROM accounts WHERE id = 2", [])
            .unwrap();
        assert_eq!(names(list(&conn).unwrap()), ["Third", "First"]);
        delete(&conn, first.id).unwrap();
        assert_eq!(get(&conn, first.id).unwrap(), None);
    }

    #[test]
    fn a_rule_this_version_cant_read_is_skipped() {
        let conn = testutil::conn();
        let saved = save(&conn, &rule("Known", None), 1).unwrap();
        conn.execute(
            "INSERT INTO mail_rules (account_id, position, name, enabled, match_all,
                 conditions_json, actions_json, stop, created_at, updated_at)
             VALUES (NULL, 5, 'Newer', 1, 1, '[{\"field\":\"future\",\"op\":\"is\"}]', '[]', 0, 1, 1)",
            [],
        )
        .unwrap();
        assert_eq!(list(&conn).unwrap(), vec![saved]);
    }
}
