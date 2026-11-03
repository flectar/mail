use crate::error::{CoreError, Result};
use crate::models::{
    AccountConfig, SenderDeliveryStatus, SenderIdentity, SenderIdentitySource, now_ms,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;

const COLUMNS: &str = "account_id,email,display_name,reply_to_email,is_primary,
    is_provider_default,verification_status,last_synced_at,source,delivery_status,
    delivery_error";

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
        source: SenderIdentitySource::parse(&row.get::<_, String>("source")?),
        delivery_status: SenderDeliveryStatus::parse(&row.get::<_, String>("delivery_status")?),
        delivery_error: row.get("delivery_error")?,
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
           is_primary=1,verification_status='accepted',source='provider'",
        params![account.id, account.email, account.display_name, now_ms()],
    )?;
    Ok(())
}

pub fn list(conn: &Connection, account_id: i64) -> Result<Vec<SenderIdentity>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM sender_identities WHERE account_id=?1
         ORDER BY is_provider_default DESC,is_primary DESC,LOWER(email)"
    ))?;
    Ok(statement
        .query_map(params![account_id], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Atomically replace a provider's discovered identities. The caller only
/// supplies values parsed from an authenticated provider endpoint. Manual
/// identities survive unless the provider now lists the same address, and
/// learned delivery outcomes are kept for addresses that remain.
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
    let current = identities
        .iter()
        .map(|identity| identity.email.to_lowercase())
        .collect::<HashSet<_>>();
    let stale = list(&tx, account_id)?
        .into_iter()
        .filter(|identity| {
            identity.source == SenderIdentitySource::Provider
                && !current.contains(&identity.email.to_lowercase())
        })
        .map(|identity| identity.email)
        .collect::<Vec<_>>();
    for email in stale {
        tx.execute(
            "DELETE FROM sender_identities WHERE account_id=?1 AND email=?2",
            params![account_id, email],
        )?;
    }
    for identity in identities {
        tx.execute(
            "INSERT INTO sender_identities (
               account_id,email,display_name,reply_to_email,is_primary,
               is_provider_default,verification_status,last_synced_at,source
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'provider')
             ON CONFLICT(account_id,email) DO UPDATE SET
               email=excluded.email,display_name=excluded.display_name,
               reply_to_email=excluded.reply_to_email,is_primary=excluded.is_primary,
               is_provider_default=excluded.is_provider_default,
               verification_status=excluded.verification_status,
               last_synced_at=excluded.last_synced_at,source='provider'",
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

/// Return the identity that authorizes `email` as a From address. Exact
/// addresses win; otherwise a verified `*@domain` identity yields a concrete
/// identity carrying the requested address and the wildcard's settings.
pub fn get_verified(
    conn: &Connection,
    account_id: i64,
    email: &str,
) -> Result<Option<SenderIdentity>> {
    let email = email.trim();
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM sender_identities
         WHERE account_id=?1 AND email=?2 COLLATE NOCASE
           AND (is_primary=1 OR verification_status='accepted')"
    ))?;
    if let Some(identity) = statement
        .query_row(params![account_id, email], from_row)
        .optional()?
        .filter(|identity| !identity.is_wildcard())
    {
        return Ok(Some(identity));
    }
    Ok(list(conn, account_id)?
        .into_iter()
        .find(|identity| identity.is_verified() && identity.authorizes(email))
        .map(|wildcard| SenderIdentity {
            email: email.to_owned(),
            is_primary: false,
            is_provider_default: false,
            delivery_status: SenderDeliveryStatus::Unknown,
            delivery_error: None,
            ..wildcard
        }))
}

pub fn resolve_default(conn: &Connection, account: &AccountConfig) -> Result<SenderIdentity> {
    ensure_primary(conn, account)?;
    if let Some(email) = account.settings.default_sender_email.as_deref()
        && let Some(identity) = get_verified(conn, account.id, email)?
    {
        return Ok(identity);
    }
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM sender_identities
         WHERE account_id=?1 AND (is_primary=1 OR verification_status='accepted')
           AND email NOT LIKE '*@%'
         ORDER BY is_provider_default DESC,is_primary DESC,LOWER(email) LIMIT 1"
    ))?;
    statement
        .query_row(params![account.id], from_row)
        .optional()?
        .ok_or_else(|| CoreError::Other("account has no authorized sender identity".into()))
}

