use crate::error::{CoreError, Result};
use crate::models::{AccountConfig, SenderIdentity, now_ms};
use rusqlite::{Connection, OptionalExtension, params};

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SenderIdentity> {
    Ok(SenderIdentity {
        account_id: row.get("account_id")?,
        email: row.get("email")?,
        display_name: row.get("display_name")?,
        reply_to_email: row.get("reply_to_email")?,
        is_primary: row.get::<_, i64>("is_primary")? != 0,
        is_provider_default: row.get::<_, i64>("is_provider_default")? != 0,
        verification_status: row.get("verification_status")?,
        last_synced_at: row.get("last_synced_at")?,
    })
}

pub fn ensure_primary(conn: &Connection, account: &AccountConfig) -> Result<()> {
    conn.execute(
        "INSERT INTO sender_identities (
           account_id,email,display_name,is_primary,is_provider_default,
           verification_status,last_synced_at
         ) VALUES (?1,?2,?3,1,1,'accepted',?4)
         ON CONFLICT(account_id,email) DO UPDATE SET
           display_name=COALESCE(excluded.display_name,sender_identities.display_name),
           is_primary=1,verification_status='accepted'",
        params![account.id, account.email, account.display_name, now_ms()],
    )?;
    Ok(())
}

pub fn list(conn: &Connection, account_id: i64) -> Result<Vec<SenderIdentity>> {
    let mut statement = conn.prepare(
        "SELECT account_id,email,display_name,reply_to_email,is_primary,
                is_provider_default,verification_status,last_synced_at
         FROM sender_identities WHERE account_id=?1
         ORDER BY is_provider_default DESC,is_primary DESC,LOWER(email)",
    )?;
    Ok(statement
        .query_map(params![account_id], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Atomically replace a provider's discovered identities. The caller only
/// supplies values parsed from an authenticated provider endpoint.
pub fn replace(
    conn: &mut Connection,
    account_id: i64,
    identities: &[SenderIdentity],
) -> Result<()> {
    if identities.is_empty() || !identities.iter().any(|identity| identity.is_primary) {
        return Err(CoreError::Other(
            "sender identity discovery omitted the primary address".into(),
        ));
    }
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM sender_identities WHERE account_id=?1",
        params![account_id],
    )?;
    for identity in identities {
        tx.execute(
            "INSERT INTO sender_identities (
               account_id,email,display_name,reply_to_email,is_primary,
               is_provider_default,verification_status,last_synced_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                account_id,
                identity.email,
                identity.display_name,
                identity.reply_to_email,
                identity.is_primary as i64,
                identity.is_provider_default as i64,
                identity.verification_status,
                identity.last_synced_at,
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn get_verified(
    conn: &Connection,
    account_id: i64,
    email: &str,
) -> Result<Option<SenderIdentity>> {
    let mut statement = conn.prepare(
        "SELECT account_id,email,display_name,reply_to_email,is_primary,
                is_provider_default,verification_status,last_synced_at
         FROM sender_identities
         WHERE account_id=?1 AND email=?2 COLLATE NOCASE
           AND (is_primary=1 OR verification_status='accepted')",
    )?;
    Ok(statement
        .query_row(params![account_id, email], from_row)
        .optional()?)
}

pub fn resolve_default(conn: &Connection, account: &AccountConfig) -> Result<SenderIdentity> {
    ensure_primary(conn, account)?;
    if let Some(email) = account.settings.default_sender_email.as_deref()
        && let Some(identity) = get_verified(conn, account.id, email)?
    {
        return Ok(identity);
    }
    let mut statement = conn.prepare(
        "SELECT account_id,email,display_name,reply_to_email,is_primary,
                is_provider_default,verification_status,last_synced_at
         FROM sender_identities
         WHERE account_id=?1 AND (is_primary=1 OR verification_status='accepted')
         ORDER BY is_provider_default DESC,is_primary DESC,LOWER(email) LIMIT 1",
    )?;
    statement
        .query_row(params![account.id], from_row)
        .optional()?
        .ok_or_else(|| CoreError::Other("account has no authorized sender identity".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repo::accounts::{self, NewAccount};
    use crate::models::{AuthKind, MailProtocol, Provider};

    #[test]
    fn only_verified_provider_identities_resolve() {
        let mut conn = crate::db::testutil::conn();
        let account_id = accounts::insert(
            &conn,
            &NewAccount {
                email: "login@example.test",
                display_name: Some("Login"),
                avatar_url: None,
                provider: Provider::Gmail,
                auth_kind: AuthKind::Oauth2,
                mail_protocol: MailProtocol::Imap,
                username: "login@example.test",
                jmap_url: "",
                jmap_account_id: None,
                imap_host: "",
                imap_port: 993,
                smtp_host: "",
                smtp_port: 465,
            },
        )
        .unwrap();
        let synced_at = now_ms();
        replace(
            &mut conn,
            account_id,
            &[
                SenderIdentity {
                    account_id,
                    email: "login@example.test".into(),
                    display_name: Some("Login".into()),
                    reply_to_email: None,
                    is_primary: true,
                    is_provider_default: false,
                    verification_status: "accepted".into(),
                    last_synced_at: synced_at,
                },
                SenderIdentity {
                    account_id,
                    email: "work@example.test".into(),
                    display_name: Some("Work".into()),
                    reply_to_email: None,
                    is_primary: false,
                    is_provider_default: true,
                    verification_status: "accepted".into(),
                    last_synced_at: synced_at,
                },
                SenderIdentity {
                    account_id,
                    email: "pending@example.test".into(),
                    display_name: None,
                    reply_to_email: None,
                    is_primary: false,
                    is_provider_default: false,
                    verification_status: "pending".into(),
                    last_synced_at: synced_at,
                },
            ],
        )
        .unwrap();

        assert!(
            get_verified(&conn, account_id, "WORK@example.test")
                .unwrap()
                .is_some()
        );
        assert!(
            get_verified(&conn, account_id, "pending@example.test")
                .unwrap()
                .is_none()
        );
        let account = accounts::get_config(&conn, account_id).unwrap().unwrap();
        assert_eq!(
            resolve_default(&conn, &account).unwrap().email,
            "work@example.test"
        );
    }
}
