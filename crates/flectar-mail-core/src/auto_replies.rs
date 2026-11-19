//! Automatic replies an account's server sends while the user is away
//! (#239): Gmail's vacation responder, a Microsoft mailbox's settings,
//! JMAP's VacationResponse, and a Sieve script of the app's own on IMAP and
//! JMAP servers that run them.
//! The app saves them to the server and reads them back while syncing, so
//! replies set in webmail show too. It never sends replies itself, so
//! servers keep their own rules against replying to lists, to automatic
//! mail and to spam.

mod graph;

use crate::Core;
use crate::accounts::credentials;
use crate::db::repo;
use crate::db::repo::auto_replies::Origin;
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use crate::models::{
    Account, AutoReply, AutoReplyQuestion, AutoReplySaved, MailProtocol, Provider, now_ms,
};
use crate::sync::engine::SyncCtx;
use crate::{oauth, sync};

impl Core {
    /// Account `account_id`'s automatic replies as last read from its server
    /// or saved to it, if the app knows them.
    pub async fn auto_reply(&self, account_id: i64) -> Result<Option<AutoReply>> {
        self.db
            .read(move |conn| repo::auto_replies::get(conn, account_id))
            .await
    }

    /// Every account's automatic replies the app knows, by account.
    pub async fn auto_replies(&self) -> Result<Vec<(i64, AutoReply)>> {
        self.db.read(|conn| repo::auto_replies::all(conn)).await
    }

    /// Reads account `account_id`'s automatic replies from its server now,
    /// for an IMAP account, whose server may keep them in a Sieve script.
    /// Other accounts' replies come with each sync.
    pub async fn check_auto_reply(&self, account_id: i64) -> Result<Option<AutoReply>> {
        let config = self
            .db
            .read(move |conn| repo::accounts::get_config(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))?;
        if config.provider == Provider::Generic && config.mail_protocol == MailProtocol::Imap {
            crate::sieve::read_back(&self.sync_ctx(), &config).await?;
        }
        self.auto_reply(account_id).await
    }

    /// Saves `reply` as account `account_id`'s automatic replies on its
    /// server, and gives them as the server keeps them now. The first time,
    /// Google asks the user to let Flectar Mail change Gmail settings, on the
    /// page `open_url` opens. On an IMAP server that runs the user's own
    /// filter script, nothing changes until the user `allowed` what saving
    /// asks.
    pub async fn set_auto_reply(
        &self,
        account_id: i64,
        reply: AutoReply,
        allowed: Option<AutoReplyQuestion>,
        open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
    ) -> Result<AutoReplySaved> {
        let (account, config, granted) = self
            .db
            .read(move |conn| {
                Ok((
                    repo::accounts::get(conn, account_id)?,
                    repo::accounts::get_config(conn, account_id)?,
                    repo::auto_replies::uses_settings(conn, account_id)?,
                ))
            })
            .await?;
        let (Some(account), Some(config)) = (account, config) else {
            return Err(CoreError::NotFound("account".into()));
        };
        let saved = match account.provider {
            Provider::Gmail => {
                if !granted {
                    self.grant_gmail_settings(&account, open_url).await?;
                }
                let responder = sync::gmail::VacationResponder::new(self.sync_ctx(), config)?;
                match responder.save(&reply).await {
                    // Google no longer lets the app change Gmail settings, so
                    // the next save asks the user again.
                    Err(CoreError::NeedsReauth) => {
                        self.db
                            .write(move |conn| {
                                repo::auto_replies::forget_settings(conn, account_id)
                            })
                            .await?;
                        return Err(CoreError::Auth(
                            "Google didn't let Flectar Mail change this account's Gmail \
                             settings. Save again to allow it"
                                .into(),
                        ));
                    }
                    saved => saved?,
                }
            }
            Provider::Microsoft => {
                // Microsoft asks for permission only while the account hasn't
                // given it.
                let token = self
                    .graph_token_with_consent(
                        account_id,
                        oauth::providers::MS_MAILBOX_SETTINGS_SCOPE,
                        open_url,
                    )
                    .await?;
                match graph::GraphReplies::new(crate::net::proxy::Owner::Account(account_id))?
                    .save(&token, &reply)
                    .await
                {
                    Err(CoreError::NeedsReauth) => {
                        return Err(CoreError::Auth(
                            "Microsoft didn't let Flectar Mail change this account's settings. \
                             Sign in to the account again, then save again"
                                .into(),
                        ));
                    }
                    saved => saved?,
                }
            }
            Provider::Generic if account.mail_protocol == MailProtocol::Jmap => {
                let secret = credentials::load_async(
                    self.credentials.clone(),
                    account_id,
                    credentials::Slot::Password,
                )
                .await?;
                let connected = crate::jmap::client::connect(&config, &secret).await?;
                // A server that runs Sieve scripts keeps them in the app's
                // script, as over ManageSieve.
                if let Some(mut scripts) = crate::jmap::sieve::replies(&connected) {
                    let addresses = self.reply_addresses(account_id).await?;
                    let writer = crate::server_rules::writer(account_id);
                    let _writing = writer.lock().await;
                    match crate::sieve::save_to(&mut scripts, &reply, &addresses, allowed.as_ref())
                        .await?
                    {
                        AutoReplySaved::Done(saved) => saved,
                        ask @ AutoReplySaved::Ask(_) => return Ok(ask),
                    }
                } else if crate::jmap::vacation::offered(&connected) {
                    crate::jmap::vacation::save(&connected, &reply).await?
                } else {
                    return Err(CoreError::Other(
                        "Flectar Mail can't set this account's automatic replies yet".into(),
                    ));
                }
            }
            Provider::Generic if account.mail_protocol == MailProtocol::Imap => {
                let password = credentials::load_async(
                    self.credentials.clone(),
                    account_id,
                    credentials::Slot::Password,
                )
                .await?;
                let addresses = self.reply_addresses(account_id).await?;
                let writer = crate::server_rules::writer(account_id);
                let _writing = writer.lock().await;
                match crate::sieve::save(
                    &crate::sieve::server(&config),
                    &config.username,
                    &password,
                    &reply,
                    &addresses,
                    allowed.as_ref(),
                )
                .await?
                {
                    AutoReplySaved::Done(saved) => saved,
                    ask @ AutoReplySaved::Ask(_) => return Ok(ask),
                }
            }
            Provider::Generic => {
                return Err(CoreError::Other(
                    "Flectar Mail can't set this account's automatic replies".into(),
                ));
            }
        };
        let stored = saved.clone();
        self.db
            .write(move |conn| {
                repo::auto_replies::store(conn, account_id, &stored, now_ms(), Origin::Saved)
            })
            .await?;
        self.bus.emit(CoreEvent::AutoReplyChanged { account_id });
        Ok(AutoReplySaved::Done(saved))
    }

