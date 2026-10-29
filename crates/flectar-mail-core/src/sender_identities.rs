//! Provider-neutral sender identities.
//!
//! Authentication ownership (`accounts.email`) is intentionally immutable
//! here. This module only controls the RFC 5322 From identity used for drafts
//! and submissions.
//!
//! Identities come from the most authoritative source each provider offers:
//! Gmail SendAs and JMAP Identity/get (RFC 8621 §6) list exactly what the
//! server will accept. Plain SMTP and Microsoft have no such endpoint (Graph
//! cannot report Send As permissions), so the user declares addresses
//! manually and Flectar learns from the server's response to real
//! submissions, as Thunderbird, Apple Mail and K-9 do.

use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::models::{
    AccountConfig, Address, MailProtocol, Provider, SenderDeliveryStatus, SenderIdentity,
    SenderIdentitySource, now_ms,
};
use crate::{Core, http_body};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;

const GMAIL_SEND_AS_URL: &str = "https://gmail.googleapis.com/gmail/v1/users/me/settings/sendAs";
pub(crate) const MAX_IDENTITIES: usize = 256;
pub(crate) const MAX_DISPLAY_NAME_CHARS: usize = 256;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GmailSendAsResponse {
    #[serde(default)]
    send_as: Vec<GmailSendAs>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GmailSendAs {
    send_as_email: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    reply_to_address: Option<String>,
    #[serde(default)]
    is_primary: bool,
    #[serde(default)]
    is_default: bool,
    #[serde(default)]
    verification_status: Option<String>,
}

impl Core {
    /// Return cached identities, always including the authenticated primary
    /// address even before the first provider refresh.
    pub async fn list_sender_identities(&self, account_id: i64) -> Result<Vec<SenderIdentity>> {
        self.db
            .write(move |conn| {
                let account = repo::accounts::get_config(conn, account_id)?
                    .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
                repo::sender_identities::ensure_primary(conn, &account)?;
                repo::sender_identities::list(conn, account_id)
            })
            .await
    }

    pub async fn default_sender_identity(&self, account_id: i64) -> Result<SenderIdentity> {
        self.resolve_sender_identity(account_id, None).await
    }

    /// Refresh identities using the account provider and return every saved
    /// identity, including manual addresses the provider does not list.
    /// Providers without a discoverable identity endpoint expose their
    /// authenticated address only.
    pub async fn refresh_sender_identities(&self, account_id: i64) -> Result<Vec<SenderIdentity>> {
        let account = self
            .db
            .read(move |conn| repo::accounts::get_config(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
        let identities = match account.mail_protocol {
            MailProtocol::Gmail => self.gmail_sender_identities(&account).await?,
            MailProtocol::Jmap => {
                let secret = crate::accounts::credentials::load_async(
                    self.credentials.clone(),
                    account.id,
                    crate::accounts::credentials::Slot::Password,
                )
                .await?;
                let connected = crate::jmap::client::connect(&account, &secret).await?;
                jmap_sender_identities(&account, &connected).await?
            }
            // SMTP servers publish no identity list.
            MailProtocol::Imap | MailProtocol::Pop3 => vec![primary_identity(&account)],
            // It has no address to send from.
            MailProtocol::Local => Vec::new(),
        };
        self.db
            .write(move |conn| {
                repo::sender_identities::replace(conn, account_id, &identities)?;
                repo::sender_identities::list(conn, account_id)
            })
            .await
    }

    /// Resolve and validate a requested From address against provider-derived
    /// state. Passing `None` uses the app default, then provider/primary default.
    pub(crate) async fn resolve_sender_identity(
        &self,
        account_id: i64,
        requested_email: Option<&str>,
    ) -> Result<SenderIdentity> {
        let requested = requested_email.map(str::trim).map(str::to_owned);
        self.db
            .write(move |conn| {
                let account = repo::accounts::get_config(conn, account_id)?
                    .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
                repo::sender_identities::ensure_primary(conn, &account)?;
                match requested.as_deref().filter(|value| !value.is_empty()) {
                    Some(email) => repo::sender_identities::get_verified(conn, account_id, email)?
                        .ok_or_else(|| {
                            CoreError::Other(format!(
                                "{email} is not a verified sender identity for this account"
                            ))
                        }),
                    None => repo::sender_identities::resolve_default(conn, &account),
                }
            })
            .await
    }

    /// Change Flectar's default for new messages without altering Gmail's own
    /// default or the account's authentication identity.
    pub async fn set_default_sender_identity(&self, account_id: i64, email: String) -> Result<()> {
        let email = email.trim().to_owned();
        self.db
            .write(move |conn| {
                let mut account = repo::accounts::get_config(conn, account_id)?
                    .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
                repo::sender_identities::ensure_primary(conn, &account)?;
                repo::sender_identities::get_verified(conn, account_id, &email)?.ok_or_else(
                    || {
                        CoreError::Other(format!(
                            "{email} is not a verified sender identity for this account"
                        ))
                    },
                )?;
                account.settings.default_sender_email = Some(email);
                repo::accounts::set_settings(conn, account_id, &account.settings)
            })
            .await
    }

    /// Add or edit an address the user may send from. Only accounts whose
    /// server cannot enumerate permitted senders accept manual entries; for
    /// Gmail and JMAP the provider is authoritative and would rewrite or
    /// reject an unlisted From, so the address must be added there instead.
    /// JMAP wildcard identities (`*@domain`) are the exception: they already
    /// authorize every address at the domain.
    pub async fn save_sender_identity(
        &self,
        account_id: i64,
        email: String,
        display_name: Option<String>,
        reply_to_email: Option<String>,
    ) -> Result<SenderIdentity> {
        let (email, display_name, reply_to_email) =
            manual_identity_fields(email, display_name, reply_to_email)?;
        self.db
            .write(move |conn| {
                let account = repo::accounts::get_config(conn, account_id)?
                    .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
                repo::sender_identities::ensure_primary(conn, &account)?;
                let identities = repo::sender_identities::list(conn, account_id)?;
                let existing = identities
                    .iter()
                    .find(|identity| identity.email.eq_ignore_ascii_case(&email));
                if existing.is_some_and(|identity| identity.source == SenderIdentitySource::Provider)
                {
                    return Err(CoreError::Other(format!(
                        "{email} is already a sender identity provided by this account"
                    )));
                }
                match manual_identity_policy(&account) {
                    ManualIdentityPolicy::Any => {}
                    ManualIdentityPolicy::WithinWildcard => {
                        if !identities.iter().any(|identity| {
                            identity.is_wildcard()
                                && identity.is_verified()
                                && identity.authorizes(&email)
                        }) {
                            return Err(CoreError::Other(format!(
                                "the JMAP server does not allow sending as {email}; add the address on the server, then refresh sender identities"
                            )));
                        }
                    }
                    ManualIdentityPolicy::Never => {
                        return Err(CoreError::Other(format!(
                            "Gmail controls which addresses this account may send from; add {email} in Gmail settings under Accounts › Send mail as, then refresh sender identities"
                        )));
                    }
                }
                if existing.is_none() && identities.len() >= MAX_IDENTITIES {
                    return Err(CoreError::Other(format!(
                        "an account can have at most {MAX_IDENTITIES} sender identities"
                    )));
                }
                let identity = SenderIdentity {
                    account_id,
                    email,
                    display_name,
                    reply_to_email,
                    is_primary: false,
                    is_provider_default: false,
                    verification_status: "accepted".into(),
                    last_synced_at: now_ms(),
                    source: SenderIdentitySource::Manual,
                    delivery_status: existing.map_or_else(Default::default, |identity| {
                        identity.delivery_status
                    }),
                    delivery_error: existing.and_then(|identity| identity.delivery_error.clone()),
                };
                repo::sender_identities::save_manual(conn, &identity)?;
                Ok(identity)
            })
            .await
    }

    /// Remove a manually added address. Provider identities can only change
    /// at the provider. A matching Flectar default falls back to the
    /// provider default.
    pub async fn remove_sender_identity(&self, account_id: i64, email: String) -> Result<()> {
        let email = email.trim().to_owned();
        self.db
            .write(move |conn| {
                let mut account = repo::accounts::get_config(conn, account_id)?
                    .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
                if !repo::sender_identities::remove_manual(conn, account_id, &email)? {
                    return Err(CoreError::Other(format!(
                        "{email} is managed by the provider and cannot be removed here"
                    )));
                }
                if account
                    .settings
                    .default_sender_email
                    .as_deref()
                    .is_some_and(|default| default.eq_ignore_ascii_case(&email))
                {
                    account.settings.default_sender_email = None;
                    repo::accounts::set_settings(conn, account_id, &account.settings)?;
                }
                Ok(())
            })
            .await
    }

    async fn gmail_sender_identities(
        &self,
        account: &AccountConfig,
    ) -> Result<Vec<SenderIdentity>> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(45))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("Flectar-Mail/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| CoreError::Network(error.to_string()))?;
        for attempt in 0..2 {
            let token = self
                .tokens
                .access_token(account.id, Provider::Gmail)
                .await?;
            let response = http
                .get(GMAIL_SEND_AS_URL)
                .bearer_auth(token)
                .send()
                .await
                .map_err(|error| {
                    if error.is_timeout() || error.is_connect() {
                        CoreError::Offline
                    } else {
                        CoreError::Network(error.to_string())
                    }
                })?;
            let status = response.status();
            let body =
                http_body::text(response, MAX_RESPONSE_BYTES, "Gmail SendAs response").await?;
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                if attempt == 0 {
                    self.tokens.invalidate(account.id).await;
                    continue;
                }
                return Err(CoreError::NeedsReauth);
            }
            if !status.is_success() {
                let message = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|value| {
                        value
                            .pointer("/error/message")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| {
                        status
                            .canonical_reason()
                            .unwrap_or("Gmail SendAs request failed")
                            .to_owned()
                    });
                return Err(
                    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
                        CoreError::Network(format!("Gmail {status}: {message}"))
                    } else {
                        CoreError::Other(format!("Gmail {status}: {message}"))
                    },
                );
            }
            let response: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
                CoreError::Network(format!("invalid Gmail SendAs JSON: {error}"))
            })?;
            return parse_gmail_identities_value(account, response);
        }
        Err(CoreError::NeedsReauth)
    }
}

