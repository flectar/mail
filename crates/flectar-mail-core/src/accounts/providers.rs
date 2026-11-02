//! Well-known server settings for OAuth providers. Password/IMAP presets for
//! Fastmail, iCloud, and similar providers live in the Slint onboarding flow.

use crate::models::{ConnectionSecurity, MailConnectionSettings, MailProtocol};

pub struct ProviderServers {
    pub mail_protocol: MailProtocol,
    pub incoming_host: &'static str,
    pub incoming_port: u16,
    pub incoming_security: ConnectionSecurity,
    pub smtp_host: &'static str,
    pub smtp_port: u16,
    pub smtp_security: ConnectionSecurity,
}

impl ProviderServers {
    pub fn connection(&self) -> MailConnectionSettings {
        MailConnectionSettings::new(self.incoming_security, self.smtp_security)
    }
}

/// Gmail synchronizes and sends through its REST API. The IMAP and SMTP
/// endpoints are recorded for completeness and are not contacted.
pub const GMAIL: ProviderServers = ProviderServers {
    mail_protocol: MailProtocol::Gmail,
    incoming_host: "imap.gmail.com",
    incoming_port: 993,
    incoming_security: ConnectionSecurity::Tls,
    smtp_host: "smtp.gmail.com",
    smtp_port: 465,
    smtp_security: ConnectionSecurity::Tls,
};

pub const MICROSOFT: ProviderServers = ProviderServers {
    mail_protocol: MailProtocol::Imap,
    incoming_host: "outlook.office365.com",
    incoming_port: 993,
    incoming_security: ConnectionSecurity::Tls,
    smtp_host: "smtp.office365.com",
    smtp_port: 587,
    smtp_security: ConnectionSecurity::Starttls,
};
