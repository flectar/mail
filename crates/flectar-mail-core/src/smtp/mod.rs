//! SMTP sending via lettre, with password or XOAUTH2 auth.

use crate::error::{CoreError, Result};
use crate::models::AccountConfig;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

pub enum SmtpAuth {
    Password(String),
    XOAuth2(String),
}

fn build_transport(
    cfg: &AccountConfig,
    auth: &SmtpAuth,
) -> Result<AsyncSmtpTransport<Tokio1Executor>> {
    use lettre::transport::smtp::client::{Tls, TlsParameters};

    use crate::models::ConnectionSecurity;
    let implicit = match cfg.settings.connection.smtp_security {
        ConnectionSecurity::Tls => true,
        ConnectionSecurity::Starttls => false,
        ConnectionSecurity::Auto => cfg.smtp_port == 465,
    };
    let mut params = TlsParameters::builder(cfg.smtp_host.clone());
    for cert in crate::imap::trusted_certificates(&cfg.settings.connection.trusted_certificate_pem)?
    {
        params = params.add_root_certificate(
            lettre::transport::smtp::client::Certificate::from_der(cert.as_ref().to_vec())
                .map_err(|e| CoreError::Tls(e.to_string()))?,
        );
    }
    if crate::imap::tls_insecure() && cfg.settings.connection.trusted_certificate_pem.is_empty() {
        params = params
            .dangerous_accept_invalid_certs(true)
            .dangerous_accept_invalid_hostnames(true);
    }
    let params = params.build().map_err(|e| CoreError::Tls(e.to_string()))?;
    let builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&cfg.smtp_host)
        .port(cfg.smtp_port)
        .tls(if implicit {
            Tls::Wrapper(params)
        } else {
            Tls::Required(params)
        });

    let builder = match auth {
        SmtpAuth::Password(pw) => builder
            .credentials(Credentials::new(cfg.username.clone(), pw.clone()))
            .authentication(vec![Mechanism::Plain, Mechanism::Login]),
        SmtpAuth::XOAuth2(token) => builder
            .credentials(Credentials::new(cfg.username.clone(), token.clone()))
            .authentication(vec![Mechanism::Xoauth2]),
    };

    // Bound greeting, STARTTLS and AUTH. Exchange Online can leave a throttled
    // SMTP session open without completing one of these phases; without a
    // transport timeout a queued Outlook send remains inflight forever.
    Ok(builder
        .timeout(Some(std::time::Duration::from_secs(30)))
        .build())
}

/// Send a fully built RFC 5322 message.
pub async fn send_raw(
    cfg: &AccountConfig,
    auth: &SmtpAuth,
    from: &str,
    recipients: &[String],
    raw: &[u8],
) -> Result<()> {
    use lettre::address::Envelope;
    let from_addr = from
        .parse()
        .map_err(|e| CoreError::Smtp(format!("bad from address: {e}")))?;
    let mut tos = Vec::with_capacity(recipients.len());
    for r in recipients {
        tos.push(
            r.parse()
                .map_err(|e| CoreError::Smtp(format!("bad recipient {r}: {e}")))?,
        );
    }
    let envelope = Envelope::new(Some(from_addr), tos)
        .map_err(|e| CoreError::Smtp(format!("Invalid message envelope: {e}")))?;

    let transport = build_transport(cfg, auth)?;
    tracing::debug!(
        host = %cfg.smtp_host,
        port = cfg.smtp_port,
        auth = match auth {
            SmtpAuth::Password(_) => "password",
            SmtpAuth::XOAuth2(_) => "xoauth2",
        },
        bytes = raw.len(),
        recipients = recipients.len(),
        "smtp send_raw: connecting to relay",
    );
    transport
        .send_raw(&envelope, &crate::mail_security::without_bcc(raw)?)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("535") || msg.to_lowercase().contains("auth") {
                CoreError::Auth(format!("smtp auth: {msg}"))
            } else {
                CoreError::Smtp(msg)
            }
        })?;
    Ok(())
}

/// Cheap connectivity/auth probe used by test_connection.
pub async fn test_connection(cfg: &AccountConfig, auth: &SmtpAuth) -> Result<()> {
    let transport = build_transport(cfg, auth)?;
    let ok = transport
        .test_connection()
        .await
        .map_err(|e| {
            if e.is_tls() {
                CoreError::Tls(format!("SMTP {}:{}: {e}. Check SSL/TLS versus STARTTLS and the server certificate. For Proton Bridge, import its exported certificate.", cfg.smtp_host, cfg.smtp_port))
            } else {
                CoreError::Smtp(format!("{}:{}: {e}", cfg.smtp_host, cfg.smtp_port))
            }
        })?;
    if ok {
        Ok(())
    } else {
        Err(CoreError::Smtp("connection test failed".into()))
    }
}
