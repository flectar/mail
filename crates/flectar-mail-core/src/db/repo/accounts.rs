use crate::error::Result;
use crate::models::*;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::parse_json_column;

pub const CREDENTIAL_SETUP_STATE: &str = "credential_setup";

fn account_from_row(row: &Row) -> rusqlite::Result<Account> {
    Ok(Account {
        id: row.get("id")?,
        email: row.get("email")?,
        display_name: row.get("display_name")?,
        avatar_url: row.get("avatar_url")?,
        provider: Provider::from_storage(&row.get::<_, String>("provider")?),
        auth_kind: AuthKind::from_storage(&row.get::<_, String>("auth_kind")?),
        mail_protocol: MailProtocol::from_storage(&row.get::<_, String>("mail_protocol")?),
        sync_state: row.get("sync_state")?,
        sync_error: row.get("sync_error")?,
    })
}

fn config_from_row(row: &Row) -> rusqlite::Result<AccountConfig> {
    let settings_json = row.get::<_, String>("settings_json")?;
    let settings = parse_json_column(&settings_json, 14)?;
    Ok(AccountConfig {
        id: row.get("id")?,
        email: row.get("email")?,
        display_name: row.get("display_name")?,
        avatar_url: row.get("avatar_url")?,
        provider: Provider::from_storage(&row.get::<_, String>("provider")?),
        auth_kind: AuthKind::from_storage(&row.get::<_, String>("auth_kind")?),
        mail_protocol: MailProtocol::from_storage(&row.get::<_, String>("mail_protocol")?),
        username: row.get("username")?,
        jmap_url: row.get("jmap_url")?,
        jmap_account_id: row.get("jmap_account_id")?,
        imap_host: row.get("imap_host")?,
        imap_port: row.get::<_, i64>("imap_port")? as u16,
        smtp_host: row.get("smtp_host")?,
        smtp_port: row.get::<_, i64>("smtp_port")? as u16,
        settings,
    })
}

