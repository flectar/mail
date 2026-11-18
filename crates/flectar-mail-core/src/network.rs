//! Settings → Network: the proxy connections go through (#250).
//!
//! The app's choice is kept in Settings and each account's in its connection
//! settings, with passwords in the keyring. They're handed to `net::proxy`,
//! which every connection asks as it opens, so a change applies to the next
//! connection; open ones are closed and opened again.

use crate::Core;
use crate::accounts::credentials::{self, Slot};
use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::models::{AccountConfig, ConnectionSecurity, MailProtocol};
use crate::net::proxy::{
    self, AccountOverride, AccountProxy, AppProxy, Bypass, Manual, ManualProxy, Owner, ProxyError,
    ProxyKind, ProxyMode, ProxySettings,
};
use std::time::Duration;

/// The keyring owner of the app's own secrets.
const APP: i64 = 0;

/// The app's proxy settings, as Settings shows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkSettings {
    pub proxy: ProxySettings,
    /// A password is saved for the manual proxy.
    pub password_saved: bool,
}

/// One account's proxy choice, as its settings show it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountNetwork {
    pub proxy: AccountProxy,
    /// A password is saved for the account's own proxy.
    pub password_saved: bool,
}

/// What trying the route of an account's mail came to.
#[derive(Debug)]
pub enum NetworkCheck {
    /// This server answered through the route.
    Answered(String),
    /// No account has a server to try.
    NoServer,
    /// The proxy failed, and said why.
    Proxy(ProxyError),
    /// The connection failed past the proxy, or without one.
    Failed(String),
}

/// How long a check may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// What to do with a saved proxy password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyPassword {
    /// Keep the one saved, if any.
    Keep,
    /// Save this one in its place.
    Set(String),
    /// Forget the one saved.
    Remove,
}

impl Core {
    /// The app's proxy settings.
    pub async fn network_settings(&self) -> Result<NetworkSettings> {
        let proxy = self
            .db
            .read(|conn| Ok(repo::settings::get(conn)?.proxy))
            .await?;
        let password_saved = self.saved_proxy_password(APP).await?.is_some();
        Ok(NetworkSettings {
            proxy,
            password_saved,
        })
    }

    /// Save how the app's connections reach their servers. Connections of
    /// the app, and of the accounts that follow it, open again through the
    /// new route.
    pub async fn set_network_settings(
        &self,
        proxy: ProxySettings,
        password: ProxyPassword,
    ) -> Result<()> {
        let proxy = checked_settings(proxy)?;
        self.store_proxy_password(APP, &proxy.manual, password)
            .await?;
        // Saving asks to try the proxy now, even one that turned the sign-in
        // down a moment ago.
        proxy::try_refused_proxies_again();
        let stored = proxy.clone();
        self.db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let mut settings = repo::settings::get(&tx)?;
                settings.proxy = stored;
                repo::settings::set(&tx, &settings)?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        let before = proxy::generation();
        self.apply_app_proxy(&proxy).await;
        if proxy::generation() != before {
            let following = self
                .list_account_configs()
                .await?
                .into_iter()
                .filter(|config| config.settings.connection.proxy == AccountProxy::App)
                .map(|config| config.id)
                .collect::<Vec<_>>();
            self.reopen_connections(&following).await;
        }
        Ok(())
    }

    /// One account's proxy choice.
    pub async fn account_network(&self, account_id: i64) -> Result<AccountNetwork> {
        let config = self.account_config(account_id).await?;
        let password_saved = matches!(config.settings.connection.proxy, AccountProxy::Own(_))
            && self.saved_proxy_password(account_id).await?.is_some();
        Ok(AccountNetwork {
            proxy: config.settings.connection.proxy,
            password_saved,
        })
    }

    /// Choose whether an account follows the app's proxy, connects without
    /// one, or has its own. Its connections open again through the new route.
    pub async fn set_account_network(
        &self,
        account_id: i64,
        proxy: AccountProxy,
        password: ProxyPassword,
    ) -> Result<()> {
        let proxy = match proxy {
            AccountProxy::Own(manual) => AccountProxy::Own(checked_manual(manual)?),
            other => other,
        };
        proxy::try_refused_proxies_again();
        match &proxy {
            AccountProxy::Own(manual) => {
                self.store_proxy_password(account_id, manual, password)
                    .await?;
            }
            // Its password has no use any more.
            AccountProxy::App | AccountProxy::Direct => {
                self.forget_proxy_password(account_id).await?;
            }
        }
        let stored = proxy.clone();
        self.db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let mut config = repo::accounts::get_config(&tx, account_id)?
                    .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
                config.settings.connection.proxy = stored;
                repo::accounts::set_settings(&tx, account_id, &config.settings)?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        let before = proxy::generation();
        self.apply_account_proxy(account_id, &proxy).await;
        if proxy::generation() != before {
            self.reopen_connections(&[account_id]).await;
        }
        Ok(())
    }

