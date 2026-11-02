//! The account list of a portable account backup.
//!
//! Version 2 records each account exactly as Flectar stores it. Version 1 was
//! written before connection security became explicit: it named the incoming
//! server `imapHost`/`imapPort`, used the provider `imap` for generic servers,
//! stored Gmail accounts with the protocol `imap`, and could leave security
//! to the `auto` port rule. Version 1 entries are converted with the same
//! rules the database migration applies to stored accounts.

use crate::models::PortableAccountConfig;
use serde_json::{Map, Value};

pub const FORMAT: &str = "flectar-mail-account-backup";
pub const VERSION: u64 = 2;

/// Parse the `accounts` array of a backup written with format `version`.
pub fn parse_accounts(version: u64, accounts: Value) -> Result<Vec<PortableAccountConfig>, String> {
    if !matches!(version, 1 | VERSION) {
        return Err("this backup version is not supported".into());
    }
    let Value::Array(entries) = accounts else {
        return Err("backup has no accounts list".into());
    };
    entries
        .into_iter()
        .map(|entry| {
            let entry = if version == 1 {
                upgrade_version_1(entry)?
            } else {
                entry
            };
            serde_json::from_value(entry).map_err(|error| format!("invalid account data: {error}"))
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Server {
    Incoming,
    Smtp,
}

fn upgrade_version_1(entry: Value) -> Result<Value, String> {
    let Value::Object(mut account) = entry else {
        return Err("invalid account data: an account must be an object".into());
    };
    let provider = match account.get("provider").and_then(Value::as_str) {
        Some("imap") => "generic",
        Some(provider) => provider,
        None => return Err("invalid account data: missing provider".into()),
    }
    .to_owned();
    let protocol = if provider == "gmail" {
        "gmail".to_owned()
    } else {
        match account.get("mailProtocol") {
            None => "imap".to_owned(),
            Some(Value::String(protocol)) => protocol.clone(),
            Some(_) => return Err("invalid account data: mailProtocol must be text".into()),
        }
    };
    account.insert("provider".into(), Value::String(provider));
    account.insert("mailProtocol".into(), Value::String(protocol));
    for (old, new) in [("imapHost", "incomingHost"), ("imapPort", "incomingPort")] {
        let value = account
            .remove(old)
            .ok_or_else(|| format!("invalid account data: missing {old}"))?;
        account.insert(new.into(), value);
    }

    let incoming_port = port(&account, "incomingPort")?;
    let smtp_port = port(&account, "smtpPort")?;
    let mut settings = match account.remove("settings") {
        None => Map::new(),
        Some(Value::Object(settings)) => settings,
        Some(_) => return Err("invalid account data: settings must be an object".into()),
    };
    let connection = match settings.remove("connection") {
        None => Map::new(),
        Some(Value::Object(connection)) => connection,
        Some(_) => return Err("invalid account data: connection must be an object".into()),
    };
    let certificate = match connection.get("trustedCertificatePem") {
        None => Value::String(String::new()),
        Some(Value::String(pem)) => Value::String(pem.clone()),
        Some(_) => {
            return Err("invalid account data: trustedCertificatePem must be text".into());
        }
    };
    let mut upgraded = Map::new();
    upgraded.insert(
        "incomingSecurity".into(),
        explicit_security(
            connection.get("imapSecurity"),
            incoming_port,
            Server::Incoming,
        )?
        .into(),
    );
    upgraded.insert(
        "smtpSecurity".into(),
        explicit_security(connection.get("smtpSecurity"), smtp_port, Server::Smtp)?.into(),
    );
    upgraded.insert("trustedCertificatePem".into(), certificate);
    settings.insert("connection".into(), Value::Object(upgraded));
    account.insert("settings".into(), Value::Object(settings));
    Ok(Value::Object(account))
}

fn port(account: &Map<String, Value>, key: &str) -> Result<u64, String> {
    account
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("invalid account data: {key} must be a port number"))
}

/// The mode a version 1 setting selected. `auto` (or no value) used STARTTLS
/// on the cleartext access/submission ports and TLS everywhere else.
fn explicit_security(
    stored: Option<&Value>,
    port: u64,
    server: Server,
) -> Result<&'static str, String> {
    match stored {
        None => Ok(conventional_security(port, server)),
        Some(Value::String(mode)) => match mode.as_str() {
            "auto" => Ok(conventional_security(port, server)),
            "tls" => Ok("tls"),
            "starttls" => Ok("starttls"),
            other => Err(format!(
                "invalid account data: unsupported connection security {other:?}"
            )),
        },
        Some(_) => Err("invalid account data: connection security must be text".into()),
    }
}

fn conventional_security(port: u64, server: Server) -> &'static str {
    let starttls = match server {
        Server::Incoming => port == 143,
        Server::Smtp => port != 465,
    };
    if starttls { "starttls" } else { "tls" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        AccountSettings, AuthKind, ConnectionSecurity, MailConnectionSettings, MailHistory,
        MailProtocol, Provider,
    };
    use serde_json::json;

    fn version_1(provider: &str, extra: Value) -> Value {
        let mut account = json!({
            "email": "me@example.test",
            "displayName": "Me",
            "provider": provider,
            "authKind": if provider == "imap" { "password" } else { "oauth2" },
            "username": "me",
            "jmapUrl": "",
            "imapHost": "mail.example.test",
            "imapPort": 993,
            "smtpHost": "mail.example.test",
            "smtpPort": 587
        });
        for (key, value) in extra.as_object().unwrap() {
            account[key] = value.clone();
        }
        account
    }

    #[test]
    fn version_1_accounts_convert_like_stored_accounts() {
        let accounts = parse_accounts(
            1,
            json!([
                version_1("imap", json!({})),
                version_1(
                    "imap",
                    json!({
                        "imapPort": 143,
                        "smtpPort": 465,
                        "settings": {
                            "mailHistory": "oneYear",
                            "connection": {
                                "imapSecurity": "auto",
                                "smtpSecurity": "auto",
                                "trustedCertificatePem": "PEM"
                            }
                        }
                    })
                ),
                version_1(
                    "imap",
                    json!({
                        "mailProtocol": "jmap",
                        "jmapUrl": "https://jmap.example.test",
                        "settings": {
                            "connection": {"imapSecurity": "starttls", "smtpSecurity": "tls"}
                        }
                    })
                ),
                version_1("gmail", json!({"smtpPort": 465})),
                version_1("microsoft", json!({})),
            ]),
        )
        .unwrap();

        let summary = accounts
            .iter()
            .map(|account| {
                (
                    account.provider,
                    account.mail_protocol,
                    account.incoming_port,
                    account.settings.connection.incoming_security,
                    account.settings.connection.smtp_security,
                )
            })
            .collect::<Vec<_>>();
        use ConnectionSecurity::{Starttls, Tls};
        assert_eq!(
            summary,
            [
                (Provider::Generic, MailProtocol::Imap, 993, Tls, Starttls),
                (Provider::Generic, MailProtocol::Imap, 143, Starttls, Tls),
                (Provider::Generic, MailProtocol::Jmap, 993, Starttls, Tls),
                (Provider::Gmail, MailProtocol::Gmail, 993, Tls, Tls),
                (Provider::Microsoft, MailProtocol::Imap, 993, Tls, Starttls),
            ]
        );
        assert_eq!(accounts[0].settings.mail_history, MailHistory::SixMonths);
        assert_eq!(accounts[1].settings.mail_history, MailHistory::OneYear);
        assert_eq!(
            accounts[1].settings.connection.trusted_certificate_pem,
            "PEM"
        );
        assert_eq!(accounts[1].incoming_host, "mail.example.test");
    }

    #[test]
    fn version_1_rejects_values_it_could_not_contain() {
        for extra in [
            json!({"settings": {"connection": {"imapSecurity": "none"}}}),
            json!({"settings": {"connection": {"smtpSecurity": 1}}}),
            json!({"settings": []}),
            json!({"imapPort": "993"}),
        ] {
            assert!(parse_accounts(1, json!([version_1("imap", extra.clone())])).is_err());
        }
        let mut missing_host = version_1("imap", json!({}));
        missing_host.as_object_mut().unwrap().remove("imapHost");
        assert!(parse_accounts(1, json!([missing_host])).is_err());
    }

    #[test]
    fn current_backups_round_trip_and_unknown_versions_are_rejected() {
        let account = PortableAccountConfig {
            email: "me@example.test".into(),
            display_name: None,
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            username: "me".into(),
            jmap_url: String::new(),
            incoming_host: "mail.example.test".into(),
            incoming_port: 143,
            smtp_host: "mail.example.test".into(),
            smtp_port: 587,
            settings: AccountSettings::new(MailConnectionSettings::new(
                ConnectionSecurity::Starttls,
                ConnectionSecurity::Starttls,
            )),
        };
        let value = serde_json::to_value([&account]).unwrap();
        assert_eq!(value[0]["incomingHost"], "mail.example.test");
        assert_eq!(parse_accounts(VERSION, value.clone()).unwrap(), [account]);
        assert!(parse_accounts(VERSION + 1, value).is_err());
        assert!(parse_accounts(VERSION, json!({})).is_err());
        let version_1_shape = json!([version_1("imap", json!({}))]);
        assert!(
            parse_accounts(VERSION, version_1_shape).is_err(),
            "current backups never carry version 1 names"
        );
    }
}