fn primary_identity(account: &AccountConfig) -> SenderIdentity {
    SenderIdentity {
        account_id: account.id,
        email: account.email.clone(),
        display_name: account.display_name.clone(),
        reply_to_email: None,
        is_primary: true,
        is_provider_default: true,
        verification_status: "accepted".into(),
        last_synced_at: now_ms(),
        source: SenderIdentitySource::Provider,
        delivery_status: SenderDeliveryStatus::Unknown,
        delivery_error: None,
    }
}

/// Which manually entered addresses an account may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualIdentityPolicy {
    /// Plain SMTP / Microsoft: the server is the only judge, at send time.
    Any,
    /// JMAP: only addresses covered by a server wildcard identity.
    WithinWildcard,
    /// Gmail: SendAs is authoritative; unlisted addresses are rewritten.
    Never,
}

/// Checks and cleans the fields of a user-declared sender identity. Wildcard
/// addresses are provider grants only; a manual one would claim a domain.
pub(crate) fn manual_identity_fields(
    email: String,
    display_name: Option<String>,
    reply_to_email: Option<String>,
) -> Result<(String, Option<String>, Option<String>)> {
    let email = email.trim().to_owned();
    if !valid_email(&email) || email.starts_with("*@") {
        return Err(CoreError::Other(format!(
            "{email} is not a valid email address"
        )));
    }
    let display_name = clean_optional(display_name);
    if display_name
        .as_deref()
        .is_some_and(|name| name.chars().count() > MAX_DISPLAY_NAME_CHARS)
    {
        return Err(CoreError::Other("the sender name is too long".into()));
    }
    let reply_to_email = clean_optional(reply_to_email);
    if let Some(reply_to) = reply_to_email.as_deref()
        && (!valid_email(reply_to) || reply_to.starts_with("*@"))
    {
        return Err(CoreError::Other(format!(
            "{reply_to} is not a valid reply-to address"
        )));
    }
    Ok((email, display_name, reply_to_email))
}