pub fn list(conn: &Connection) -> Result<Vec<Account>> {
    let mut stmt = conn.prepare(
        "SELECT id, email, display_name, avatar_url, provider, auth_kind,
                mail_protocol, sync_state, sync_error
         FROM accounts
         WHERE sync_state <> ?1
         ORDER BY sort_order, id",
    )?;
    let rows = stmt
        .query_map(params![CREDENTIAL_SETUP_STATE], account_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn list_configs(conn: &Connection) -> Result<Vec<AccountConfig>> {
    let mut stmt = conn.prepare(
        "SELECT id, email, display_name, avatar_url, provider, auth_kind,
                mail_protocol, username, jmap_url, jmap_account_id, imap_host,
                imap_port, smtp_host, smtp_port, settings_json
         FROM accounts
         WHERE sync_state <> ?1
         ORDER BY sort_order, id",
    )?;
    let rows = stmt
        .query_map(params![CREDENTIAL_SETUP_STATE], config_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_config(conn: &Connection, id: i64) -> Result<Option<AccountConfig>> {
    let mut stmt = conn.prepare(
        "SELECT id, email, display_name, avatar_url, provider, auth_kind,
                mail_protocol, username, jmap_url, jmap_account_id, imap_host,
                imap_port, smtp_host, smtp_port, settings_json
         FROM accounts WHERE id = ?1",
    )?;
    Ok(stmt.query_row(params![id], config_from_row).optional()?)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Account>> {
    let mut stmt = conn.prepare(
        "SELECT id, email, display_name, avatar_url, provider, auth_kind,
                mail_protocol, sync_state, sync_error
         FROM accounts WHERE id = ?1",
    )?;
    Ok(stmt.query_row(params![id], account_from_row).optional()?)
}

pub fn find_by_email(conn: &Connection, email: &str) -> Result<Option<Account>> {
    let mut stmt = conn.prepare(
        "SELECT id, email, display_name, avatar_url, provider, auth_kind,
                mail_protocol, sync_state, sync_error
         FROM accounts
         WHERE email = ?1 COLLATE NOCASE
         LIMIT 1",
    )?;
    Ok(stmt
        .query_row(params![email], account_from_row)
        .optional()?)
}

pub fn credential_setup_ids(conn: &Connection) -> Result<Vec<i64>> {
    let mut statement =
        conn.prepare("SELECT id FROM accounts WHERE sync_state = ?1 ORDER BY id")?;
    Ok(statement
        .query_map(params![CREDENTIAL_SETUP_STATE], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub struct NewAccount<'a> {
    pub email: &'a str,
    pub display_name: Option<&'a str>,
    pub avatar_url: Option<&'a str>,
    pub provider: Provider,
    pub auth_kind: AuthKind,
    pub mail_protocol: MailProtocol,
    pub username: &'a str,
    pub jmap_url: &'a str,
    pub jmap_account_id: Option<&'a str>,
    pub imap_host: &'a str,
    pub imap_port: u16,
    pub smtp_host: &'a str,
    pub smtp_port: u16,
}

pub fn insert(conn: &Connection, a: &NewAccount) -> Result<i64> {
    insert_with_sync_state(conn, a, "idle")
}

pub fn insert_with_sync_state(conn: &Connection, a: &NewAccount, sync_state: &str) -> Result<i64> {
    let settings_json = serde_json::to_string(&AccountSettings::default())?;
    conn.execute(
        "INSERT INTO accounts (email, display_name, avatar_url, provider, auth_kind, mail_protocol, username,
                               jmap_url, jmap_account_id,
                               imap_host, imap_port, smtp_host, smtp_port, created_at,
                               sort_order, settings_json, sync_state)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
                 COALESCE((SELECT MAX(sort_order) + 1 FROM accounts), 0), ?15, ?16)",
        params![
            a.email,
            a.display_name,
            a.avatar_url,
            a.provider.as_str(),
            a.auth_kind.as_str(),
            a.mail_protocol.as_str(),
            a.username,
            a.jmap_url,
            a.jmap_account_id,
            a.imap_host,
            a.imap_port,
            a.smtp_host,
            a.smtp_port,
            now_ms(),
            settings_json,
            sync_state,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn set_settings(conn: &Connection, id: i64, settings: &AccountSettings) -> Result<()> {
    let settings_json = serde_json::to_string(settings)?;
    let changed = conn.execute(
        "UPDATE accounts SET settings_json = ?2 WHERE id = ?1",
        params![id, settings_json],
    )?;
    if changed == 0 {
        return Err(crate::error::CoreError::NotFound(format!("account {id}")));
    }
    Ok(())
}

pub fn update_password(conn: &Connection, id: i64, a: &NewAccount) -> Result<()> {
    let (old_protocol, old_jmap_url, old_jmap_account_id): (String, String, Option<String>) = conn
        .query_row(
            "SELECT mail_protocol,jmap_url,jmap_account_id FROM accounts WHERE id=?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let jmap_identity_changed = (old_protocol == "jmap" || a.mail_protocol == MailProtocol::Jmap)
        && (old_protocol != a.mail_protocol.as_str()
            || old_jmap_url != a.jmap_url
            || old_jmap_account_id.as_deref() != a.jmap_account_id);
    // The caller has stopped the account workers before this transaction. A
    // submission that was interrupted at that instant is inherently
    // uncertain and must never be replayed automatically. Other same-account
    // operations are safe to release back to the durable queue.
    conn.execute(
        "UPDATE pending_actions
         SET state='failed',finished_at=?2,
             last_error='account connection changed during submission; check Sent before retrying'
         WHERE account_id=?1 AND kind='send' AND state='inflight'",
        params![id, now_ms()],
    )?;
    conn.execute(
        "UPDATE pending_actions SET state='pending',not_before=?2
         WHERE account_id=?1 AND kind<>'send' AND state='inflight'",
        params![id, now_ms()],
    )?;
    conn.execute(
        "UPDATE accounts SET email = ?2, display_name = ?3, avatar_url = ?4, provider = ?5,
                auth_kind = ?6, mail_protocol = ?7, username = ?8,
                jmap_url = ?9, jmap_account_id = ?10, imap_host = ?11, imap_port = ?12,
                smtp_host = ?13, smtp_port = ?14, sync_state = 'idle', sync_error = NULL
         WHERE id = ?1",
        params![
            id,
            a.email,
            a.display_name,
            a.avatar_url,
            a.provider.as_str(),
            a.auth_kind.as_str(),
            a.mail_protocol.as_str(),
            a.username,
            a.jmap_url,
            a.jmap_account_id,
            a.imap_host,
            a.imap_port,
            a.smtp_host,
            a.smtp_port,
        ],
    )?;
    if jmap_identity_changed {
        conn.execute(
            "UPDATE pending_actions
             SET state='cancelled',finished_at=?2,
                 last_error='remote mail account changed before this action completed'
             WHERE account_id=?1 AND state IN ('pending','inflight')",
            params![id, now_ms()],
        )?;
        conn.execute(
            "DELETE FROM jmap_sync_state WHERE account_id=?1",
            params![id],
        )?;
        conn.execute(
            "UPDATE messages SET jmap_id=NULL,jmap_blob_id=NULL WHERE account_id=?1",
            params![id],
        )?;
        conn.execute(
            "UPDATE threads SET jmap_id=NULL WHERE account_id=?1",
            params![id],
        )?;
        if old_protocol == "jmap" && a.mail_protocol == MailProtocol::Jmap {
            conn.execute(
                "UPDATE folders SET jmap_id=NULL WHERE account_id=?1",
                params![id],
            )?;
        }
    }
    Ok(())
}

pub fn update_oauth_identity(
    conn: &Connection,
    id: i64,
    display_name: Option<&str>,
    avatar_url: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE accounts
         SET display_name = COALESCE(?2, display_name),
             avatar_url = COALESCE(?3, avatar_url)
         WHERE id = ?1",
        params![id, display_name, avatar_url],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM accounts WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn set_sync_state(conn: &Connection, id: i64, state: &str) -> Result<()> {
    set_sync_state_with_error(conn, id, state, None)
}

pub fn set_sync_state_with_error(
    conn: &Connection,
    id: i64,
    state: &str,
    error: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET sync_state = ?2, sync_error = ?3 WHERE id = ?1",
        params![id, state, error],
    )?;
    Ok(())
}

/// Move one account before or after another and compact the persisted order.
/// A complete rewrite keeps ordering deterministic after any number of moves
/// and avoids fragile fractional positions.
pub fn reorder(conn: &mut Connection, source_id: i64, target_id: i64, after: bool) -> Result<()> {
    if source_id == target_id {
        return Ok(());
    }
    let mut ids = {
        let mut stmt = conn.prepare("SELECT id FROM accounts ORDER BY sort_order, id")?;
        stmt.query_map([], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let source_index = ids
        .iter()
        .position(|id| *id == source_id)
        .ok_or_else(|| crate::error::CoreError::NotFound("source account".into()))?;
    ids.remove(source_index);
    let target_index = ids
        .iter()
        .position(|id| *id == target_id)
        .ok_or_else(|| crate::error::CoreError::NotFound("target account".into()))?;
    ids.insert(target_index + usize::from(after), source_id);

    let tx = conn.transaction()?;
    for (position, account_id) in ids.into_iter().enumerate() {
        tx.execute(
            "UPDATE accounts SET sort_order = ?2 WHERE id = ?1",
            params![account_id, position as i64],
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_by_email_is_case_insensitive_and_update_keeps_account_id() {
        let conn = crate::db::testutil::conn();
        let first = NewAccount {
            email: "me@test.dev",
            display_name: None,
            avatar_url: None,
            provider: Provider::Imap,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            username: "me",
            jmap_url: "",
            jmap_account_id: None,
            imap_host: "imap.test.dev",
            imap_port: 993,
            smtp_host: "smtp.test.dev",
            smtp_port: 465,
        };
        let id = insert(&conn, &first).unwrap();
        assert_eq!(
            get_config(&conn, id)
                .unwrap()
                .unwrap()
                .settings
                .mail_history,
            MailHistory::SixMonths
        );

        assert_eq!(find_by_email(&conn, "ME@TEST.DEV").unwrap().unwrap().id, id);

        let updated = NewAccount {
            email: "me@test.dev",
            display_name: Some("Me"),
            avatar_url: None,
            provider: Provider::Imap,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            username: "updated-user",
            jmap_url: "",
            jmap_account_id: None,
            imap_host: "imap2.test.dev",
            imap_port: 993,
            smtp_host: "smtp2.test.dev",
            smtp_port: 587,
        };
        update_password(&conn, id, &updated).unwrap();

        let config = get_config(&conn, id).unwrap().unwrap();
        assert_eq!(config.id, id);
        assert_eq!(config.username, "updated-user");
        assert_eq!(config.smtp_host, "smtp2.test.dev");
        assert_eq!(config.smtp_port, 587);

        update_oauth_identity(
            &conn,
            id,
            Some("Profile Name"),
            Some("https://lh3.googleusercontent.com/a/profile"),
        )
        .unwrap();
        let account = get(&conn, id).unwrap().unwrap();
        assert_eq!(account.display_name.as_deref(), Some("Profile Name"));
        assert_eq!(
            account.avatar_url.as_deref(),
            Some("https://lh3.googleusercontent.com/a/profile")
        );
    }

    #[test]
    fn credential_setup_accounts_are_recoverable_but_not_visible_or_started() {
        let conn = crate::db::testutil::conn();
        let account = NewAccount {
            email: "pending@test.dev",
            display_name: None,
            avatar_url: None,
            provider: Provider::Gmail,
            auth_kind: AuthKind::Oauth2,
            mail_protocol: MailProtocol::Imap,
            username: "pending@test.dev",
            jmap_url: "",
            jmap_account_id: None,
            imap_host: "imap.gmail.com",
            imap_port: 993,
            smtp_host: "smtp.gmail.com",
            smtp_port: 465,
        };
        let id = insert_with_sync_state(&conn, &account, CREDENTIAL_SETUP_STATE).unwrap();

        assert!(list(&conn).unwrap().is_empty());
        assert!(list_configs(&conn).unwrap().is_empty());
        assert_eq!(
            find_by_email(&conn, "PENDING@test.dev")
                .unwrap()
                .unwrap()
                .id,
            id
        );

        set_sync_state(&conn, id, "idle").unwrap();
        assert_eq!(list(&conn).unwrap().len(), 1);
        assert_eq!(list_configs(&conn).unwrap().len(), 1);
    }

    #[test]
    fn empty_account_settings_use_the_bounded_default() {
        let conn = crate::db::testutil::conn();
        conn.execute(
            "INSERT INTO accounts (
               email, provider, auth_kind, username, imap_host, imap_port,
               smtp_host, smtp_port, created_at, settings_json
             ) VALUES ('legacy@test.dev', 'imap', 'password', 'legacy', 'imap', 993,
                       'smtp', 465, 0, '{}')",
            [],
        )
        .unwrap();
        let config = get_config(&conn, 1).unwrap().unwrap();
        assert_eq!(config.settings.mail_history, MailHistory::SixMonths);
    }

    #[test]
    fn changing_jmap_identity_invalidates_all_remote_checkpoints() {
        let conn = crate::db::testutil::conn();
        let initial = NewAccount {
            email: "me@test.dev",
            display_name: None,
            avatar_url: None,
            provider: Provider::Imap,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Jmap,
            username: "me@test.dev",
            jmap_url: "https://mail.test.dev",
            jmap_account_id: Some("account-a"),
            imap_host: "",
            imap_port: 993,
            smtp_host: "",
            smtp_port: 465,
        };
        let id = insert(&conn, &initial).unwrap();
        conn.execute(
            "INSERT INTO folders(account_id,imap_name,jmap_id) VALUES(?1,'Inbox','mailbox-a')",
            params![id],
        )
        .unwrap();
        let folder_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO threads(account_id,jmap_id) VALUES(?1,'thread-a')",
            params![id],
        )
        .unwrap();
        let thread_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO messages(account_id,folder_id,thread_id,date,jmap_id,jmap_blob_id)
             VALUES(?1,?2,?3,1,'email-a','blob-a')",
            params![id, folder_id, thread_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jmap_sync_state(account_id,email_state) VALUES(?1,'state-a')",
            params![id],
        )
        .unwrap();

        update_password(&conn, id, &initial).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT email_state FROM jmap_sync_state WHERE account_id=?1",
                params![id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "state-a"
        );

        let changed = NewAccount {
            jmap_url: "https://new.test.dev",
            jmap_account_id: Some("account-b"),
            ..initial
        };
        let mutation = crate::db::repo::actions::enqueue(
            &conn,
            id,
            "star",
            None,
            None,
            &serde_json::json!({}),
            None,
        )
        .unwrap();
        let send = crate::db::repo::actions::enqueue(
            &conn,
            id,
            "send",
            None,
            None,
            &serde_json::json!({}),
            None,
        )
        .unwrap();
        crate::db::repo::actions::set_state(&conn, send, "inflight", None).unwrap();
        update_password(&conn, id, &changed).unwrap();
        assert!(
            conn.query_row(
                "SELECT email_state FROM jmap_sync_state WHERE account_id=?1",
                params![id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .unwrap()
            .is_none()
        );
        for query in [
            "SELECT jmap_id FROM folders WHERE id=?1",
            "SELECT jmap_id FROM threads WHERE id=?1",
            "SELECT jmap_id FROM messages WHERE account_id=?1",
        ] {
            assert!(
                conn.query_row(
                    query,
                    params![if query.contains("account_id") {
                        id
                    } else if query.contains("folders") {
                        folder_id
                    } else {
                        thread_id
                    }],
                    |row| row.get::<_, Option<String>>(0)
                )
                .unwrap()
                .is_none()
            );
        }
        assert_eq!(
            crate::db::repo::actions::get(&conn, mutation)
                .unwrap()
                .unwrap()
                .state,
            "cancelled"
        );
        let interrupted_send = crate::db::repo::actions::get(&conn, send).unwrap().unwrap();
        assert_eq!(interrupted_send.state, "failed");
    }

    #[test]
    fn reorder_moves_before_and_after_and_persists_for_both_lists() {
        let mut conn = crate::db::testutil::conn();
        fn account(email: &str) -> NewAccount<'_> {
            NewAccount {
                email,
                display_name: None,
                avatar_url: None,
                provider: Provider::Imap,
                auth_kind: AuthKind::Password,
                mail_protocol: MailProtocol::Imap,
                username: email,
                jmap_url: "",
                jmap_account_id: None,
                imap_host: "imap.test.dev",
                imap_port: 993,
                smtp_host: "smtp.test.dev",
                smtp_port: 465,
            }
        }
        let first = insert(&conn, &account("first@test.dev")).unwrap();
        let second = insert(&conn, &account("second@test.dev")).unwrap();
        let third = insert(&conn, &account("third@test.dev")).unwrap();

        reorder(&mut conn, first, third, true).unwrap();
        assert_eq!(
            list(&conn)
                .unwrap()
                .into_iter()
                .map(|account| account.id)
                .collect::<Vec<_>>(),
            vec![second, third, first]
        );
        assert_eq!(
            list_configs(&conn)
                .unwrap()
                .into_iter()
                .map(|account| account.id)
                .collect::<Vec<_>>(),
            vec![second, third, first]
        );

        reorder(&mut conn, first, second, false).unwrap();
        assert_eq!(
            list(&conn)
                .unwrap()
                .into_iter()
                .map(|account| account.id)
                .collect::<Vec<_>>(),
            vec![first, second, third]
        );
    }
}
