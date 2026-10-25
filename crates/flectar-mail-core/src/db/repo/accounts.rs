use crate::accounts::{mailbox_key, same_mailbox};
use crate::error::Result;
use crate::models::*;
use crate::pop3::Expire;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::parse_json_column;

pub const CREDENTIAL_SETUP_STATE: &str = "credential_setup";

fn account_from_row(row: &Row) -> rusqlite::Result<Account> {
    Ok(Account {
        id: row.get("id")?,
        email: row.get("email")?,
        display_name: row.get("display_name")?,
        avatar_url: row.get("avatar_url")?,
        provider: row.get("provider")?,
        auth_kind: row.get("auth_kind")?,
        mail_protocol: row.get("mail_protocol")?,
        sync_state: row.get("sync_state")?,
        sync_error: row.get("sync_error")?,
        can_create_top_level_mailbox: row.get::<_, i64>("can_create_top_level_mailbox")? != 0,
        server_keeps_mail_days: row.get::<_, Option<String>>("expire")?.and_then(|value| {
            match super::pop3::parse_expire(&value)? {
                Expire::Days(days) => Some(days),
                Expire::Never => None,
            }
        }),
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
        provider: row.get("provider")?,
        auth_kind: row.get("auth_kind")?,
        mail_protocol: row.get("mail_protocol")?,
        username: row.get("username")?,
        jmap_url: row.get("jmap_url")?,
        jmap_account_id: row.get("jmap_account_id")?,
        incoming_host: row.get("incoming_host")?,
        incoming_port: row.get("incoming_port")?,
        smtp_host: row.get("smtp_host")?,
        smtp_port: row.get("smtp_port")?,
        settings,
    })
}

pub fn list(conn: &Connection) -> Result<Vec<Account>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.email, a.display_name, a.avatar_url, a.provider, a.auth_kind,
                a.mail_protocol, a.sync_state, a.sync_error, a.can_create_top_level_mailbox,
                s.expire
         FROM accounts a LEFT JOIN pop3_sync_state s ON s.account_id = a.id
         WHERE a.sync_state <> ?1
         ORDER BY a.sort_order, a.id",
    )?;
    let rows = stmt
        .query_map(params![CREDENTIAL_SETUP_STATE], account_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn list_configs(conn: &Connection) -> Result<Vec<AccountConfig>> {
    let mut stmt = conn.prepare(
        "SELECT id, email, display_name, avatar_url, provider, auth_kind,
                mail_protocol, username, jmap_url, jmap_account_id, incoming_host,
                incoming_port, smtp_host, smtp_port, settings_json
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
                mail_protocol, username, jmap_url, jmap_account_id, incoming_host,
                incoming_port, smtp_host, smtp_port, settings_json
         FROM accounts WHERE id = ?1",
    )?;
    Ok(stmt.query_row(params![id], config_from_row).optional()?)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Account>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.email, a.display_name, a.avatar_url, a.provider, a.auth_kind,
                a.mail_protocol, a.sync_state, a.sync_error, a.can_create_top_level_mailbox,
                s.expire
         FROM accounts a LEFT JOIN pop3_sync_state s ON s.account_id = a.id
         WHERE a.id = ?1",
    )?;
    Ok(stmt.query_row(params![id], account_from_row).optional()?)
}

/// The account whose address belongs to the same mailbox as `email`, in any
/// spelling (see [`mailbox_key`]). An account stored with exactly this
/// address wins over another spelling of it.
pub fn find_by_mailbox(conn: &Connection, email: &str) -> Result<Option<Account>> {
    let email = email.trim();
    let key = mailbox_key(email);
    let mut statement = conn.prepare("SELECT id, email FROM accounts ORDER BY id")?;
    let accounts = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let id = accounts
        .iter()
        .find(|(_, stored)| stored.eq_ignore_ascii_case(email))
        .or_else(|| {
            accounts
                .iter()
                .find(|(_, stored)| mailbox_key(stored) == key)
        })
        .map(|(id, _)| *id);
    match id {
        Some(id) => get(conn, id),
        None => Ok(None),
    }
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
    pub incoming_host: &'a str,
    pub incoming_port: u16,
    pub smtp_host: &'a str,
    pub smtp_port: u16,
}

pub fn insert(conn: &Connection, a: &NewAccount, settings: &AccountSettings) -> Result<i64> {
    insert_with_sync_state(conn, a, settings, "idle")
}