pub fn manual_identity_policy(account: &AccountConfig) -> ManualIdentityPolicy {
    manual_identity_policy_for(account.mail_protocol)
}

pub(crate) fn manual_identity_policy_for(protocol: MailProtocol) -> ManualIdentityPolicy {
    match protocol {
        MailProtocol::Gmail | MailProtocol::Local => ManualIdentityPolicy::Never,
        MailProtocol::Jmap => ManualIdentityPolicy::WithinWildcard,
        MailProtocol::Imap | MailProtocol::Pop3 => ManualIdentityPolicy::Any,
    }
}

pub(crate) async fn jmap_sender_identities(
    account: &AccountConfig,
    connected: &crate::jmap::client::ConnectedClient,
) -> Result<Vec<SenderIdentity>> {
    let mut request = connected.request();
    request.get_identity().account_id(&connected.account_id);
    let identities = connected
        .send_single::<jmap_client::core::response::IdentityGetResponse>(request)
        .await
        .map_err(crate::jmap::client::map_error)?
        .take_list();
    parse_jmap_identities(
        account,
        identities.iter().map(|identity| JmapIdentity {
            name: identity.name(),
            email: identity.email(),
            reply_to: identity
                .reply_to()
                .and_then(|addresses| addresses.first())
                .map(|address| address.email()),
        }),
    )
}

