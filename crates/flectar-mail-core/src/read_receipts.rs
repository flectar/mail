//! Read receipts this client sends (RFC 8098), once the user or Settings
//! answered a request (see `Core::answer_read_receipt`). Each protocol's
//! action worker records the answer on the server where it keeps one
//! ($MDNSent, RFC 3503) and sends the receipt this module builds.

use crate::accounts::credentials::{self, Slot};
use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::mime::{ReadReceipt, build_read_receipt};
use crate::models::{AccountConfig, Address, AuthKind};
use crate::smtp;
use crate::sync::engine::SyncCtx;
use rusqlite::{OptionalExtension, params};

/// A read receipt ready to send.
pub(crate) struct OutgoingReceipt {
    /// The address the message reached, which sends it.
    pub from: Address,
    pub to: Vec<Address>,
    pub raw: Vec<u8>,
    /// The Gmail thread of the message it answers.
    pub gm_thread: Option<String>,
}

/// The receipt `message_id` asks for, from the account's address it was
/// sent to. None when it asks for none any more.
pub(crate) async fn prepare(
    ctx: &SyncCtx,
    account_id: i64,
    message_id: i64,
    user_agreed: bool,
) -> Result<Option<OutgoingReceipt>> {
    ctx.db
        .read(move |conn| {
            let Some((receipt_to, subject, original_id, to_json, cc_json, gm_thread)) = conn
                .query_row(
                    "SELECT receipt_to, subject, message_id, to_json, cc_json, gm_thrid
                     FROM messages WHERE id = ?1 AND account_id = ?2",
                    params![message_id, account_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, Option<String>>(5)?,
                        ))
                    },
                )
                .optional()?
            else {
                return Ok(None);
            };
            let Some(receipt_to) = receipt_to else {
                return Ok(None);
            };
            let to: Vec<Address> = serde_json::from_str(&receipt_to)?;
            if to.is_empty() {
                return Ok(None);
            }
            let addressed: Vec<Address> = serde_json::from_str::<Vec<Address>>(&to_json)?
                .into_iter()
                .chain(serde_json::from_str::<Vec<Address>>(&cc_json)?)
                .collect();
            let identities = repo::sender_identities::list(conn, account_id)?;
            // The address the message reached is the one that read it; mail
            // that reached it as Bcc or through an alias answers from the
            // account's own address.
            let from = addressed
                .iter()
                .find_map(|address| {
                    identities.iter().find(|identity| {
                        identity.is_verified()
                            && identity.email.eq_ignore_ascii_case(&address.email)
                    })
                })
                .or_else(|| identities.iter().find(|identity| identity.is_primary))
                .map(|identity| Address {
                    name: identity
                        .display_name
                        .clone()
                        .filter(|name| !name.trim().is_empty()),
                    email: identity.email.clone(),
                })
                .ok_or_else(|| CoreError::NotFound("the account's own address".into()))?;
            let domain = from
                .email
                .rsplit_once('@')
                .map(|(_, domain)| domain.to_owned())
                .unwrap_or_else(|| "localhost".into());
            let (_, raw) = build_read_receipt(&ReadReceipt {
                recipient: &from,
                to: &to,
                original_subject: &subject,
                original_message_id: original_id.as_deref(),
                user_agreed,
                reporting_domain: &domain,
            })?;
            Ok(Some(OutgoingReceipt {
                from,
                to,
                raw,
                gm_thread,
            }))
        })
        .await
}

/// Send a receipt through the account's SMTP server. RFC 8098 section 3
/// wants the null envelope sender; a server that refuses it gets the
/// account's address instead, as most clients send them.
pub(crate) async fn send_by_smtp(
    ctx: &SyncCtx,
    config: &AccountConfig,
    receipt: &OutgoingReceipt,
) -> Result<()> {
    let recipients: Vec<String> = receipt.to.iter().map(|to| to.email.clone()).collect();
    let mut auth = smtp_auth(ctx, config).await?;
    for attempt in 0..2 {
        let result = match smtp::send_raw_from(config, &auth, None, &recipients, &receipt.raw).await
        {
            Err(CoreError::Smtp(message)) if crate::queue::has_permanent_smtp_reply(&message) => {
                tracing::info!(
                    account_id = config.id,
                    "the SMTP server refused a read receipt's null sender; sending from the account"
                );
                smtp::send_raw_from(
                    config,
                    &auth,
                    Some(&receipt.from.email),
                    &recipients,
                    &receipt.raw,
                )
                .await
            }
            result => result,
        };
        match result {
            // A rejected token fails before MAIL FROM, so one retry with a
            // fresh one can't deliver the receipt twice.
            Err(CoreError::Auth(_)) if attempt == 0 && config.auth_kind == AuthKind::Oauth2 => {
                ctx.tokens.invalidate(config.id).await;
                auth = smtp_auth(ctx, config).await?;
            }
            result => return result,
        }
    }
    unreachable!("the second attempt returns")
}

async fn smtp_auth(ctx: &SyncCtx, config: &AccountConfig) -> Result<smtp::SmtpAuth> {
    Ok(match config.auth_kind {
        AuthKind::Password => smtp::SmtpAuth::Password(
            credentials::load_async(ctx.credentials.clone(), config.id, Slot::Password).await?,
        ),
        AuthKind::Oauth2 => {
            smtp::SmtpAuth::XOAuth2(ctx.tokens.access_token(config.id, config.provider).await?)
        }
    })
}
