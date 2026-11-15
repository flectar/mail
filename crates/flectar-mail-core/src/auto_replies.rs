//! Automatic replies an account's server sends while the user is away
//! (#239): Gmail's vacation responder for now. The app saves them to the
//! server and reads them back on each sync, so replies set in webmail show
//! too. It never sends replies itself, so servers keep their own rules
//! against replying to lists, to automatic mail and to spam.

use crate::Core;
use crate::accounts::credentials;
use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use crate::models::{Account, AutoReply, Provider, now_ms};
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

    /// Saves `reply` as account `account_id`'s automatic replies on its
    /// server, and gives them as the server keeps them now. The first time,
    /// Google asks the user to let Flectar Mail change Gmail settings, on the
    /// page `open_url` opens.
    pub async fn set_auto_reply(
        &self,
        account_id: i64,
        reply: AutoReply,
        open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
    ) -> Result<AutoReply> {
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
        if account.provider != Provider::Gmail {
            return Err(CoreError::Other(
                "Flectar Mail can't set this account's automatic replies yet".into(),
            ));
        }
        if !granted {
            self.grant_gmail_settings(&account, open_url).await?;
        }
        let responder = sync::gmail::VacationResponder::new(self.sync_ctx(), config)?;
        let saved = match responder.save(&reply).await {
            Ok(saved) => saved,
            // Google no longer lets the app change Gmail settings, so the
            // next save asks the user again.
            Err(CoreError::NeedsReauth) => {
                self.db
                    .write(move |conn| repo::auto_replies::forget_settings(conn, account_id))
                    .await?;
                return Err(CoreError::Auth(
                    "Google didn't let Flectar Mail change this account's Gmail settings. Save \
                     again to allow it"
                        .into(),
                ));
            }
            Err(error) => return Err(error),
        };
        let stored = saved.clone();
        self.db
            .write(move |conn| repo::auto_replies::store(conn, account_id, &stored, now_ms(), true))
            .await?;
        self.bus.emit(CoreEvent::AutoReplyChanged { account_id });
        Ok(saved)
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

#[cfg(test)]
#[path = "auto_replies_tests.rs"]
mod tests;