    /// The addresses account `account_id` receives mail at, which Sieve
    /// replies answer mail to.
    async fn reply_addresses(&self, account_id: i64) -> Result<Vec<String>> {
        self.db
            .read(move |conn| crate::server_rules::reply_addresses(conn, account_id))
            .await
    }

    /// Asks Google, in the browser, to let the app change `account`'s Gmail
    /// settings, keeping every other permission it uses.
    async fn grant_gmail_settings(
        &self,
        account: &Account,
        open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
    ) -> Result<()> {
        credentials::check_available_async(self.credentials.clone()).await?;
        let mut scopes = self.google_scopes_in_use(account.id).await?;
        if !scopes.contains(&oauth::providers::GMAIL_SETTINGS_SCOPE) {
            scopes.push(oauth::providers::GMAIL_SETTINGS_SCOPE);
        }
        let outcome = tokio::select! {
            r = oauth::flow::authorize_with_broker(
                Provider::Gmail,
                &scopes,
                Some(&account.email),
                self.oauth_redirects.clone(),
                open_url,
            ) => r?,
            _ = self.oauth_cancel.notified() => {
                return Err(CoreError::Auth("sign-in cancelled".into()));
            }
        };
        self.store_sign_in(account, outcome).await?;
        Ok(())
    }
}

/// Reads a Microsoft account's automatic replies back from its mailbox
/// settings, once the app saved them there, so that replies changed in
/// Outlook show too.
pub(crate) async fn read_back_from_graph(ctx: &SyncCtx, account_id: i64) -> Result<()> {
    let saved_at = ctx
        .db
        .read(move |conn| repo::auto_replies::saved_at(conn, account_id))
        .await?;
    if saved_at.is_none() {
        return Ok(());
    }
    let token = ctx
        .tokens
        .access_token_for_scope(
            account_id,
            Provider::Microsoft,
            oauth::providers::MS_MAILBOX_SETTINGS_SCOPE,
        )
        .await?;
    let reply = graph::GraphReplies::new(crate::net::proxy::Owner::Account(account_id))?
        .read(&token)
        .await?;
    let changed = ctx
        .db
        .write(move |conn| {
            repo::auto_replies::store(
                conn,
                account_id,
                &reply,
                now_ms(),
                Origin::Read { saved_at },
            )
        })
        .await?;
    if changed {
        ctx.bus.emit(CoreEvent::AutoReplyChanged { account_id });
    }
    Ok(())
}

#[cfg(test)]
#[path = "auto_replies_tests.rs"]
mod tests;