    /// Hand the saved proxy settings to the connections, before the first
    /// one opens.
    pub(crate) async fn load_proxy_settings(&self) -> Result<()> {
        let settings = self.db.read(|conn| repo::settings::get(conn)).await?;
        self.apply_app_proxy(&settings.proxy).await;
        for config in self.list_account_configs().await? {
            self.apply_account_proxy(config.id, &config.settings.connection.proxy)
                .await;
        }
        Ok(())
    }

    /// Try the route an account's mail takes now: open a connection to its
    /// incoming server, through the proxy if it has one, without signing in.
    /// `account_id` names the account; without it, the first account that
    /// follows the app's proxy is tried.
    pub async fn check_network(&self, account_id: Option<i64>) -> Result<NetworkCheck> {
        let configs = self.list_account_configs().await?;
        let config = match account_id {
            Some(id) => configs.into_iter().find(|config| config.id == id),
            None => configs
                .into_iter()
                .find(|config| config.settings.connection.proxy == AccountProxy::App),
        };
        let Some((scheme, host, port)) = config.as_ref().and_then(incoming_server) else {
            return Ok(NetworkCheck::NoServer);
        };
        let owner = config.map_or(Owner::App, |config| config.proxy_owner());
        let connected = tokio::time::timeout(
            CHECK_TIMEOUT,
            proxy::connect_for(owner, scheme, &host, port),
        )
        .await;
        Ok(match connected {
            Ok(Ok(_stream)) => NetworkCheck::Answered(host),
            Ok(Err(error)) => {
                let message = error.to_string();
                match error
                    .into_inner()
                    .map(|inner| inner.downcast::<ProxyError>())
                {
                    Some(Ok(proxy_error)) => NetworkCheck::Proxy(*proxy_error),
                    _ => NetworkCheck::Failed(message),
                }
            }
            Err(_) => NetworkCheck::Failed(format!("{host} didn't answer in time")),
        })
    }

    /// Stop using a removed account's proxy.
    pub(crate) fn forget_account_proxy(account_id: i64) {
        proxy::set_account(account_id, None);
    }

    async fn apply_app_proxy(&self, settings: &ProxySettings) {
        let app = match settings.mode {
            ProxyMode::System => AppProxy::System,
            ProxyMode::Direct => AppProxy::Direct,
            ProxyMode::Manual => {
                let password = self.proxy_password_for(APP, &settings.manual).await;
                AppProxy::Manual(Manual::new(&settings.manual, password))
            }
        };
        proxy::set_app(app);
    }

    async fn apply_account_proxy(&self, account_id: i64, choice: &AccountProxy) {
        let choice = match choice {
            AccountProxy::App => None,
            AccountProxy::Direct => Some(AccountOverride::Direct),
            AccountProxy::Own(manual) => {
                let password = self.proxy_password_for(account_id, manual).await;
                Some(AccountOverride::Own(Manual::new(manual, password)))
            }
        };
        proxy::set_account(account_id, choice);
    }

    /// The password of a proxy that asks for a user name. A keyring that
    /// can't be read leaves it out, so the proxy says it wants one.
    async fn proxy_password_for(&self, owner: i64, manual: &ManualProxy) -> Option<String> {
        if manual.user.trim().is_empty() {
            return None;
        }
        match self.saved_proxy_password(owner).await {
            Ok(password) => password,
            Err(error) => {
                tracing::warn!(owner, %error, "could not read the proxy password");
                None
            }
        }
    }

    async fn saved_proxy_password(&self, owner: i64) -> Result<Option<String>> {
        match credentials::load_async(self.credentials.clone(), owner, Slot::ProxyPassword).await {
            Ok(password) if !password.is_empty() => Ok(Some(password)),
            Ok(_) => Ok(None),
            Err(CoreError::CredentialStoreUnavailable(message)) => {
                Err(CoreError::CredentialStoreUnavailable(message))
            }
            Err(_) => Ok(None),
        }
    }

