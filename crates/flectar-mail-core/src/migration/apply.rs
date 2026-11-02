//! Persists reviewed account drafts.

use super::plan::{AccountDraft, SignIn};
use crate::accounts::mailbox_key;
use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::models::{Settings, Signature, SignatureDefaults};
use crate::{Core, sender_identities, signatures};

/// Accounts per import, matching the backup restore limit.
pub const MAX_IMPORTED_ACCOUNTS: usize = 100;

/// What happened to each submitted draft. Every draft appears in exactly one
/// list, so a partial import can never be reported as complete.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountImportReport {
    pub imported: Vec<ImportedAccount>,
    /// Addresses that Flectar already had when the import ran.
    pub already_present: Vec<String>,
    pub failed: Vec<FailedAccount>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedAccount {
    pub account_id: i64,
    pub email: String,
    pub sign_in: SignIn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedAccount {
    pub email: String,
    pub error: String,
}

/// A draft that passed validation, with derived values ready to write.
struct Checked {
    draft: AccountDraft,
    aliases: Vec<(String, Option<String>, Option<String>)>,
    color: Option<String>,
    /// Name, sanitized HTML, and whether it is the account default.
    signatures: Vec<(String, String, bool)>,
}

impl Core {
    /// Creates accounts from reviewed migration drafts.
    ///
    /// Each account is written inside its own savepoint of one transaction:
    /// an invalid draft is reported as failed without leaving a partial
    /// account, and the others still import. Existing accounts are never
    /// changed. New accounts stay paused until the user signs in, because
    /// migrated settings never include credentials.
    pub async fn import_accounts(&self, drafts: Vec<AccountDraft>) -> Result<AccountImportReport> {
        if drafts.len() > MAX_IMPORTED_ACCOUNTS {
            return Err(CoreError::Other(format!(
                "an import can add at most {MAX_IMPORTED_ACCOUNTS} accounts"
            )));
        }
        let mut report = AccountImportReport::default();
        let mut checked = Vec::with_capacity(drafts.len());
        let mut batch = std::collections::HashSet::new();
        for draft in drafts {
            let checked_draft = check(draft).and_then(|draft| {
                let email = draft.draft.config.email.trim();
                if batch.insert(mailbox_key(email)) {
                    Ok(draft)
                } else {
                    Err((
                        email.to_owned(),
                        CoreError::Other("the address appears twice in this import".into()),
                    ))
                }
            });
            match checked_draft {
                Ok(draft) => checked.push(draft),
                Err((email, error)) => report.failed.push(FailedAccount {
                    email,
                    error: error.to_string(),
                }),
            }
        }

        self.db
            .write(move |conn| {
                let mut transaction = conn.transaction()?;
                let mut settings = repo::settings::get(&transaction)?;
                for account in checked {
                    let email = account.draft.config.email.trim().to_owned();
                    if repo::accounts::find_by_mailbox(&transaction, &email)?.is_some() {
                        report.already_present.push(email);
                        continue;
                    }
                    let mut savepoint = transaction.savepoint()?;
                    let mut staged = settings.clone();
                    match insert(&savepoint, &account, &mut staged) {
                        Ok(account_id) => {
                            savepoint.commit()?;
                            settings = staged;
                            report.imported.push(ImportedAccount {
                                account_id,
                                email,
                                sign_in: SignIn::for_provider(account.draft.config.provider),
                            });
                        }
                        Err(error) => {
                            savepoint.rollback()?;
                            report.failed.push(FailedAccount {
                                email,
                                error: error.to_string(),
                            });
                        }
                    }
                }
                if !report.imported.is_empty() {
                    repo::settings::set(&transaction, &settings)?;
                }
                transaction.commit()?;
                Ok(report)
            })
            .await
    }
}

fn check(draft: AccountDraft) -> std::result::Result<Checked, (String, CoreError)> {
    let email = draft.config.email.trim().to_owned();
    let fail = |error: CoreError| (email.clone(), error);
    crate::validate_portable_account(&draft.config).map_err(fail)?;
    if crate::normalize_account_email(&email).is_none() {
        return Err(fail(CoreError::Other(format!(
            "invalid account email address: {email}"
        ))));
    }
    // Drafts get the same rules as identities added in Settings.
    let policy = sender_identities::manual_identity_policy_for(draft.config.mail_protocol);
    if !draft.aliases.is_empty() && policy != sender_identities::ManualIdentityPolicy::Any {
        return Err(fail(CoreError::Other(
            "this provider manages its own sender identities".into(),
        )));
    }
    if draft.aliases.len() >= sender_identities::MAX_IDENTITIES {
        return Err(fail(CoreError::Other(
            "the account has too many sender identities".into(),
        )));
    }
    let mut aliases = Vec::with_capacity(draft.aliases.len());
    for alias in &draft.aliases {
        let fields = sender_identities::manual_identity_fields(
            alias.email.clone(),
            alias.display_name.clone(),
            alias.reply_to.clone(),
        )
        .map_err(fail)?;
        if fields.0.eq_ignore_ascii_case(&email)
            || aliases
                .iter()
                .any(|(existing, _, _): &(String, _, _)| existing.eq_ignore_ascii_case(&fields.0))
        {
            return Err(fail(CoreError::Other(format!(
                "{} is listed more than once",
                fields.0
            ))));
        }
        aliases.push(fields);
    }
    if let Some(sender) = draft.config.settings.default_sender_email.as_deref()
        && !sender.eq_ignore_ascii_case(&email)
        && !aliases
            .iter()
            .any(|(alias, _, _)| alias.eq_ignore_ascii_case(sender))
    {
        return Err(fail(CoreError::Other(format!(
            "the default sender {sender} is not one of the account's addresses"
        ))));
    }
    // The marker color is cosmetic: an unusable one is dropped, not fatal.
    let color = draft
        .color
        .as_deref()
        .and_then(|color| crate::normalize_mail_profile_color(color).ok());
    let signatures = draft
        .signatures
        .iter()
        .filter_map(|signature| {
            let name = signature.name.trim();
            // The editor's limits, measured before sanitizing as the editor
            // and the planner measure them. A signature that is too large is
            // left out rather than truncated mid-markup.
            (!name.is_empty()
                && name.len() <= 100
                && signature.html.len() <= signatures::MAX_SIGNATURE_BYTES)
                .then(|| {
                    (
                        name.to_owned(),
                        signatures::sanitize_html(&signature.html),
                        signature.default,
                    )
                })
        })
        .take(signatures::MAX_SIGNATURES_PER_ACCOUNT)
        .collect();
    Ok(Checked {
        draft,
        aliases,
        color,
        signatures,
    })
}

fn insert(conn: &rusqlite::Connection, account: &Checked, settings: &mut Settings) -> Result<i64> {
    let config = &account.draft.config;
    let account_id = repo::accounts::insert_with_sync_state(
        conn,
        &repo::accounts::NewAccount {
            email: config.email.trim(),
            display_name: config.display_name.as_deref(),
            avatar_url: None,
            provider: config.provider,
            auth_kind: config.auth_kind,
            mail_protocol: config.mail_protocol,
            username: config.username.trim(),
            jmap_url: config.jmap_url.trim(),
            jmap_account_id: None,
            incoming_host: config.incoming_host.trim(),
            incoming_port: config.incoming_port,
            smtp_host: config.smtp_host.trim(),
            smtp_port: config.smtp_port,
        },
        &config.settings,
        "needs_reauth",
    )?;

    for (email, display_name, reply_to) in &account.aliases {
        repo::sender_identities::save_manual(
            conn,
            &crate::models::SenderIdentity {
                account_id,
                email: email.clone(),
                display_name: display_name.clone(),
                reply_to_email: reply_to.clone(),
                is_primary: false,
                is_provider_default: false,
                verification_status: "accepted".into(),
                last_synced_at: crate::models::now_ms(),
                source: crate::models::SenderIdentitySource::Manual,
                delivery_status: Default::default(),
                delivery_error: None,
            },
        )?;
    }

    let mut default_signature = None;
    for (name, html, default) in &account.signatures {
        let id = format!("sig-{:032x}", rand::random::<u128>());
        if *default {
            default_signature = Some(id.clone());
        }
        settings.signature_list.push(Signature {
            id,
            account_id,
            name: name.clone(),
            html: html.clone(),
        });
    }
    if let Some(id) = default_signature {
        settings.signature_defaults.insert(
            account_id.to_string(),
            SignatureDefaults {
                new_id: Some(id.clone()),
                reply_id: Some(id),
            },
        );
    }
    if let Some(color) = &account.color {
        settings
            .account_colors
            .insert(account_id.to_string(), color.clone());
    }
    Ok(account_id)
}