/// Insert or edit a user-declared identity. Provider-owned rows are never
/// overwritten: their settings belong to the provider.
pub fn save_manual(conn: &Connection, identity: &SenderIdentity) -> Result<()> {
    let existing = conn
        .query_row(
            "SELECT source FROM sender_identities WHERE account_id=?1 AND email=?2",
            params![identity.account_id, identity.email],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if existing
        .is_some_and(|source| SenderIdentitySource::parse(&source) != SenderIdentitySource::Manual)
    {
        return Err(CoreError::Other(format!(
            "{} is already provided by this account",
            identity.email
        )));
    }
    conn.execute(
        "INSERT INTO sender_identities (
           account_id,email,display_name,reply_to_email,is_primary,
           is_provider_default,verification_status,last_synced_at,source
         ) VALUES (?1,?2,?3,?4,0,0,'accepted',?5,'manual')
         ON CONFLICT(account_id,email) DO UPDATE SET
           email=excluded.email,display_name=excluded.display_name,
           reply_to_email=excluded.reply_to_email,last_synced_at=excluded.last_synced_at",
        params![
            identity.account_id,
            identity.email,
            identity.display_name,
            identity.reply_to_email,
            now_ms(),
        ],
    )?;
    Ok(())
}

/// Delete a user-declared identity. Returns false when no manual row matched.
pub fn remove_manual(conn: &Connection, account_id: i64, email: &str) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM sender_identities
         WHERE account_id=?1 AND email=?2 COLLATE NOCASE AND source='manual'",
        params![account_id, email.trim()],
    )? > 0)
}

/// Record what the server did with the most recent submission from `email`.
pub fn record_delivery(
    conn: &Connection,
    account_id: i64,
    email: &str,
    status: SenderDeliveryStatus,
    error: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE sender_identities SET delivery_status=?3,delivery_error=?4
         WHERE account_id=?1 AND email=?2 COLLATE NOCASE",
        params![account_id, email.trim(), status.as_str(), error],
    )?;
    Ok(())
}

/// Every address that belongs to the account, for telling outgoing mail and
/// the user's own addresses apart from correspondents.
#[derive(Debug, Clone, Default)]
pub struct OwnAddresses {
    exact: HashSet<String>,
    domains: HashSet<String>,
}

impl OwnAddresses {
    pub fn insert(&mut self, email: String) {
        let email = email.trim().to_ascii_lowercase();
        match email.split_once('@') {
            Some(("*", domain)) => {
                self.domains.insert(domain.to_owned());
            }
            _ => {
                self.exact.insert(email);
            }
        }
    }

    pub fn contains(&self, email: &str) -> bool {
        let email = email.trim();
        let lowered;
        let email = if email.bytes().any(|byte| byte.is_ascii_uppercase()) {
            lowered = email.to_ascii_lowercase();
            lowered.as_str()
        } else {
            email
        };
        self.exact.contains(email)
            || email
                .rsplit_once('@')
                .is_some_and(|(local, domain)| !local.is_empty() && self.domains.contains(domain))
    }
}

pub fn own_addresses(conn: &Connection, account_id: i64) -> Result<OwnAddresses> {
    let mut own = OwnAddresses::default();
    for identity in list(conn, account_id)? {
        if identity.is_verified() {
            own.insert(identity.email);
        }
    }
    Ok(own)
}

