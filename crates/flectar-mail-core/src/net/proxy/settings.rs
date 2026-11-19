//! The proxy choices people make in Settings, as stored. Passwords stay in
//! the keyring.

use super::ProxyKind;
use serde::{Deserialize, Serialize};

/// How the app's connections reach their servers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySettings {
    #[serde(default)]
    pub mode: ProxyMode,
    /// The proxy of [`ProxyMode::Manual`]. It's kept while another mode is
    /// chosen, so choosing Manual again brings it back.
    #[serde(default)]
    pub manual: ManualProxy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProxyMode {
    /// Whatever proxy the operating system's settings name, if any.
    #[default]
    System,
    /// No proxy: every connection goes straight to its server.
    Direct,
    /// The proxy in [`ProxySettings::manual`].
    Manual,
}

/// A proxy typed in by hand. Its password is in the keyring.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualProxy {
    #[serde(default)]
    pub kind: ProxyKind,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    /// The user name, or empty when the proxy doesn't ask for one.
    #[serde(default)]
    pub user: String,
    /// Servers reached without the proxy, as [`super::Bypass`] reads them.
    #[serde(default)]
    pub bypass: String,
}

impl ManualProxy {
    /// Whether the proxy can be used: a server name and a port.
    pub fn is_complete(&self) -> bool {
        !self.host.trim().is_empty() && self.port != 0
    }
}

/// Which proxy one account's connections use.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "use")]
pub enum AccountProxy {
    /// The app's proxy setting.
    #[default]
    App,
    /// No proxy, whatever the app uses.
    Direct,
    /// A proxy of the account's own. Its password is in the keyring under
    /// the account.
    Own(ManualProxy),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_forms() {
        assert_eq!(
            serde_json::to_value(ProxySettings::default()).unwrap(),
            serde_json::json!({
                "mode": "system",
                "manual": {"kind": "socks5", "host": "", "port": 0, "user": "", "bypass": ""}
            })
        );
        assert_eq!(
            serde_json::to_value(AccountProxy::Direct).unwrap(),
            serde_json::json!({"use": "direct"})
        );
        let own = AccountProxy::Own(ManualProxy {
            kind: ProxyKind::Http,
            host: "proxy.example".into(),
            port: 3128,
            user: "ana".into(),
            bypass: String::new(),
        });
        let stored = serde_json::to_value(&own).unwrap();
        assert_eq!(
            stored,
            serde_json::json!({
                "use": "own", "kind": "http", "host": "proxy.example", "port": 3128,
                "user": "ana", "bypass": ""
            })
        );
        assert_eq!(serde_json::from_value::<AccountProxy>(stored).unwrap(), own);
    }

    #[test]
    fn settings_saved_before_proxies_existed_use_the_system() {
        let settings: ProxySettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.mode, ProxyMode::System);
        let account: AccountProxy = serde_json::from_str(r#"{"use": "app"}"#).unwrap();
        assert_eq!(account, AccountProxy::App);
    }
}