    async fn store_proxy_password(
        &self,
        owner: i64,
        manual: &ManualProxy,
        password: ProxyPassword,
    ) -> Result<()> {
        // A proxy without a user name takes no password.
        if manual.user.trim().is_empty() {
            return self.forget_proxy_password(owner).await;
        }
        match password {
            ProxyPassword::Keep => Ok(()),
            ProxyPassword::Remove => self.forget_proxy_password(owner).await,
            ProxyPassword::Set(password) if password.is_empty() => {
                self.forget_proxy_password(owner).await
            }
            ProxyPassword::Set(password) => {
                if manual.kind == ProxyKind::Socks5 && password.len() > 255 {
                    return Err(CoreError::Other(
                        "The password is longer than a SOCKS5 proxy accepts.".into(),
                    ));
                }
                credentials::store_async(
                    self.credentials.clone(),
                    owner,
                    Slot::ProxyPassword,
                    password,
                )
                .await
            }
        }
    }

    async fn forget_proxy_password(&self, owner: i64) -> Result<()> {
        match credentials::delete_async(self.credentials.clone(), owner, Slot::ProxyPassword).await
        {
            Ok(()) => Ok(()),
            Err(CoreError::CredentialStoreUnavailable(message)) => {
                Err(CoreError::CredentialStoreUnavailable(message))
            }
            // Nothing was saved.
            Err(_) => Ok(()),
        }
    }

    async fn account_config(&self, account_id: i64) -> Result<AccountConfig> {
        self.db
            .read(move |conn| repo::accounts::get_config(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))
    }

    /// Close and reopen the connections of these accounts, so they take the
    /// route chosen now. HTTP clients shared between requests are built again
    /// on their next request.
    async fn reopen_connections(&self, accounts: &[i64]) {
        for &account_id in accounts {
            self.restart_actor(account_id).await;
        }
        for (account_id, handle) in self.cal_handles.read().await.iter() {
            if accounts.contains(account_id) {
                handle.nudge();
            }
        }
        for (account_id, handle) in self.card_handles.read().await.iter() {
            if accounts.contains(account_id) {
                handle.nudge();
            }
        }
    }
}

/// Where an account's mail comes from, as a connection opens it: the
/// scheme the system's settings know it by, the server and the port.
fn incoming_server(config: &AccountConfig) -> Option<(&'static str, String, u16)> {
    let encrypted = config.settings.connection.incoming_security == ConnectionSecurity::Tls;
    match config.mail_protocol {
        MailProtocol::Imap => Some((
            if encrypted { "imaps" } else { "imap" },
            config.incoming_host.clone(),
            config.incoming_port,
        )),
        MailProtocol::Pop3 => Some((
            if encrypted { "pop3s" } else { "pop3" },
            config.incoming_host.clone(),
            config.incoming_port,
        )),
        MailProtocol::Jmap => {
            let url = url::Url::parse(&config.jmap_url).ok()?;
            Some((
                "https",
                url.host_str()?.to_owned(),
                url.port_or_known_default()?,
            ))
        }
        MailProtocol::Gmail => Some(("https", "gmail.googleapis.com".into(), 443)),
        MailProtocol::Local => None,
    }
}

/// The settings with a manual proxy that can be used, or why not.
fn checked_settings(mut settings: ProxySettings) -> Result<ProxySettings> {
    let manual = std::mem::take(&mut settings.manual);
    settings.manual = if settings.mode == ProxyMode::Manual {
        checked_manual(manual)?
    } else {
        // Kept for later, as typed, when it's not in use.
        manual
    };
    Ok(settings)
}

fn checked_manual(mut manual: ManualProxy) -> Result<ManualProxy> {
    let host = manual.host.trim();
    // An IPv6 address may come in brackets, as in a URL.
    manual.host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
        .to_owned();
    manual.user = manual.user.trim().to_owned();
    manual.bypass = manual.bypass.trim().to_owned();
    if manual.host.is_empty() || manual.port == 0 {
        return Err(CoreError::Other(
            "Enter the proxy's server and port.".into(),
        ));
    }
    proxy::check_server_name(&manual.host).map_err(|_| {
        CoreError::Other(format!("{} isn't a server name or address.", manual.host))
    })?;
    if manual.kind == ProxyKind::Socks4a {
        return Err(CoreError::Other("Choose a SOCKS5 or HTTP proxy.".into()));
    }
    if manual.kind == ProxyKind::Socks5 && manual.user.len() > 255 {
        return Err(CoreError::Other(
            "The user name is longer than a SOCKS5 proxy accepts.".into(),
        ));
    }
    let unreadable = Bypass::unreadable(&manual.bypass);
    if !unreadable.is_empty() {
        return Err(CoreError::Other(format!(
            "{} can't be read as server names or addresses.",
            unreadable.join(", ")
        )));
    }
    Ok(manual)
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