/// The addresses of every account, for mail that may have been sent from
/// any of them.
pub fn own_addresses_of_all(conn: &Connection) -> Result<OwnAddresses> {
    let mut own = OwnAddresses::default();
    let mut statement = conn.prepare(
        "SELECT email FROM sender_identities
         WHERE is_primary = 1 OR verification_status = 'accepted'",
    )?;
    for email in statement.query_map([], |row| row.get::<_, String>(0))? {
        own.insert(email?);
    }
    Ok(own)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repo::accounts::{self, NewAccount};
    use crate::models::{AuthKind, MailProtocol, Provider};

    fn account(conn: &Connection, provider: Provider) -> i64 {
        let (auth_kind, mail_protocol) = match provider {
            Provider::Gmail => (AuthKind::Oauth2, MailProtocol::Gmail),
            Provider::Microsoft => (AuthKind::Oauth2, MailProtocol::Imap),
            Provider::Generic => (AuthKind::Password, MailProtocol::Imap),
        };
        accounts::insert(
            conn,
            &NewAccount {
                email: "login@example.test",
                display_name: Some("Login"),
                avatar_url: None,
                provider,
                auth_kind,
                mail_protocol,
                username: "login@example.test",
                jmap_url: "",
                jmap_account_id: None,
                incoming_host: "",
                incoming_port: 993,
                smtp_host: "",
                smtp_port: 465,
            },
            &crate::db::testutil::account_settings(),
        )
        .unwrap()
    }

    fn identity(account_id: i64, email: &str, primary: bool, status: &str) -> SenderIdentity {
        SenderIdentity {
            account_id,
            email: email.into(),
            display_name: None,
            reply_to_email: None,
            is_primary: primary,
            is_provider_default: primary,
            verification_status: status.into(),
            last_synced_at: now_ms(),
            source: SenderIdentitySource::Provider,
            delivery_status: SenderDeliveryStatus::Unknown,
            delivery_error: None,
        }
    }

    #[test]
    fn only_verified_provider_identities_resolve() {
        let mut conn = crate::db::testutil::conn();
        let account_id = account(&conn, Provider::Gmail);
        let mut login = identity(account_id, "login@example.test", true, "accepted");
        login.is_provider_default = false;
        let mut work = identity(account_id, "work@example.test", false, "accepted");
        work.is_provider_default = true;
        replace(
            &mut conn,
            account_id,
            &[
                login,
                work,
                identity(account_id, "pending@example.test", false, "pending"),
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

    #[test]
    fn provider_refresh_keeps_manual_identities_and_learned_delivery() {
        let mut conn = crate::db::testutil::conn();
        let account_id = account(&conn, Provider::Generic);
        let mut alias = identity(account_id, "alias@example.test", false, "accepted");
        alias.display_name = Some("Alias".into());
        alias.reply_to_email = Some("desk@example.test".into());
        save_manual(&conn, &alias).unwrap();
        record_delivery(
            &conn,
            account_id,
            "ALIAS@example.test",
            SenderDeliveryStatus::Rejected,
            Some("553 5.7.1 not owned by user"),
        )
        .unwrap();
        record_delivery(
            &conn,
            account_id,
            "login@example.test",
            SenderDeliveryStatus::Confirmed,
            None,
        )
        .unwrap();

        replace(
            &mut conn,
            account_id,
            &[identity(account_id, "login@example.test", true, "accepted")],
        )
        .unwrap();

        let identities = list(&conn, account_id).unwrap();
        assert_eq!(identities.len(), 2);
        let alias = identities
            .iter()
            .find(|identity| identity.email == "alias@example.test")
            .unwrap();
        assert_eq!(alias.source, SenderIdentitySource::Manual);
        assert_eq!(alias.delivery_status, SenderDeliveryStatus::Rejected);
        assert_eq!(alias.reply_to_email.as_deref(), Some("desk@example.test"));
        let login = identities
            .iter()
            .find(|identity| identity.is_primary)
            .unwrap();
        assert_eq!(login.delivery_status, SenderDeliveryStatus::Confirmed);
        assert!(
            get_verified(&conn, account_id, "alias@example.test")
                .unwrap()
                .is_some()
        );

        // A provider that starts listing the address takes ownership of it.
        replace(
            &mut conn,
            account_id,
            &[
                identity(account_id, "login@example.test", true, "accepted"),
                identity(account_id, "alias@example.test", false, "accepted"),
            ],
        )
        .unwrap();
        assert!(!remove_manual(&conn, account_id, "alias@example.test").unwrap());
        assert!(
            save_manual(
                &conn,
                &identity(account_id, "alias@example.test", false, "accepted")
            )
            .is_err()
        );
    }

    #[test]
    fn wildcard_identities_authorize_concrete_addresses_only() {
        let mut conn = crate::db::testutil::conn();
        let account_id = account(&conn, Provider::Generic);
        let mut wildcard = identity(account_id, "*@team.test", false, "accepted");
        wildcard.display_name = Some("Team".into());
        wildcard.reply_to_email = Some("help@team.test".into());
        replace(
            &mut conn,
            account_id,
            &[
                identity(account_id, "login@example.test", true, "accepted"),
                wildcard,
            ],
        )
        .unwrap();

        let resolved = get_verified(&conn, account_id, "sales@TEAM.test")
            .unwrap()
            .unwrap();
        assert_eq!(resolved.email, "sales@TEAM.test");
        assert_eq!(resolved.display_name.as_deref(), Some("Team"));
        assert_eq!(resolved.reply_to_email.as_deref(), Some("help@team.test"));
        assert!(!resolved.is_wildcard());
        assert!(
            get_verified(&conn, account_id, "*@team.test")
                .unwrap()
                .is_none()
        );
        assert!(
            get_verified(&conn, account_id, "sales@other.test")
                .unwrap()
                .is_none()
        );

        let account = accounts::get_config(&conn, account_id).unwrap().unwrap();
        assert_eq!(
            resolve_default(&conn, &account).unwrap().email,
            "login@example.test"
        );

        let own = own_addresses(&conn, account_id).unwrap();
        assert!(own.contains("Login@Example.test"));
        assert!(own.contains("anyone@team.test"));
        assert!(!own.contains("team.test"));
        assert!(!own.contains("friend@example.test"));
    }
}