struct JmapIdentity<'a> {
    name: Option<&'a str>,
    email: Option<&'a str>,
    reply_to: Option<&'a str>,
}

/// Project RFC 8621 Identity objects. JMAP permits several identities with
/// the same address and different names; the first one wins locally. The
/// authenticated address stays primary even if the server omits it, matching
/// `ensure_primary`, and is Flectar's provider default because JMAP has no
/// default-identity concept.
fn parse_jmap_identities<'a>(
    account: &AccountConfig,
    identities: impl IntoIterator<Item = JmapIdentity<'a>>,
) -> Result<Vec<SenderIdentity>> {
    let synced_at = now_ms();
    let mut seen = HashSet::new();
    let mut parsed = Vec::new();
    for identity in identities {
        let Some(email) = identity
            .email
            .map(str::trim)
            .filter(|email| valid_email(email))
        else {
            return Err(CoreError::Network(
                "JMAP returned an identity without a valid email address".into(),
            ));
        };
        if !seen.insert(email.to_ascii_lowercase()) {
            continue;
        }
        if parsed.len() == MAX_IDENTITIES {
            return Err(CoreError::Network(format!(
                "JMAP returned more than {MAX_IDENTITIES} sender identities"
            )));
        }
        let is_primary = email.eq_ignore_ascii_case(&account.email);
        parsed.push(SenderIdentity {
            account_id: account.id,
            email: email.to_owned(),
            display_name: clean_optional(identity.name.map(str::to_owned)),
            reply_to_email: identity
                .reply_to
                .map(str::trim)
                .filter(|email| valid_email(email) && !email.starts_with("*@"))
                .map(str::to_owned),
            is_primary,
            is_provider_default: is_primary,
            verification_status: "accepted".into(),
            last_synced_at: synced_at,
            source: SenderIdentitySource::Provider,
            delivery_status: SenderDeliveryStatus::Unknown,
            delivery_error: None,
        });
    }
    if !parsed.iter().any(|identity| identity.is_primary) {
        parsed.push(primary_identity(account));
    }
    Ok(parsed)
}

/// Choose the From address for a reply or forward of a received message.
///
/// Mirrors the consensus of Thunderbird, K-9 and Fastmail: exact matches in
/// To, then Cc, then the original author (replying to your own sent mail);
/// then wildcard identities, using the concrete address the message was sent
/// to; then RFC 5233 subaddresses (`user+tag@`), answered from the base
/// identity because many servers refuse a plus-address in From.
pub fn reply_sender<'a>(
    identities: &[SenderIdentity],
    recipients: impl IntoIterator<Item = &'a Address> + Clone,
) -> Option<String> {
    let selectable = || {
        identities
            .iter()
            .filter(|identity| identity.is_selectable())
    };
    let exact = recipients.clone().into_iter().find_map(|address| {
        selectable()
            .find(|identity| identity.email.eq_ignore_ascii_case(address.email.trim()))
            .map(|identity| identity.email.clone())
    });
    exact
        .or_else(|| {
            recipients.clone().into_iter().find_map(|address| {
                identities
                    .iter()
                    .any(|identity| {
                        identity.is_verified()
                            && identity.is_wildcard()
                            && identity.authorizes(&address.email)
                    })
                    .then(|| address.email.trim().to_owned())
            })
        })
        .or_else(|| {
            recipients.into_iter().find_map(|address| {
                let base = without_subaddress(&address.email)?;
                selectable()
                    .find(|identity| identity.email.eq_ignore_ascii_case(&base))
                    .map(|identity| identity.email.clone())
            })
        })
}