pub fn insert_with_sync_state(
    conn: &Connection,
    a: &NewAccount,
    settings: &AccountSettings,
    sync_state: &str,
) -> Result<i64> {
    let settings_json = serde_json::to_string(settings)?;
    // JMAP advertises top-level mailbox rights during discovery; until then
    // the UI must not offer a mutation the server may reject. Other
    // protocols validate the operation when it is attempted.
    let can_create_top_level_mailbox = match a.mail_protocol {
        MailProtocol::Gmail | MailProtocol::Imap | MailProtocol::Pop3 => true,
        MailProtocol::Jmap => false,
    };
    conn.execute(
        "INSERT INTO accounts (email, display_name, avatar_url, provider, auth_kind, mail_protocol, username,
                               jmap_url, jmap_account_id,
                               incoming_host, incoming_port, smtp_host, smtp_port, created_at,
                               sort_order, settings_json, sync_state,
                               can_create_top_level_mailbox)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
                 COALESCE((SELECT MAX(sort_order) + 1 FROM accounts), 0), ?15, ?16, ?17)",
        params![
            a.email,
            a.display_name,
            a.avatar_url,
            a.provider,
            a.auth_kind,
            a.mail_protocol,
            a.username,
            a.jmap_url,
            a.jmap_account_id,
            a.incoming_host,
            a.incoming_port,
            a.smtp_host,
            a.smtp_port,
            now_ms(),
            settings_json,
            sync_state,
            can_create_top_level_mailbox,
        ],
    )?;
    let account_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO sender_identities (
           account_id,email,display_name,is_primary,is_provider_default,
           verification_status,last_synced_at
         ) VALUES (?1,?2,?3,1,1,'accepted',?4)",
        params![account_id, a.email, a.display_name, now_ms()],
    )?;
    match a.mail_protocol {
        MailProtocol::Pop3 => super::pop3::create_account_state(conn, account_id)?,
        // Folders come from the server.
        MailProtocol::Gmail | MailProtocol::Imap | MailProtocol::Jmap => {}
    }
    Ok(account_id)
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
    let (old_protocol, old_jmap_url, old_jmap_account_id, old_outgoing, old_incoming_host): (
        MailProtocol,
        String,
        Option<String>,
        (String, u16, String),
        String,
    ) = conn.query_row(
        "SELECT mail_protocol,jmap_url,jmap_account_id,smtp_host,smtp_port,username,incoming_host
         FROM accounts WHERE id=?1",
        params![id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                (row.get(3)?, row.get(4)?, row.get(5)?),
                row.get(6)?,
            ))
        },
    )?;
    // POP3 mail exists only on this device, so it can't move to or from a
    // protocol that keeps mail on the server.
    if (old_protocol == MailProtocol::Pop3) != (a.mail_protocol == MailProtocol::Pop3) {
        return Err(crate::error::CoreError::Other(
            "an account can't switch between POP3 and IMAP or JMAP".into(),
        ));
    }
    let jmap_identity_changed = (old_protocol == MailProtocol::Jmap
        || a.mail_protocol == MailProtocol::Jmap)
        && (old_protocol != a.mail_protocol
            || old_jmap_url != a.jmap_url
            || old_jmap_account_id.as_deref() != a.jmap_account_id);
    // Addresses the user declared stay while the same server judges them:
    // re-entering a password (as every imported account must) is not a
    // reason to forget them. A JMAP server only allows those under its own
    // wildcard identities, so a new identity source drops them.
    let keep_manual = !jmap_identity_changed;
    let outgoing_changed = (
        old_outgoing.0.as_str(),
        old_outgoing.1,
        old_outgoing.2.as_str(),
    ) != (a.smtp_host, a.smtp_port, a.username);
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
        "UPDATE accounts SET email = ?2, display_name = COALESCE(?3, display_name),
                avatar_url = ?4, provider = ?5,
                auth_kind = ?6, mail_protocol = ?7, username = ?8,
                jmap_url = ?9, jmap_account_id = ?10, incoming_host = ?11, incoming_port = ?12,
                smtp_host = ?13, smtp_port = ?14, sync_state = 'idle', sync_error = NULL
         WHERE id = ?1",
        params![
            id,
            a.email,
            a.display_name,
            a.avatar_url,
            a.provider,
            a.auth_kind,
            a.mail_protocol,
            a.username,
            a.jmap_url,
            a.jmap_account_id,
            a.incoming_host,
            a.incoming_port,
            a.smtp_host,
            a.smtp_port,
        ],
    )?;
    // A password-account edit invalidates any provider aliases discovered for
    // the old connection. Seed the new authenticated address after the
    // account row itself has been accepted (for example, by its UNIQUE email
    // constraint), so a rejected edit cannot leave mismatched identities.
    conn.execute(
        "DELETE FROM sender_identities WHERE account_id=?1 AND (source<>'manual' OR ?2=0)",
        params![id, keep_manual],
    )?;
    if outgoing_changed {
        // What the old outgoing server did with an address says nothing
        // about the new one.
        conn.execute(
            "UPDATE sender_identities SET delivery_status='unknown',delivery_error=NULL
             WHERE account_id=?1",
            params![id],
        )?;
    }
    conn.execute(
        "INSERT INTO sender_identities (
           account_id,email,display_name,is_primary,is_provider_default,
           verification_status,last_synced_at
         ) VALUES (?1,?2,(SELECT display_name FROM accounts WHERE id=?1),1,1,'accepted',?3)
         ON CONFLICT(account_id,email) DO UPDATE SET
           display_name=excluded.display_name,is_primary=1,is_provider_default=1,
           verification_status='accepted',source='provider'",
        params![id, a.email, now_ms()],
    )?;
    // UIDLs only identify messages on the server and mailbox that issued
    // them.
    if a.mail_protocol == MailProtocol::Pop3
        && (!old_incoming_host.eq_ignore_ascii_case(a.incoming_host)
            || old_outgoing.2 != a.username)
    {
        super::pop3::forget_server(conn, id)?;
    }
    let imap_identity_changed = a.mail_protocol == MailProtocol::Imap
        && (old_protocol != MailProtocol::Imap
            || !old_incoming_host.eq_ignore_ascii_case(a.incoming_host)
            || old_outgoing.2 != a.username);
    // Mail from the previous server or protocol keeps its local state and
    // waits to be matched by Message-ID with its copy on the new server; what
    // isn't there goes once the new server's mail has been gone through. This
    // runs before the identities below are forgotten: they tell the mail that
    // came from a server apart from the sent mail only this device holds.
    if imap_identity_changed || jmap_identity_changed {
        super::messages::await_relink(conn, id, None)?;
    }
    // UIDs only identify messages on the IMAP server that issued them.
    // Sending, and filing what was sent, don't depend on the old server, so
    // they stay queued.
    if imap_identity_changed {
        conn.execute(
            "UPDATE pending_actions
             SET state='cancelled',finished_at=?2,
                 last_error='remote mail account changed before this action completed'
             WHERE account_id=?1 AND state IN ('pending','inflight')
               AND kind NOT IN ('send','append_sent')",
            params![id, now_ms()],
        )?;
        conn.execute(
            "UPDATE folders SET uidvalidity=NULL, uidnext=NULL, highestmodseq=NULL,
                    last_seen_uid=0, backfill_cursor=NULL, backfill_done=0
             WHERE account_id=?1",
            params![id],
        )?;
        conn.execute("DELETE FROM sync_failures WHERE account_id=?1", params![id])?;
    } else if a.mail_protocol != MailProtocol::Imap {
        // Only an IMAP server leaves folders out of its list.
        conn.execute(
            "UPDATE folders SET missing_on_server=0
             WHERE account_id=?1 AND missing_on_server=1",
            params![id],
        )?;
    }
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
            "DELETE FROM jmap_full_sync_stored WHERE account_id=?1",
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
        if old_protocol == MailProtocol::Jmap && a.mail_protocol == MailProtocol::Jmap {
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

/// Stores the spelling of an OAuth account's address that its provider
/// signed in with, such as Gmail's dotted form of an imported login. The
/// account signs in as that address, and its own sender identity, default
/// sender and drafts move to it. Fails when another account already has the
/// mailbox.
pub fn set_signed_in_address(conn: &Connection, id: i64, email: &str) -> Result<()> {
    let mut config = get_config(conn, id)?
        .ok_or_else(|| crate::error::CoreError::NotFound(format!("account {id}")))?;
    let previous = std::mem::replace(&mut config.email, email.to_owned());
    if previous == email {
        return Ok(());
    }
    if !same_mailbox(&previous, email) {
        return Err(crate::error::CoreError::Auth(format!(
            "{email} is a different mailbox than {previous}"
        )));
    }
    let mut statement = conn.prepare("SELECT email FROM accounts WHERE id <> ?1")?;
    let others = statement
        .query_map(params![id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if let Some(other) = others.iter().find(|other| same_mailbox(other, email)) {
        return Err(crate::error::CoreError::Other(format!(
            "{other} is already added as another account"
        )));
    }

    conn.execute(
        "UPDATE accounts SET email = ?2, username = ?2 WHERE id = ?1",
        params![id, email],
    )?;
    // The identity keeps its settings under the new spelling, unless the
    // provider already listed that spelling.
    conn.execute(
        "UPDATE OR IGNORE sender_identities SET email = ?3 WHERE account_id = ?1 AND email = ?2",
        params![id, previous, email],
    )?;
    conn.execute(
        "DELETE FROM sender_identities WHERE account_id = ?1 AND email = ?2 AND email <> ?3",
        params![id, previous, email],
    )?;
    super::sender_identities::ensure_primary(conn, &config)?;
    if config
        .settings
        .default_sender_email
        .as_deref()
        .is_some_and(|sender| sender.eq_ignore_ascii_case(&previous))
    {
        config.settings.default_sender_email = Some(email.to_owned());
        set_settings(conn, id, &config.settings)?;
    }
    // Sending checks the From address of a draft against the identities.
    conn.execute(
        "UPDATE messages SET from_addr = ?3
         WHERE account_id = ?1 AND is_draft = 1 AND from_addr = ?2 COLLATE NOCASE",
        params![id, previous, email],
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
    update_sync_state_with_error(conn, id, state, error)?;
    Ok(())
}

/// Persist a state transition and report whether the observable state changed.
/// The null-safe comparison prevents tight sync follow-up loops from flooding
/// UI subscribers with duplicate events.
pub fn update_sync_state_with_error(
    conn: &Connection,
    id: i64,
    state: &str,
    error: Option<&str>,
) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE accounts SET sync_state = ?2, sync_error = ?3
         WHERE id = ?1 AND (sync_state IS NOT ?2 OR sync_error IS NOT ?3)",
        params![id, state, error],
    )? != 0)
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
    use crate::db::testutil::account_settings as settings;

    #[test]
    fn password_edits_keep_declared_identities_and_the_display_name() {
        let conn = crate::db::testutil::conn();
        let account = NewAccount {
            email: "me@test.dev",
            display_name: Some("Me Myself"),
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            username: "me",
            jmap_url: "",
            jmap_account_id: None,
            incoming_host: "imap.test.dev",
            incoming_port: 993,
            smtp_host: "smtp.test.dev",
            smtp_port: 465,
        };
        let id = insert_with_sync_state(&conn, &account, &settings(), "needs_reauth").unwrap();
        let identity = |email: &str, source| SenderIdentity {
            account_id: id,
            email: email.into(),
            display_name: None,
            reply_to_email: None,
            is_primary: false,
            is_provider_default: false,
            verification_status: "accepted".into(),
            last_synced_at: 0,
            source,
            delivery_status: Default::default(),
            delivery_error: None,
        };
        super::super::sender_identities::save_manual(
            &conn,
            &identity("alias@test.dev", SenderIdentitySource::Manual),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sender_identities (account_id,email,is_primary,is_provider_default,
               verification_status,last_synced_at,source)
             VALUES (?1,'discovered@test.dev',0,0,'accepted',0,'provider')",
            params![id],
        )
        .unwrap();

        // The account editor sends no display name when the user signs in.
        update_password(
            &conn,
            id,
            &NewAccount {
                display_name: None,
                ..account
            },
        )
        .unwrap();

        assert_eq!(
            get(&conn, id).unwrap().unwrap().display_name.as_deref(),
            Some("Me Myself")
        );
        let identities = super::super::sender_identities::list(&conn, id).unwrap();
        let emails = identities
            .iter()
            .map(|identity| identity.email.as_str())
            .collect::<Vec<_>>();
        assert_eq!(emails, ["me@test.dev", "alias@test.dev"]);
        assert!(identities[0].is_primary);
        assert_eq!(identities[0].display_name.as_deref(), Some("Me Myself"));
    }

    #[test]
    fn password_edits_reassess_declared_identities_for_a_new_server() {
        let conn = crate::db::testutil::conn();
        let account = NewAccount {
            email: "me@test.dev",
            display_name: None,
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            username: "me",
            jmap_url: "",
            jmap_account_id: None,
            incoming_host: "imap.test.dev",
            incoming_port: 993,
            smtp_host: "smtp.test.dev",
            smtp_port: 465,
        };
        let id = insert(&conn, &account, &settings()).unwrap();
        super::super::sender_identities::save_manual(
            &conn,
            &SenderIdentity {
                account_id: id,
                email: "alias@test.dev".into(),
                display_name: None,
                reply_to_email: None,
                is_primary: false,
                is_provider_default: false,
                verification_status: "accepted".into(),
                last_synced_at: 0,
                source: SenderIdentitySource::Manual,
                delivery_status: Default::default(),
                delivery_error: None,
            },
        )
        .unwrap();
        super::super::sender_identities::record_delivery(
            &conn,
            id,
            "alias@test.dev",
            SenderDeliveryStatus::Rejected,
            Some("550 sender not allowed"),
        )
        .unwrap();

        // The new outgoing server has not refused the alias yet.
        let moved = NewAccount {
            smtp_host: "mail.test.dev",
            ..account
        };
        update_password(&conn, id, &moved).unwrap();
        let identities = super::super::sender_identities::list(&conn, id).unwrap();
        let alias = identities
            .iter()
            .find(|identity| identity.email == "alias@test.dev")
            .unwrap();
        assert_eq!(alias.delivery_status, SenderDeliveryStatus::Unknown);
        assert_eq!(alias.delivery_error, None);

        // A JMAP server only allows the addresses it grants.
        update_password(
            &conn,
            id,
            &NewAccount {
                mail_protocol: MailProtocol::Jmap,
                jmap_url: "https://jmap.test.dev/.well-known/jmap",
                ..moved
            },
        )
        .unwrap();
        let identities = super::super::sender_identities::list(&conn, id).unwrap();
        let emails = identities
            .iter()
            .map(|identity| identity.email.as_str())
            .collect::<Vec<_>>();
        assert_eq!(emails, ["me@test.dev"]);
    }

    #[test]
    fn find_by_mailbox_is_case_insensitive_and_update_keeps_account_id() {
        let conn = crate::db::testutil::conn();
        let first = NewAccount {
            email: "me@test.dev",
            display_name: None,
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            username: "me",
            jmap_url: "",
            jmap_account_id: None,
            incoming_host: "imap.test.dev",
            incoming_port: 993,
            smtp_host: "smtp.test.dev",
            smtp_port: 465,
        };
        let id = insert(&conn, &first, &settings()).unwrap();
        assert_eq!(
            get_config(&conn, id)
                .unwrap()
                .unwrap()
                .settings
                .mail_history,
            MailHistory::SixMonths
        );

        assert_eq!(
            find_by_mailbox(&conn, "ME@TEST.DEV").unwrap().unwrap().id,
            id
        );

        let updated = NewAccount {
            email: "me@test.dev",
            display_name: Some("Me"),
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            username: "updated-user",
            jmap_url: "",
            jmap_account_id: None,
            incoming_host: "imap2.test.dev",
            incoming_port: 993,
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
            mail_protocol: MailProtocol::Gmail,
            username: "pending@test.dev",
            jmap_url: "",
            jmap_account_id: None,
            incoming_host: "imap.gmail.com",
            incoming_port: 993,
            smtp_host: "smtp.gmail.com",
            smtp_port: 465,
        };
        let id =
            insert_with_sync_state(&conn, &account, &settings(), CREDENTIAL_SETUP_STATE).unwrap();

        assert!(list(&conn).unwrap().is_empty());
        assert!(list_configs(&conn).unwrap().is_empty());
        assert_eq!(
            find_by_mailbox(&conn, "PENDING@test.dev")
                .unwrap()
                .unwrap()
                .id,
            id
        );

        set_sync_state(&conn, id, "idle").unwrap();
        assert_eq!(list(&conn).unwrap().len(), 1);
        assert_eq!(list_configs(&conn).unwrap().len(), 1);
    }

    fn google(email: &str) -> NewAccount<'_> {
        NewAccount {
            email,
            display_name: None,
            avatar_url: None,
            provider: Provider::Gmail,
            auth_kind: AuthKind::Oauth2,
            mail_protocol: MailProtocol::Gmail,
            username: email,
            jmap_url: "",
            jmap_account_id: None,
            incoming_host: "imap.gmail.com",
            incoming_port: 993,
            smtp_host: "smtp.gmail.com",
            smtp_port: 465,
        }
    }

    #[test]
    fn find_by_mailbox_matches_gmail_spellings_and_prefers_the_exact_address() {
        let conn = crate::db::testutil::conn();
        let imported = insert(&conn, &google("johndoe@gmail.com"), &settings()).unwrap();
        let find = |email: &str| find_by_mailbox(&conn, email).unwrap().map(|found| found.id);
        assert_eq!(find(" John.Doe@googlemail.com "), Some(imported));
        assert_eq!(find("jane@gmail.com"), None);

        // Older versions could add a second spelling of the same mailbox.
        let added = insert(&conn, &google("john.doe@gmail.com"), &settings()).unwrap();
        assert_eq!(find("JOHN.DOE@gmail.com"), Some(added));
        assert_eq!(find("j.ohndoe@gmail.com"), Some(imported));

        let workspace = insert(&conn, &google("jane.doe@example.com"), &settings()).unwrap();
        assert_eq!(find("Jane.Doe@Example.com"), Some(workspace));
        assert_eq!(find("janedoe@example.com"), None);
    }

    #[test]
    fn signed_in_address_moves_the_identity_default_sender_and_drafts() {
        let conn = crate::db::testutil::conn();
        let mut account_settings = settings();
        account_settings.default_sender_email = Some("JohnDoe@gmail.com".into());
        let id = insert_with_sync_state(
            &conn,
            &google("johndoe@gmail.com"),
            &account_settings,
            "needs_reauth",
        )
        .unwrap();
        let message = |is_draft: bool| {
            conn.execute(
                "INSERT INTO messages (account_id, from_addr, date, is_draft)
                 VALUES (?1, 'johndoe@gmail.com', 0, ?2)",
                params![id, is_draft],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let (draft, received) = (message(true), message(false));

        set_signed_in_address(&conn, id, "john.doe@gmail.com").unwrap();

        let config = get_config(&conn, id).unwrap().unwrap();
        assert_eq!(config.email, "john.doe@gmail.com");
        assert_eq!(config.username, "john.doe@gmail.com");
        assert_eq!(
            config.settings.default_sender_email.as_deref(),
            Some("john.doe@gmail.com")
        );
        let identities = super::super::sender_identities::list(&conn, id).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].email, "john.doe@gmail.com");
        assert!(identities[0].is_primary);
        let from = |message: i64| {
            conn.query_row(
                "SELECT from_addr FROM messages WHERE id = ?1",
                [message],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
        };
        assert_eq!(from(draft), "john.doe@gmail.com");
        assert_eq!(from(received), "johndoe@gmail.com");
    }

    #[test]
    fn signed_in_address_keeps_one_identity_and_only_respells_its_own_mailbox() {
        let conn = crate::db::testutil::conn();
        let id = insert(&conn, &google("JohnDoe@gmail.com"), &settings()).unwrap();
        let emails = || {
            super::super::sender_identities::list(&conn, id)
                .unwrap()
                .into_iter()
                .map(|identity| (identity.email, identity.is_primary))
                .collect::<Vec<_>>()
        };

        set_signed_in_address(&conn, id, "johndoe@gmail.com").unwrap();
        assert_eq!(emails(), [("johndoe@gmail.com".to_owned(), true)]);

        // Gmail already listed the dotted spelling as a send-as address.
        conn.execute(
            "INSERT INTO sender_identities (account_id, email, is_primary, is_provider_default)
             VALUES (?1, 'john.doe@gmail.com', 0, 0)",
            params![id],
        )
        .unwrap();
        set_signed_in_address(&conn, id, "john.doe@gmail.com").unwrap();
        assert_eq!(emails(), [("john.doe@gmail.com".to_owned(), true)]);

        assert!(set_signed_in_address(&conn, id, "jane@gmail.com").is_err());
        insert(&conn, &google("j.ohndoe@googlemail.com"), &settings()).unwrap();
        let error = set_signed_in_address(&conn, id, "johndoe@gmail.com").unwrap_err();
        assert_eq!(
            error.to_string(),
            "j.ohndoe@googlemail.com is already added as another account"
        );
        assert_eq!(
            get_config(&conn, id).unwrap().unwrap().email,
            "john.doe@gmail.com"
        );
    }

    #[test]
    fn stored_account_values_are_read_strictly() {
        let conn = crate::db::testutil::conn();
        conn.execute(
            "INSERT INTO accounts (
               email, provider, auth_kind, mail_protocol, username, incoming_host,
               incoming_port, smtp_host, smtp_port, created_at, settings_json
             ) VALUES ('stored@test.dev', 'generic', 'password', 'imap', 'stored', 'imap',
                       143, 'smtp', 587, 0,
                       '{\"connection\":{\"incomingSecurity\":\"starttls\",\"smtpSecurity\":\"starttls\"}}')",
            [],
        )
        .unwrap();
        let config = get_config(&conn, 1).unwrap().unwrap();
        assert_eq!(config.settings.mail_history, MailHistory::SixMonths);
        assert_eq!(
            config.settings.connection.incoming_security,
            ConnectionSecurity::Starttls
        );
        assert_eq!(config.incoming_port, 143);

        conn.execute("UPDATE accounts SET settings_json = '{}' WHERE id = 1", [])
            .unwrap();
        assert!(
            get_config(&conn, 1).is_err(),
            "connection security is required"
        );
    }

    #[test]
    fn sync_state_update_reports_only_observable_transitions() {
        let conn = crate::db::testutil::conn();
        crate::db::testutil::seed_account(&conn);

        assert!(update_sync_state_with_error(&conn, 1, "syncing", None).unwrap());
        assert!(!update_sync_state_with_error(&conn, 1, "syncing", None).unwrap());
        assert!(
            update_sync_state_with_error(&conn, 1, "retrying", Some("connection lost")).unwrap()
        );
        assert!(
            !update_sync_state_with_error(&conn, 1, "retrying", Some("connection lost")).unwrap()
        );
        assert!(update_sync_state_with_error(&conn, 1, "idle", None).unwrap());
    }

    #[test]
    fn changing_jmap_identity_invalidates_all_remote_checkpoints() {
        let conn = crate::db::testutil::conn();
        let initial = NewAccount {
            email: "me@test.dev",
            display_name: None,
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Jmap,
            username: "me@test.dev",
            jmap_url: "https://mail.test.dev",
            jmap_account_id: Some("account-a"),
            incoming_host: "",
            incoming_port: 993,
            smtp_host: "",
            smtp_port: 465,
        };
        let id = insert(&conn, &initial, &settings()).unwrap();
        assert!(
            !get(&conn, id)
                .unwrap()
                .unwrap()
                .can_create_top_level_mailbox,
            "JMAP creation controls stay hidden until the session advertises permission"
        );
        conn.execute(
            "INSERT INTO folders(account_id,path,jmap_id) VALUES(?1,'Inbox','mailbox-a')",
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
        conn.execute(
            "INSERT INTO jmap_full_sync_stored(account_id,email_id) VALUES(?1,'email-a')",
            params![id],
        )
        .unwrap();
        let full_sync_progress = |conn: &Connection| {
            conn.query_row(
                "SELECT COUNT(*) FROM jmap_full_sync_stored WHERE account_id=?1",
                params![id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };

        update_password(&conn, id, &initial).unwrap();
        assert_eq!(full_sync_progress(&conn), 1);
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
        let waiting = |conn: &Connection| {
            conn.query_row(
                "SELECT relink_pending FROM messages WHERE account_id=?1",
                params![id],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
        };
        assert!(!waiting(&conn));
        update_password(&conn, id, &changed).unwrap();
        assert!(waiting(&conn), "mail of the old server waits for its copy");
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
        assert_eq!(full_sync_progress(&conn), 0);
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
                provider: Provider::Generic,
                auth_kind: AuthKind::Password,
                mail_protocol: MailProtocol::Imap,
                username: email,
                jmap_url: "",
                jmap_account_id: None,
                incoming_host: "imap.test.dev",
                incoming_port: 993,
                smtp_host: "smtp.test.dev",
                smtp_port: 465,
            }
        }
        let first = insert(&conn, &account("first@test.dev"), &settings()).unwrap();
        let second = insert(&conn, &account("second@test.dev"), &settings()).unwrap();
        let third = insert(&conn, &account("third@test.dev"), &settings()).unwrap();

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

    #[test]
    fn pop3_edits_keep_the_ledger_only_for_the_same_server_and_user() {
        let conn = crate::db::testutil::conn();
        let initial = NewAccount {
            email: "me@test.dev",
            display_name: None,
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Pop3,
            username: "me",
            jmap_url: "",
            jmap_account_id: None,
            incoming_host: "pop.test.dev",
            incoming_port: 995,
            smtp_host: "smtp.test.dev",
            smtp_port: 465,
        };
        let id = insert(&conn, &initial, &settings()).unwrap();
        let known = |conn: &Connection| super::super::pop3::known_uidls(conn, id).unwrap().len();
        super::super::pop3::record_skipped(&conn, id, "a", 1, 1).unwrap();

        // Another port or letter case reaches the same mailbox.
        let same = NewAccount {
            incoming_host: "POP.test.dev",
            incoming_port: 110,
            ..initial
        };
        update_password(&conn, id, &same).unwrap();
        assert_eq!(known(&conn), 1);

        let other_user = NewAccount {
            username: "other",
            ..same
        };
        update_password(&conn, id, &other_user).unwrap();
        assert_eq!(known(&conn), 0);

        super::super::pop3::record_skipped(&conn, id, "b", 1, 1).unwrap();
        let other_server = NewAccount {
            incoming_host: "mail.test.dev",
            ..other_user
        };
        update_password(&conn, id, &other_server).unwrap();
        assert_eq!(known(&conn), 0);

        let to_imap = NewAccount {
            mail_protocol: MailProtocol::Imap,
            ..other_server
        };
        assert!(update_password(&conn, id, &to_imap).is_err());
    }

    fn relink_state(conn: &Connection, id: i64) -> (bool, Option<i64>) {
        conn.query_row(
            "SELECT relink_pending, uid FROM messages WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    }

    fn seed_message(
        conn: &Connection,
        account_id: i64,
        folder_id: i64,
        uid: Option<i64>,
        jmap_id: Option<&str>,
        is_outgoing: bool,
    ) -> i64 {
        conn.execute(
            "INSERT INTO messages (account_id, folder_id, uid, jmap_id, is_outgoing, date)
             VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            params![account_id, folder_id, uid, jmap_id, is_outgoing],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn another_imap_server_unlinks_the_mail_that_came_from_a_server() {
        let conn = crate::db::testutil::conn();
        let initial = NewAccount {
            email: "me@test.dev",
            display_name: None,
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            username: "me",
            jmap_url: "",
            jmap_account_id: None,
            incoming_host: "imap.test.dev",
            incoming_port: 993,
            smtp_host: "smtp.test.dev",
            smtp_port: 465,
        };
        let id = insert(&conn, &initial, &settings()).unwrap();
        let inbox =
            super::super::folders::upsert(&conn, id, "INBOX", Some("/"), Some("inbox")).unwrap();
        let sent =
            super::super::folders::upsert(&conn, id, "Sent", Some("/"), Some("sent")).unwrap();
        super::super::folders::set_uid_state(&conn, inbox, Some(7), Some(9), None).unwrap();
        super::super::folders::set_last_seen_uid(&conn, inbox, 8).unwrap();
        super::super::folders::set_backfill(&conn, inbox, Some(1), true).unwrap();
        let received = seed_message(&conn, id, inbox, Some(5), None, false);
        let filed = seed_message(&conn, id, sent, Some(6), None, true);
        let only_here = seed_message(&conn, id, sent, None, None, true);
        let draft = seed_message(&conn, id, sent, None, None, false);
        conn.execute(
            "INSERT INTO drafts_meta (message_id) VALUES (?1)",
            params![draft],
        )
        .unwrap();
        let actions = [
            ("star", received),
            ("append_sent", only_here),
            ("send", draft),
        ]
        .map(|(kind, message_id)| {
            crate::db::repo::actions::enqueue(
                &conn,
                id,
                kind,
                Some(message_id),
                None,
                &serde_json::json!({}),
                None,
            )
            .unwrap()
        });
        super::super::sync_failures::record_header(&conn, inbox, 4, None, "bad header").unwrap();
        let states = |conn: &Connection| {
            actions.map(|action| {
                crate::db::repo::actions::get(conn, action)
                    .unwrap()
                    .unwrap()
                    .state
            })
        };

        // Another port or letter case reaches the same mailbox.
        let same = NewAccount {
            incoming_host: "IMAP.test.dev",
            incoming_port: 143,
            ..initial
        };
        update_password(&conn, id, &same).unwrap();
        assert_eq!(relink_state(&conn, received), (false, Some(5)));
        assert_eq!(states(&conn), ["pending", "pending", "pending"]);

        let moved = NewAccount {
            incoming_host: "mail.example.net",
            ..same
        };
        update_password(&conn, id, &moved).unwrap();
        assert_eq!(relink_state(&conn, received), (true, None));
        assert_eq!(relink_state(&conn, filed), (true, None));
        assert_eq!(relink_state(&conn, only_here), (false, None));
        assert_eq!(relink_state(&conn, draft), (false, None));
        assert_eq!(states(&conn), ["cancelled", "pending", "pending"]);
        let folder = super::super::folders::get(&conn, inbox).unwrap().unwrap();
        assert_eq!(
            (
                folder.uidvalidity,
                folder.last_seen_uid,
                folder.backfill_cursor
            ),
            (None, 0, None)
        );
        assert!(!folder.backfill_done);
        let failures: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_failures WHERE account_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(failures, 0);

        // Another user on the same server is another mailbox.
        conn.execute(
            "UPDATE messages SET uid = 5, relink_pending = 0 WHERE id = ?1",
            params![received],
        )
        .unwrap();
        update_password(
            &conn,
            id,
            &NewAccount {
                username: "other",
                ..moved
            },
        )
        .unwrap();
        assert_eq!(relink_state(&conn, received), (true, None));

        // A JMAP server matches the same mail, and has no missing folders.
        conn.execute(
            "UPDATE folders SET missing_on_server = 1 WHERE id = ?1",
            params![sent],
        )
        .unwrap();
        conn.execute(
            "UPDATE messages SET uid = 6, relink_pending = 0 WHERE id = ?1",
            params![filed],
        )
        .unwrap();
        update_password(
            &conn,
            id,
            &NewAccount {
                username: "other",
                mail_protocol: MailProtocol::Jmap,
                jmap_url: "https://mail.example.net/.well-known/jmap",
                ..moved
            },
        )
        .unwrap();
        assert_eq!(relink_state(&conn, received), (true, None));
        assert_eq!(relink_state(&conn, filed), (true, None));
        assert_eq!(relink_state(&conn, only_here), (false, None));
        assert_eq!(relink_state(&conn, draft), (false, None));
        let missing: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM folders WHERE missing_on_server = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(missing, 0);
    }

    #[test]
    fn switching_from_jmap_to_imap_unlinks_the_jmap_mail() {
        let conn = crate::db::testutil::conn();
        let jmap = NewAccount {
            email: "me@test.dev",
            display_name: None,
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Jmap,
            username: "me@test.dev",
            jmap_url: "https://mail.test.dev",
            jmap_account_id: Some("account-a"),
            incoming_host: "",
            incoming_port: 993,
            smtp_host: "",
            smtp_port: 465,
        };
        let id = insert(&conn, &jmap, &settings()).unwrap();
        let inbox =
            super::super::folders::upsert_jmap(&conn, id, "mailbox-a", "Inbox", Some("inbox"))
                .unwrap();
        let received = seed_message(&conn, id, inbox, None, Some("email-a"), false);
        let sent = seed_message(&conn, id, inbox, None, Some("email-b"), true);
        let only_here = seed_message(&conn, id, inbox, None, None, true);

        update_password(
            &conn,
            id,
            &NewAccount {
                mail_protocol: MailProtocol::Imap,
                jmap_url: "",
                jmap_account_id: None,
                incoming_host: "mail.test.dev",
                smtp_host: "mail.test.dev",
                ..jmap
            },
        )
        .unwrap();
        assert_eq!(relink_state(&conn, received), (true, None));
        assert_eq!(relink_state(&conn, sent), (true, None));
        assert_eq!(relink_state(&conn, only_here), (false, None));
        let jmap_ids: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE jmap_id IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(jmap_ids, 0);
        // An IMAP folder that takes over this one's name starts its history.
        assert!(
            !super::super::folders::get(&conn, inbox)
                .unwrap()
                .unwrap()
                .backfill_done
        );
    }
}
