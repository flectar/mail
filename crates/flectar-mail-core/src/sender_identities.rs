//! Provider-neutral sender identities with Gmail SendAs discovery.
//!
//! Authentication ownership (`accounts.email`) is intentionally immutable
//! here. This module only controls the RFC 5322 From identity used for drafts
//! and submissions.

use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::models::{AccountConfig, Provider, SenderIdentity, now_ms};
use crate::{Core, http_body};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;

const GMAIL_SEND_AS_URL: &str = "https://gmail.googleapis.com/gmail/v1/users/me/settings/sendAs";
const MAX_IDENTITIES: usize = 256;
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

    /// Refresh identities using the account provider. Providers without a
    /// discoverable identity endpoint expose their authenticated address only.
    pub async fn refresh_sender_identities(&self, account_id: i64) -> Result<Vec<SenderIdentity>> {
        let account = self
            .db
            .read(move |conn| repo::accounts::get_config(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
        let identities = match account.provider {
            Provider::Gmail => self.gmail_sender_identities(&account).await?,
            Provider::Microsoft | Provider::Imap => vec![primary_identity(&account)],
        };
        let saved = identities.clone();
        self.db
            .write(move |conn| {
                repo::sender_identities::replace(conn, account_id, &saved)?;
                Ok(())
            })
            .await?;
        Ok(identities)
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
    }
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
        let email = alias.send_as_email.trim();
        if !valid_email(email) || !seen.insert(email.to_ascii_lowercase()) {
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
            email: email.to_owned(),
            display_name: clean_optional(alias.display_name),
            reply_to_email: clean_optional(alias.reply_to_address),
            is_primary: alias.is_primary,
            is_provider_default: alias.is_default,
            verification_status: verification_status.into(),
            last_synced_at: synced_at,
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
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
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
    use crate::models::{AccountSettings, AuthKind, MailProtocol};

    fn account() -> AccountConfig {
        AccountConfig {
            id: 7,
            email: "primary@example.com".into(),
            display_name: Some("Primary".into()),
            avatar_url: None,
            provider: Provider::Gmail,
            auth_kind: AuthKind::Oauth2,
            mail_protocol: MailProtocol::Imap,
            username: "primary@example.com".into(),
            jmap_url: String::new(),
            jmap_account_id: None,
            imap_host: String::new(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 465,
            settings: AccountSettings::default(),
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
}