/// Whether `email` is one of the account's own addresses, including wildcard
/// domains and subaddresses of an identity. Used to keep the user out of
/// reply-all recipient lists.
pub fn is_own_address(identities: &[SenderIdentity], email: &str) -> bool {
    let email = email.trim();
    let base = without_subaddress(email);
    identities
        .iter()
        .filter(|identity| identity.is_verified())
        .any(|identity| {
            identity.authorizes(email)
                || base
                    .as_deref()
                    .is_some_and(|base| identity.authorizes(base))
        })
}

fn without_subaddress(email: &str) -> Option<String> {
    let (local, domain) = email.trim().rsplit_once('@')?;
    let (base, detail) = local.split_once('+')?;
    (!base.is_empty() && !detail.is_empty()).then(|| format!("{base}@{domain}"))
}

/// Recognize a submission server refusing the From/envelope sender, as
/// opposed to a recipient or content problem. Codes and phrases come from
/// RFC 6409 §6.1 servers in the wild: Postfix `reject_sender_login_mismatch`
/// (553 5.7.1 … not owned by user), Exchange Online (550 5.7.60, 554 5.2.252
/// SendAsDenied), Dovecot/Exim/Stalwart variants, and Gmail SendAs denial.
pub fn is_sender_rejection(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "sendasdenied",
        "5.7.60",
        "5.2.252",
        "not owned by user",
        "sender address rejected",
        "send as this sender",
        "not allowed to send as",
        "not authorized to send as",
        "not permitted to send as",
        "sender not authorized",
        "sender is not authorized",
        "unauthorized sender",
        "sender address not allowed",
        "invalid sender",
        "forbiddenfrom",
        "forbiddenmailfrom",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn parse_gmail_identities(
    account: &AccountConfig,
    aliases: Vec<GmailSendAs>,
) -> Result<Vec<SenderIdentity>> {
    if aliases.len() > MAX_IDENTITIES {
        return Err(CoreError::Network(format!(
            "Gmail returned more than {MAX_IDENTITIES} sender identities"
        )));
    }
    let synced_at = now_ms();
    let mut seen = HashSet::new();
    let mut identities = Vec::with_capacity(aliases.len());
    for alias in aliases {
        let email = trim_owned(alias.send_as_email);
        if !valid_email(&email) || !seen.insert(email.to_ascii_lowercase()) {
            return Err(CoreError::Network(
                "Gmail returned an invalid or duplicate sender identity".into(),
            ));
        }
        let verification_status = if alias.is_primary {
            "accepted"
        } else {
            match alias.verification_status.as_deref() {
                Some("accepted") => "accepted",
                Some("pending") | None | Some("verificationStatusUnspecified") => "pending",
                Some(_) => {
                    return Err(CoreError::Network(
                        "Gmail returned an unknown sender verification status".into(),
                    ));
                }
            }
        };
        identities.push(SenderIdentity {
            account_id: account.id,
            email,
            display_name: clean_optional(alias.display_name),
            reply_to_email: clean_optional(alias.reply_to_address),
            is_primary: alias.is_primary,
            is_provider_default: alias.is_default,
            verification_status: verification_status.into(),
            last_synced_at: synced_at,
            source: SenderIdentitySource::Provider,
            delivery_status: SenderDeliveryStatus::Unknown,
            delivery_error: None,
        });
    }
    let primary = identities
        .iter()
        .find(|identity| identity.is_primary)
        .ok_or_else(|| {
            CoreError::Network("Gmail SendAs response omitted the primary address".into())
        })?;
    if !primary.email.eq_ignore_ascii_case(&account.email) {
        return Err(CoreError::Network(
            "Gmail SendAs primary address does not match the authenticated account".into(),
        ));
    }
    if !identities
        .iter()
        .any(|identity| identity.is_provider_default)
    {
        return Err(CoreError::Network(
            "Gmail SendAs response omitted the default address".into(),
        ));
    }
    Ok(identities)
}

pub(crate) fn parse_gmail_identities_value(
    account: &AccountConfig,
    value: serde_json::Value,
) -> Result<Vec<SenderIdentity>> {
    let response: GmailSendAsResponse = serde_json::from_value(value)
        .map_err(|error| CoreError::Network(format!("invalid Gmail SendAs JSON: {error}")))?;
    parse_gmail_identities(account, response.send_as)
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(trim_owned)
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
}

fn trim_owned(mut value: String) -> String {
    value.truncate(value.trim_end().len());
    let start = value.len() - value.trim_start().len();
    if start > 0 {
        value.drain(..start);
    }
    // Moving a short retained field must not keep a provider response's large
    // whitespace buffer alive. Ordinary fields keep their existing allocation.
    if value.capacity() > value.len().saturating_mul(2).max(64) {
        value.shrink_to_fit();
    }
    value
}

fn valid_email(value: &str) -> bool {
    value.len() <= 320
        && !value.chars().any(char::is_control)
        && !value.chars().any(char::is_whitespace)
        && value.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && !domain.is_empty()
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        })
        && value.matches('@').count() == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AuthKind, MailProtocol};
    #[test]
    fn retained_optional_fields_reuse_storage_without_whitespace_capacity() {
        let value = "  Display name  ".to_owned();
        let pointer = value.as_ptr();
        let clean = clean_optional(Some(value)).unwrap();
        assert_eq!(clean, "Display name");
        assert_eq!(clean.as_ptr(), pointer);
        let padded = format!("{}é{}", " ".repeat(100_000), " ".repeat(100_000));
        let clean = clean_optional(Some(padded)).unwrap();
        assert_eq!(clean, "é");
        assert_eq!(clean.capacity(), "é".len());
    }

    fn account() -> AccountConfig {
        AccountConfig {
            id: 7,
            email: "primary@example.com".into(),
            display_name: Some("Primary".into()),
            avatar_url: None,
            provider: Provider::Gmail,
            auth_kind: AuthKind::Oauth2,
            mail_protocol: MailProtocol::Gmail,
            username: "primary@example.com".into(),
            jmap_url: String::new(),
            jmap_account_id: None,
            incoming_host: String::new(),
            incoming_port: 993,
            smtp_host: String::new(),
            smtp_port: 465,
            settings: crate::db::testutil::account_settings(),
        }
    }

    #[test]
    fn gmail_aliases_keep_only_accepted_aliases_selectable() {
        let identities = parse_gmail_identities(
            &account(),
            vec![
                GmailSendAs {
                    send_as_email: "primary@example.com".into(),
                    display_name: Some("Primary".into()),
                    reply_to_address: None,
                    is_primary: true,
                    is_default: false,
                    verification_status: None,
                },
                GmailSendAs {
                    send_as_email: "work@example.net".into(),
                    display_name: Some("Work".into()),
                    reply_to_address: None,
                    is_primary: false,
                    is_default: true,
                    verification_status: Some("accepted".into()),
                },
                GmailSendAs {
                    send_as_email: "waiting@example.net".into(),
                    display_name: None,
                    reply_to_address: None,
                    is_primary: false,
                    is_default: false,
                    verification_status: Some("pending".into()),
                },
            ],
        )
        .unwrap();
        assert_eq!(identities.len(), 3);
        assert!(identities[0].is_verified());
        assert!(identities[1].is_verified());
        assert!(!identities[2].is_verified());
        assert!(identities[1].is_provider_default);
    }

    fn manual(email: &str) -> SenderIdentity {
        SenderIdentity {
            account_id: 7,
            email: email.into(),
            display_name: None,
            reply_to_email: None,
            is_primary: false,
            is_provider_default: false,
            verification_status: "accepted".into(),
            last_synced_at: 0,
            source: SenderIdentitySource::Manual,
            delivery_status: SenderDeliveryStatus::Unknown,
            delivery_error: None,
        }
    }

    fn address(email: &str) -> Address {
        Address {
            name: None,
            email: email.into(),
        }
    }

    #[test]
    fn jmap_identities_keep_primary_wildcards_and_first_duplicate() {
        let identities = parse_jmap_identities(
            &account(),
            [
                JmapIdentity {
                    name: Some(" Sales "),
                    email: Some("sales@example.com"),
                    reply_to: Some("desk@example.com"),
                },
                JmapIdentity {
                    name: Some("Sales (again)"),
                    email: Some("SALES@example.com"),
                    reply_to: None,
                },
                JmapIdentity {
                    name: None,
                    email: Some("*@team.example"),
                    reply_to: None,
                },
            ],
        )
        .unwrap();
        assert_eq!(identities.len(), 3);
        assert_eq!(identities[0].display_name.as_deref(), Some("Sales"));
        assert_eq!(
            identities[0].reply_to_email.as_deref(),
            Some("desk@example.com")
        );
        assert!(identities[1].is_wildcard());
        assert!(!identities[1].is_selectable());
        // The authenticated address is always present and primary.
        assert!(identities[2].is_primary && identities[2].is_provider_default);
        assert_eq!(identities[2].email, "primary@example.com");

        assert!(
            parse_jmap_identities(
                &account(),
                [JmapIdentity {
                    name: None,
                    email: Some("not an address"),
                    reply_to: None,
                }],
            )
            .is_err()
        );
    }

    #[test]
    fn reply_sender_prefers_exact_then_wildcard_then_subaddress() {
        let mut primary = manual("primary@example.com");
        primary.is_primary = true;
        let mut wildcard = manual("*@team.example");
        wildcard.source = SenderIdentitySource::Provider;
        let mut pending = manual("pending@example.com");
        pending.verification_status = "pending".into();
        let identities = vec![primary, manual("alias@example.com"), wildcard, pending];

        let to = [address("friend@else.example"), address("Alias@Example.com")];
        assert_eq!(
            reply_sender(&identities, to.iter()).as_deref(),
            Some("alias@example.com")
        );
        // An exact match anywhere outranks an earlier wildcard match.
        let to = [
            address("sales@team.example"),
            address("primary@example.com"),
        ];
        assert_eq!(
            reply_sender(&identities, to.iter()).as_deref(),
            Some("primary@example.com")
        );
        let to = [address("sales@TEAM.example")];
        assert_eq!(
            reply_sender(&identities, to.iter()).as_deref(),
            Some("sales@TEAM.example")
        );
        let to = [address("alias+newsletter@example.com")];
        assert_eq!(
            reply_sender(&identities, to.iter()).as_deref(),
            Some("alias@example.com")
        );
        let to = [
            address("pending@example.com"),
            address("other@else.example"),
        ];
        assert_eq!(reply_sender(&identities, to.iter()), None);

        assert!(is_own_address(&identities, "ALIAS+x@example.com"));
        assert!(is_own_address(&identities, "anyone@team.example"));
        assert!(!is_own_address(&identities, "pending@example.com"));
        assert!(!is_own_address(&identities, "friend@else.example"));
    }

    #[test]
    fn sender_rejections_are_distinguished_from_other_failures() {
        for message in [
            "553 5.7.1 <alias@example.com>: Sender address rejected: not owned by user login@example.com",
            "550 5.7.60 SMTP; Client does not have permissions to send as this sender",
            "554 5.2.252 SendAsDenied; login@contoso.com not allowed to send as alias@contoso.com",
            "jmap error: forbiddenFrom",
        ] {
            assert!(is_sender_rejection(message), "{message}");
        }
        for message in [
            "550 5.1.1 <nobody@example.com>: Recipient address rejected: User unknown",
            "552 5.3.4 Message size exceeds fixed limit",
            "451 4.3.0 temporary failure",
        ] {
            assert!(!is_sender_rejection(message), "{message}");
        }
    }
}
