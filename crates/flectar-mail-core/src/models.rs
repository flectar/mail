//! Data models shared by the core and native application. Serialized field
//! names remain stable for persisted JSON and provider-facing payloads.

use serde::{Deserialize, Serialize};

/// The service behind an account. It decides how the user signs in and which
/// provider extras (calendars, aliases) exist; [`MailProtocol`] decides how
/// mail itself is synchronized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// Any standards-based server the user configures by hand.
    Generic,
    Gmail,
    Microsoft,
}

impl Provider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Gmail => "gmail",
            Self::Microsoft => "microsoft",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "generic" => Some(Self::Generic),
            "gmail" => Some(Self::Gmail),
            "microsoft" => Some(Self::Microsoft),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    Password,
    Oauth2,
}

/// The wire protocol an account uses for mail. Every synchronization,
/// mailbox, and action code path dispatches on this value exhaustively, so a
/// protocol can never silently inherit another protocol's behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailProtocol {
    /// The Gmail REST API for synchronization, labels, drafts, and sending.
    Gmail,
    /// IMAP access with SMTP submission.
    Imap,
    /// RFC 8620/8621 JMAP Mail and EmailSubmission.
    Jmap,
    /// POP3 download into folders that exist only on this device, with SMTP
    /// submission.
    Pop3,
    /// No server at all: mail the user imported, kept in folders that exist
    /// only on this device. There is at most one such account, and it has no
    /// address, sign-in, or settings, so it never sends.
    Local,
}

impl MailProtocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gmail => "gmail",
            Self::Imap => "imap",
            Self::Jmap => "jmap",
            Self::Pop3 => "pop3",
            Self::Local => "local",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "gmail" => Some(Self::Gmail),
            "imap" => Some(Self::Imap),
            "jmap" => Some(Self::Jmap),
            "pop3" => Some(Self::Pop3),
            "local" => Some(Self::Local),
            _ => None,
        }
    }

    /// Whether one message can belong to several mailboxes at once (Gmail
    /// labels, JMAP `mailboxIds`). Single-folder protocols store location in
    /// `messages.folder_id` alone.
    pub const fn has_multi_mailbox_messages(self) -> bool {
        match self {
            Self::Gmail | Self::Jmap => true,
            Self::Imap | Self::Pop3 | Self::Local => false,
        }
    }
}

/// The strongest receiver-reported authentication result that the mail UI
/// may present for a message. These values describe message/domain
/// authentication, not whether the content itself is safe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SenderVerification {
    #[default]
    None,
    /// The visible From domain passed DMARC and an aligned DKIM signature.
    Domain,
    /// Microsoft 365's composite authentication passed.
    Microsoft,
    /// The receiver completed BIMI validation without reporting verified
    /// authority evidence.
    Bimi,
    /// BIMI passed and the receiver reports valid authority evidence
    /// (for example a VMC or CMC).
    Brand,
}

impl SenderVerification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Domain => "domain",
            Self::Microsoft => "microsoft",
            Self::Bimi => "bimi",
            Self::Brand => "brand",
        }
    }

    pub fn from_storage(value: &str) -> Self {
        match value {
            "domain" => Self::Domain,
            "microsoft" => Self::Microsoft,
            "bimi" => Self::Bimi,
            "brand" => Self::Brand,
            _ => Self::None,
        }
    }

    pub fn is_verified(self) -> bool {
        self != Self::None
    }
}

impl AuthKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Oauth2 => "oauth2",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "password" => Some(Self::Password),
            "oauth2" => Some(Self::Oauth2),
            _ => None,
        }
    }
}

/// How much existing server history an account downloads in the background.
/// This is deliberately a download boundary, not a retention policy: reducing
/// the window never deletes mail that is already available locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MailHistory {
    OneMonth,
    ThreeMonths,
    SixMonths,
    OneYear,
    All,
}

/// What an account downloads ahead of time within its mail history (#253).
/// Anything not downloaded comes from the server when it's opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadPolicy {
    /// Headers alone: messages download when they're opened.
    Headers,
    /// Headers and messages, with attachments when they're opened.
    #[default]
    Messages,
    /// Messages and their attachments, up to
    /// [`DownloadPolicy::LARGEST_ATTACHMENT`] each, to open offline.
    Everything,
}

impl DownloadPolicy {
    /// Attachments larger than this download when they're opened, as their
    /// provider's sending limits are often about this size.
    pub const LARGEST_ATTACHMENT: i64 = 25 * 1024 * 1024;

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Headers => "headers",
            Self::Messages => "messages",
            Self::Everything => "everything",
        }
    }

    pub fn from_storage(value: &str) -> Option<Self> {
        match value {
            "headers" => Some(Self::Headers),
            "messages" => Some(Self::Messages),
            "everything" => Some(Self::Everything),
            _ => None,
        }
    }
}

impl MailHistory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OneMonth => "oneMonth",
            Self::ThreeMonths => "threeMonths",
            Self::SixMonths => "sixMonths",
            Self::OneYear => "oneYear",
            Self::All => "all",
        }
    }

    pub fn from_storage(value: &str) -> Option<Self> {
        match value {
            "oneMonth" => Some(Self::OneMonth),
            "threeMonths" => Some(Self::ThreeMonths),
            "sixMonths" => Some(Self::SixMonths),
            "oneYear" => Some(Self::OneYear),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    pub const fn months(self) -> Option<u32> {
        match self {
            Self::OneMonth => Some(1),
            Self::ThreeMonths => Some(3),
            Self::SixMonths => Some(6),
            Self::OneYear => Some(12),
            Self::All => None,
        }
    }

    /// UTC calendar-date boundary used by Gmail and IMAP searches. Calendar
    /// months match the user-facing labels better than fixed 30-day buckets.
    pub fn cutoff_date_at(self, timestamp_ms: i64) -> Option<chrono::NaiveDate> {
        let months = self.months()?;
        let at = chrono::DateTime::from_timestamp_millis(timestamp_ms)
            .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH);
        at.date_naive()
            .checked_sub_months(chrono::Months::new(months))
    }

    pub fn cutoff_ms_at(self, timestamp_ms: i64) -> Option<i64> {
        self.cutoff_date_at(timestamp_ms).and_then(|date| {
            date.and_hms_opt(0, 0, 0)
                .map(|value| value.and_utc().timestamp_millis())
        })
    }
}

/// Transport security of one mail connection. Stored explicitly for every
/// account so the server port never decides whether a connection is
/// encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionSecurity {
    /// TLS from the first byte.
    Tls,
    /// A plaintext greeting upgraded with STARTTLS before any credentials
    /// are sent. A server that refuses the upgrade is never used.
    Starttls,
    /// No encryption: the password and every message cross the network in
    /// the clear. Only password sign-in is allowed this way.
    None,
}

impl ConnectionSecurity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tls => "tls",
            Self::Starttls => "starttls",
            Self::None => "none",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tls" => Some(Self::Tls),
            "starttls" => Some(Self::Starttls),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub const fn is_encrypted(self) -> bool {
        match self {
            Self::Tls | Self::Starttls => true,
            Self::None => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailConnectionSettings {
    /// Security of the incoming mail server (IMAP).
    pub incoming_security: ConnectionSecurity,
    pub smtp_security: ConnectionSecurity,
    /// Additional trust anchors for this account only; normal hostname and
    /// validity checks still apply. Contains public certificates, never keys.
    #[serde(default)]
    pub trusted_certificate_pem: String,
    /// The proxy the account's connections go through: the app's, none, or
    /// its own (#250).
    #[serde(default)]
    pub proxy: crate::net::proxy::AccountProxy,
    /// How the incoming server is signed in to (#252).
    #[serde(default)]
    pub incoming_login: LoginMethod,
    /// How the SMTP server is signed in to.
    #[serde(default)]
    pub smtp_login: LoginMethod,
    /// The SMTP server's own username, when it isn't the account's. Its
    /// password is saved apart, or is the account's when none is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smtp_username: Option<String>,
    /// How long to wait for the account's servers, in seconds, when it isn't
    /// the standard [`crate::net::timeouts::STANDARD`] (#252).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_timeout_secs: Option<u32>,
}

impl MailConnectionSettings {
    pub fn new(incoming_security: ConnectionSecurity, smtp_security: ConnectionSecurity) -> Self {
        Self {
            incoming_security,
            smtp_security,
            trusted_certificate_pem: String::new(),
            proxy: crate::net::proxy::AccountProxy::App,
            incoming_login: LoginMethod::Automatic,
            smtp_login: LoginMethod::Automatic,
            smtp_username: None,
            server_timeout_secs: None,
        }
    }

    /// How long to wait for the account's servers before giving up (#252).
    pub fn server_timeout(&self) -> std::time::Duration {
        self.server_timeout_secs
            .map_or(crate::net::timeouts::STANDARD, |seconds| {
                std::time::Duration::from_secs(u64::from(seconds))
            })
    }
}

/// How Flectar Mail signs in to a server with a password (#252).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LoginMethod {
    /// As the server offers, with SASL PLAIN where it's announced and the
    /// plain login command otherwise or when PLAIN fails.
    #[default]
    Automatic,
    /// SASL PLAIN (RFC 4616), as IMAP's AUTHENTICATE PLAIN and POP3's and
    /// SMTP's AUTH PLAIN.
    Plain,
    /// The plain login command, as IMAP's LOGIN, POP3's USER and PASS and
    /// SMTP's AUTH LOGIN.
    Login,
    /// No sign-in, for an SMTP server that relays the mail of its network.
    None,
}

impl LoginMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Plain => "plain",
            Self::Login => "login",
            Self::None => "none",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "automatic" => Some(Self::Automatic),
            "plain" => Some(Self::Plain),
            "login" => Some(Self::Login),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// Who files a copy after an SMTP submission. API transports own their filing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SentCopyPolicy {
    #[default]
    Auto,
    Server,
    Client,
}

impl SentCopyPolicy {
    pub fn should_append(self, provider: Provider) -> bool {
        match self {
            Self::Auto => !matches!(provider, Provider::Gmail | Provider::Microsoft),
            Self::Server => false,
            Self::Client => true,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Server => "server",
            Self::Client => "client",
        }
    }

    pub fn from_storage(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "server" => Some(Self::Server),
            "client" => Some(Self::Client),
            _ => None,
        }
    }
}

/// How an account's messages start: formatted, or as plain text only. Each message can change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageFormat {
    #[default]
    Formatted,
    PlainText,
}

impl MessageFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Formatted => "formatted",
            Self::PlainText => "plainText",
        }
    }

    pub fn from_storage(value: &str) -> Option<Self> {
        match value {
            "formatted" => Some(Self::Formatted),
            "plainText" => Some(Self::PlainText),
            _ => None,
        }
    }
}

/// How long a POP3 account leaves downloaded mail on its server. Mail is
/// removed from the server only once this device has stored it, and every
/// choice but `Always` also removes mail deleted here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeepOnServer {
    #[default]
    Always,
    UntilDeleted,
    SevenDays,
    FourteenDays,
    ThirtyDays,
    UntilDownloaded,
}

impl KeepOnServer {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::UntilDeleted => "untilDeleted",
            Self::SevenDays => "sevenDays",
            Self::FourteenDays => "fourteenDays",
            Self::ThirtyDays => "thirtyDays",
            Self::UntilDownloaded => "untilDownloaded",
        }
    }

    pub fn from_storage(value: &str) -> Option<Self> {
        match value {
            "always" => Some(Self::Always),
            "untilDeleted" => Some(Self::UntilDeleted),
            "sevenDays" => Some(Self::SevenDays),
            "fourteenDays" => Some(Self::FourteenDays),
            "thirtyDays" => Some(Self::ThirtyDays),
            "untilDownloaded" => Some(Self::UntilDownloaded),
            _ => None,
        }
    }

    /// How many days downloaded mail stays on the server, when that's
    /// limited.
    pub const fn days(self) -> Option<u32> {
        match self {
            Self::Always | Self::UntilDeleted => None,
            Self::SevenDays => Some(7),
            Self::FourteenDays => Some(14),
            Self::ThirtyDays => Some(30),
            Self::UntilDownloaded => Some(0),
        }
    }

    /// Whether mail deleted here is removed from the server.
    pub const fn removes_deleted(self) -> bool {
        !matches!(self, Self::Always)
    }
}

/// How often an account checks for new mail (#251).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckInterval {
    /// As often as Settings has every account check.
    #[default]
    App,
    /// Every this many minutes, one of [`CheckInterval::MINUTES`].
    Minutes(u32),
    /// Only when the user asks, and when the app starts.
    Manual,
}

impl CheckInterval {
    /// The intervals an account can check at.
    pub const MINUTES: [u32; 5] = [1, 5, 15, 30, 60];

    /// Whether an account can be set to check this way.
    pub fn is_offered(self) -> bool {
        match self {
            Self::App | Self::Manual => true,
            Self::Minutes(minutes) => Self::MINUTES.contains(&minutes),
        }
    }
}

/// Typed contents of `accounts.settings_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSettings {
    // Missing fields can occur in imported development-era configuration.
    // Apply the same bounded default as a newly created account so an absent
    // value never triggers an unbounded mailbox download.
    #[serde(default = "default_mail_history")]
    pub mail_history: MailHistory,
    /// What this device downloads ahead of time within the mail history
    /// (#253). Each device keeps its own, as it goes with the device's
    /// storage and network.
    #[serde(default)]
    pub download: DownloadPolicy,
    #[serde(default)]
    pub security: crate::mail_security::MailSecurity,
    pub connection: MailConnectionSettings,
    /// The sender identity Flectar should preselect for new messages. The
    /// authenticated account address remains the account/login identity.
    #[serde(default)]
    pub default_sender_email: Option<String>,
    #[serde(default)]
    pub sent_copy_policy: SentCopyPolicy,
    /// POP3 accounts only.
    #[serde(default)]
    pub keep_on_server: KeepOnServer,
    /// How new messages start.
    #[serde(default)]
    pub message_format: MessageFormat,
    /// Replies and forwards to messages that are plain text only start in plain text, also
    /// when new messages start formatted.
    #[serde(default = "plain_text_replies_default")]
    pub plain_text_replies: bool,
    /// New messages ask for a read receipt (RFC 8098), which each message
    /// can still change.
    #[serde(default)]
    pub request_read_receipts: bool,
    /// IMAP accounts only: list and sync the folders the user subscribed to
    /// (RFC 9051 6.3.7), plus the Inbox, a folder for each purpose Flectar
    /// Mail files mail into, and the folders rules file into.
    #[serde(default)]
    pub subscribed_folders_only: bool,
    /// How often the account checks for new mail.
    #[serde(default)]
    pub check_interval: CheckInterval,
    /// IMAP and JMAP accounts: the server tells of new mail as it arrives
    /// (IMAP IDLE, JMAP's event source). Off, the account checks at its
    /// interval instead. An account that checks only when asked doesn't
    /// listen either.
    #[serde(default = "push_default")]
    pub push: bool,
    /// Copies every message written from the account starts with, whichever
    /// sender identity it's from (#252).
    #[serde(default)]
    pub copies: AutomaticCopies,
    /// Copies the messages written from one of the account's sender
    /// identities start with too, by the identity's address in lowercase.
    #[serde(default)]
    pub identity_copies: std::collections::BTreeMap<String, AutomaticCopies>,
    /// The folders of an IMAP account chosen by hand for roles such as Sent,
    /// as their paths by role (#252). A role without one goes to the folder
    /// the server suggests. Always written, empty too: Flectar Mail Sync
    /// keeps the fields a record leaves out as a newer version's.
    #[serde(default)]
    pub folder_roles: std::collections::BTreeMap<String, String>,
}

const fn plain_text_replies_default() -> bool {
    true
}

const fn push_default() -> bool {
    true
}

impl AccountSettings {
    /// Settings of a newly created account. Connection security has no
    /// default: every account records the modes it was configured with.
    pub fn new(connection: MailConnectionSettings) -> Self {
        Self {
            mail_history: default_mail_history(),
            download: DownloadPolicy::default(),
            security: Default::default(),
            connection,
            default_sender_email: None,
            sent_copy_policy: SentCopyPolicy::Auto,
            keep_on_server: KeepOnServer::Always,
            message_format: MessageFormat::Formatted,
            plain_text_replies: plain_text_replies_default(),
            request_read_receipts: false,
            subscribed_folders_only: false,
            check_interval: CheckInterval::App,
            push: push_default(),
            folder_roles: std::collections::BTreeMap::new(),
            copies: AutomaticCopies::default(),
            identity_copies: std::collections::BTreeMap::new(),
        }
    }

    /// The copies a message written from `identity` starts with: the
    /// account's, the identity's own, and the Bcc its provider keeps.
    pub fn copies_for(&self, identity: &SenderIdentity) -> AutomaticCopies {
        let own = self
            .identity_copies
            .get(&identity.email.trim().to_lowercase())
            .cloned()
            .unwrap_or_default();
        let provider = AutomaticCopies {
            cc: Vec::new(),
            bcc: identity.bcc.clone(),
        };
        self.copies.clone().and(&own).and(&provider)
    }
}

const fn default_mail_history() -> MailHistory {
    MailHistory::SixMonths
}

#[cfg(test)]
mod account_settings_tests {
    use super::*;

    const MINIMAL_SETTINGS: &str =
        r#"{"connection":{"incomingSecurity":"tls","smtpSecurity":"starttls"}}"#;

    #[test]
    fn keep_on_server_defaults_to_always_and_survives_serialization() {
        let mut settings: AccountSettings = serde_json::from_str(MINIMAL_SETTINGS).unwrap();
        assert_eq!(settings.keep_on_server, KeepOnServer::Always);
        for keep in [
            KeepOnServer::Always,
            KeepOnServer::UntilDeleted,
            KeepOnServer::SevenDays,
            KeepOnServer::FourteenDays,
            KeepOnServer::ThirtyDays,
            KeepOnServer::UntilDownloaded,
        ] {
            settings.keep_on_server = keep;
            let restored: AccountSettings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(restored.keep_on_server, keep);
            assert_eq!(KeepOnServer::from_storage(keep.as_str()), Some(keep));
        }
        assert!(!KeepOnServer::Always.removes_deleted());
        assert_eq!(KeepOnServer::UntilDeleted.days(), None);
        assert_eq!(KeepOnServer::FourteenDays.days(), Some(14));
        assert_eq!(KeepOnServer::UntilDownloaded.days(), Some(0));
    }

    #[test]
    fn message_format_defaults_and_choices_survive_serialization() {
        let mut settings: AccountSettings = serde_json::from_str(MINIMAL_SETTINGS).unwrap();
        // Accounts from before the choice start formatted, with plain-text replies to plain text.
        assert_eq!(settings.message_format, MessageFormat::Formatted);
        assert!(settings.plain_text_replies);
        let new = AccountSettings::new(settings.connection.clone());
        assert_eq!(
            (new.message_format, new.plain_text_replies),
            (MessageFormat::Formatted, true)
        );
        for format in [MessageFormat::Formatted, MessageFormat::PlainText] {
            settings.message_format = format;
            settings.plain_text_replies = false;
            let restored: AccountSettings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(
                (restored.message_format, restored.plain_text_replies),
                (format, false)
            );
            assert_eq!(MessageFormat::from_storage(format.as_str()), Some(format));
        }
        assert_eq!(MessageFormat::from_storage("html"), None);
    }

    #[test]
    fn sent_copy_defaults_and_overrides_survive_serialization() {
        let mut settings: AccountSettings = serde_json::from_str(MINIMAL_SETTINGS).unwrap();
        assert_eq!(settings.sent_copy_policy, SentCopyPolicy::Auto);
        assert!(!settings.sent_copy_policy.should_append(Provider::Microsoft));
        assert!(settings.sent_copy_policy.should_append(Provider::Generic));
        for policy in [
            SentCopyPolicy::Auto,
            SentCopyPolicy::Server,
            SentCopyPolicy::Client,
        ] {
            settings.sent_copy_policy = policy;
            let restored: AccountSettings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(restored.sent_copy_policy, policy);
            assert_eq!(SentCopyPolicy::from_storage(policy.as_str()), Some(policy));
        }
        assert!(SentCopyPolicy::Client.should_append(Provider::Microsoft));
        assert!(!SentCopyPolicy::Server.should_append(Provider::Generic));
    }

    fn address(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(str::to_owned),
            email: email.to_owned(),
        }
    }

    /// A message from an identity starts with the account's copies, the
    /// identity's own and its provider's Bcc, each address once, and one in
    /// Cc gets no copy in Bcc too (#252).
    #[test]
    fn copies_join_the_account_the_identity_and_its_provider() {
        let mut settings: AccountSettings = serde_json::from_str(MINIMAL_SETTINGS).unwrap();
        assert!(settings.copies.is_empty() && settings.identity_copies.is_empty());
        // Always written, so Flectar Mail Sync carries an emptied list too.
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json["copies"], serde_json::json!({"cc": [], "bcc": []}));
        assert_eq!(json["identityCopies"], serde_json::json!({}));

        settings.copies = AutomaticCopies {
            cc: vec![address(Some("Boss"), "boss@example.com")],
            bcc: vec![address(None, "archive@example.com")],
        };
        settings.identity_copies.insert(
            "sales@example.com".into(),
            AutomaticCopies {
                cc: vec![address(None, "team@example.com")],
                bcc: vec![address(None, "BOSS@example.com")],
            },
        );
        let identity = |email: &str, bcc: Vec<Address>| SenderIdentity {
            account_id: 1,
            email: email.into(),
            display_name: None,
            reply_to_email: None,
            is_primary: false,
            is_provider_default: false,
            verification_status: "accepted".into(),
            last_synced_at: 0,
            source: SenderIdentitySource::Provider,
            delivery_status: SenderDeliveryStatus::Unknown,
            delivery_error: None,
            bcc,
        };
        let sales = identity(
            "Sales@Example.com",
            vec![
                address(None, "ledger@example.com"),
                address(None, "team@example.com"),
            ],
        );
        assert_eq!(
            settings.copies_for(&sales),
            AutomaticCopies {
                cc: vec![
                    address(Some("Boss"), "boss@example.com"),
                    address(None, "team@example.com"),
                ],
                bcc: vec![
                    address(None, "archive@example.com"),
                    address(None, "ledger@example.com"),
                ],
            }
        );
        // Another identity gets the account's alone.
        assert_eq!(
            settings.copies_for(&identity("me@example.com", Vec::new())),
            settings.copies
        );
        let restored: AccountSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
    }

    /// An account downloads messages ahead of time unless it's set otherwise,
    /// and the choice survives storage (#253).
    #[test]
    fn what_an_account_downloads_starts_at_messages_and_is_kept() {
        let mut settings: AccountSettings = serde_json::from_str(MINIMAL_SETTINGS).unwrap();
        assert_eq!(settings.download, DownloadPolicy::Messages);
        for download in [
            DownloadPolicy::Headers,
            DownloadPolicy::Messages,
            DownloadPolicy::Everything,
        ] {
            settings.download = download;
            let restored: AccountSettings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(restored.download, download);
            assert_eq!(
                DownloadPolicy::from_storage(download.as_str()),
                Some(download)
            );
        }
        assert_eq!(DownloadPolicy::from_storage("bodies"), None);
    }

    #[test]
    fn subscribed_folders_only_starts_off_and_survives_serialization() {
        let mut settings: AccountSettings = serde_json::from_str(MINIMAL_SETTINGS).unwrap();
        assert!(!settings.subscribed_folders_only);
        settings.subscribed_folders_only = true;
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""subscribedFoldersOnly":true"#));
        let restored: AccountSettings = serde_json::from_str(&json).unwrap();
        assert!(restored.subscribed_folders_only);
    }

    #[test]
    fn accounts_check_as_settings_say_with_push_until_changed() {
        let mut settings: AccountSettings = serde_json::from_str(MINIMAL_SETTINGS).unwrap();
        assert_eq!(settings.check_interval, CheckInterval::App);
        assert!(settings.push);
        for (interval, stored) in [
            (
                CheckInterval::Minutes(30),
                r#""checkInterval":{"minutes":30}"#,
            ),
            (CheckInterval::Manual, r#""checkInterval":"manual""#),
            (CheckInterval::App, r#""checkInterval":"app""#),
        ] {
            settings.check_interval = interval;
            settings.push = false;
            let json = serde_json::to_string(&settings).unwrap();
            assert!(json.contains(stored), "{json}");
            assert!(json.contains(r#""push":false"#), "{json}");
            let restored: AccountSettings = serde_json::from_str(&json).unwrap();
            assert_eq!(restored.check_interval, interval);
            assert!(!restored.push);
        }
        for minutes in CheckInterval::MINUTES {
            assert!(CheckInterval::Minutes(minutes).is_offered());
        }
        assert!(!CheckInterval::Minutes(0).is_offered());
        assert!(!CheckInterval::Minutes(7).is_offered());
    }

    #[test]
    fn new_and_partially_stored_settings_share_the_bounded_default() {
        let connection =
            MailConnectionSettings::new(ConnectionSecurity::Tls, ConnectionSecurity::Starttls);
        let created = AccountSettings::new(connection.clone());
        assert_eq!(created.mail_history, MailHistory::SixMonths);
        let stored: AccountSettings = serde_json::from_str(MINIMAL_SETTINGS).unwrap();
        assert_eq!(stored, created);
        assert_eq!(stored.connection.trusted_certificate_pem, "");
    }

    #[test]
    fn connection_security_is_never_inferred() {
        assert!(serde_json::from_str::<AccountSettings>("{}").is_err());
        assert!(
            serde_json::from_str::<AccountSettings>(
                r#"{"connection":{"incomingSecurity":"auto","smtpSecurity":"tls"}}"#
            )
            .is_err()
        );
        assert_eq!(ConnectionSecurity::parse("auto"), None);
        for security in [
            ConnectionSecurity::Tls,
            ConnectionSecurity::Starttls,
            ConnectionSecurity::None,
        ] {
            assert_eq!(ConnectionSecurity::parse(security.as_str()), Some(security));
        }
    }

    #[test]
    fn calendar_month_cutoffs_are_stable_at_month_ends() {
        let at = chrono::NaiveDate::from_ymd_opt(2026, 8, 31)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        assert_eq!(
            MailHistory::SixMonths.cutoff_date_at(at),
            Some(chrono::NaiveDate::from_ymd_opt(2026, 2, 28).unwrap())
        );
        assert_eq!(MailHistory::All.cutoff_ms_at(at), None);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: i64,
    pub email: String,
    /// The owner's name, as the account's provider gave it when the account
    /// signed in with it.
    pub display_name: Option<String>,
    /// The name the user gave the account, such as "Work". Lists of accounts
    /// show it instead of the address; people it writes to never see it.
    #[serde(default)]
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub provider: Provider,
    pub auth_kind: AuthKind,
    pub mail_protocol: MailProtocol,
    pub sync_state: String,
    pub sync_error: Option<String>,
    /// When sync asks the server again, in milliseconds since the epoch,
    /// while the account waits because the server asked it to (#251).
    #[serde(default)]
    pub sync_retry_at: Option<i64>,
    /// Whether the provider currently permits creating a root mailbox.
    #[serde(default = "default_true")]
    pub can_create_top_level_mailbox: bool,
    /// The days a POP3 server keeps downloaded mail before removing it on
    /// its own, as it announced at the last sign-in (RFC 2449 EXPIRE). Zero
    /// removes it when the session ends. `None` when it keeps mail or hasn't
    /// said.
    #[serde(default)]
    pub server_keeps_mail_days: Option<u32>,
    /// When the last check that brought the Inbox up to date started, in
    /// milliseconds since the epoch: the account last checked for new mail
    /// then. `None` before its first complete check.
    #[serde(default)]
    pub caught_up_at: Option<i64>,
}

impl Account {
    /// What lists of accounts call it: the name the user gave it, else the
    /// provider's name for its owner, else its address.
    pub fn label(&self) -> &str {
        account_label(
            self.name.as_deref(),
            self.display_name.as_deref(),
            &self.email,
        )
    }
}

/// The name of an account in lists of accounts: `name`, else
/// `display_name`, else `email`, skipping blank names.
pub fn account_label<'a>(
    name: Option<&'a str>,
    display_name: Option<&'a str>,
    email: &'a str,
) -> &'a str {
    [name, display_name]
        .into_iter()
        .flatten()
        .find(|name| !name.trim().is_empty())
        .unwrap_or(email)
}

/// A name the user typed for an account, as stored: on one line, without
/// surrounding spaces. Blank means no name.
pub fn clean_account_name(name: &str) -> Option<String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    (!name.is_empty()).then_some(name)
}

/// Mail imported into folders that exist only on this device. It belongs to
/// an account of protocol [`MailProtocol::Local`], which account lists leave
/// out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalMail {
    pub account_id: i64,
    pub messages: u64,
    pub bytes: u64,
}

/// Full account row including server config; internal to the native application.
#[derive(Debug, Clone)]
pub struct AccountConfig {
    /// 0 while an account is being set up, before it's saved.
    pub id: i64,
    pub email: String,
    pub display_name: Option<String>,
    /// See [`Account::name`].
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub provider: Provider,
    pub auth_kind: AuthKind,
    pub mail_protocol: MailProtocol,
    pub username: String,
    /// HTTPS origin used to discover `/.well-known/jmap`.
    pub jmap_url: String,
    /// Selected JMAP Mail account id advertised by the Session resource.
    pub jmap_account_id: Option<String>,
    /// Incoming mail server (IMAP). Unused by API protocols.
    pub incoming_host: String,
    pub incoming_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub settings: AccountSettings,
}

impl AccountConfig {
    /// The username the SMTP server is signed in with.
    pub fn smtp_username(&self) -> &str {
        self.settings
            .connection
            .smtp_username
            .as_deref()
            .unwrap_or(&self.username)
    }

    /// Whether the account sends mail. API accounts send through their API,
    /// IMAP and POP3 accounts through an SMTP server unless they only
    /// receive (#252).
    pub fn sends_mail(&self) -> bool {
        match self.mail_protocol {
            MailProtocol::Imap | MailProtocol::Pop3 => !self.smtp_host.is_empty(),
            MailProtocol::Gmail | MailProtocol::Jmap => true,
            MailProtocol::Local => false,
        }
    }

    /// Whose proxy settings the account's connections follow: its own once
    /// it's saved, the app's while it's being set up.
    pub fn proxy_owner(&self) -> crate::net::proxy::Owner {
        if self.id > 0 {
            crate::net::proxy::Owner::Account(self.id)
        } else {
            crate::net::proxy::Owner::App
        }
    }
}

/// Portable account connection metadata used by backup/restore. Credentials,
/// cached mail, and local database ids are intentionally not represented.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PortableAccountConfig {
    pub email: String,
    pub display_name: Option<String>,
    /// See [`Account::name`].
    #[serde(default)]
    pub name: Option<String>,
    pub provider: Provider,
    pub auth_kind: AuthKind,
    pub mail_protocol: MailProtocol,
    pub username: String,
    #[serde(default)]
    pub jmap_url: String,
    pub incoming_host: String,
    pub incoming_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub settings: AccountSettings,
}

impl From<&AccountConfig> for PortableAccountConfig {
    fn from(config: &AccountConfig) -> Self {
        Self {
            email: config.email.clone(),
            display_name: config.display_name.clone(),
            name: config.name.clone(),
            provider: config.provider,
            auth_kind: config.auth_kind,
            mail_protocol: config.mail_protocol,
            username: config.username.clone(),
            jmap_url: config.jmap_url.clone(),
            incoming_host: config.incoming_host.clone(),
            incoming_port: config.incoming_port,
            smtp_host: config.smtp_host.clone(),
            smtp_port: config.smtp_port,
            settings: {
                let mut settings = config.settings.clone();
                // A proxy belongs to the network a device is on, so an
                // account moving to another device leaves its own behind.
                settings.connection.proxy = crate::net::proxy::AccountProxy::App;
                settings
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

/// A provider-authorized address that may appear in an outgoing From header.
/// Provider discovery is deliberately separate from the authenticated account
/// identity: aliases must never replace the login/synchronization address.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SenderIdentity {
    pub account_id: i64,
    pub email: String,
    pub display_name: Option<String>,
    pub reply_to_email: Option<String>,
    pub is_primary: bool,
    pub is_provider_default: bool,
    /// Gmail uses `accepted` / `pending`; provider-neutral primary identities
    /// are recorded as `accepted` as well.
    pub verification_status: String,
    pub last_synced_at: i64,
    pub source: SenderIdentitySource,
    /// Outcome of the most recent submission from this address.
    pub delivery_status: SenderDeliveryStatus,
    /// Server reply that rejected the most recent submission, if any.
    pub delivery_error: Option<String>,
    /// The Bcc the provider keeps for this identity, which RFC 8621 §6 asks
    /// clients to add to the mail they write from it (#252).
    #[serde(default)]
    pub bcc: Vec<Address>,
}

/// Addresses a message gets copies to unless its writer removes them: those
/// the Cc and Bcc of mail written from an account or sender identity start
/// with (#252).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticCopies {
    #[serde(default)]
    pub cc: Vec<Address>,
    #[serde(default)]
    pub bcc: Vec<Address>,
}

impl AutomaticCopies {
    /// Addresses each list takes at most.
    pub const MAX_ADDRESSES: usize = 50;

    pub fn is_empty(&self) -> bool {
        self.cc.is_empty() && self.bcc.is_empty()
    }

    /// These and then `more`, each address once: an address in Cc gets no
    /// copy in Bcc too.
    pub fn and(mut self, more: &Self) -> Self {
        let mut seen = std::collections::HashSet::new();
        let mut keep = |address: &Address| seen.insert(address.email.trim().to_lowercase());
        let cc = self.cc.into_iter().chain(more.cc.iter().cloned());
        self.cc = cc.filter(|address| keep(address)).collect();
        let bcc = self.bcc.into_iter().chain(more.bcc.iter().cloned());
        self.bcc = bcc.filter(|address| keep(address)).collect();
        self
    }
}

/// Where a sender identity came from.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum SenderIdentitySource {
    /// Mirrors the authenticated address or an authoritative provider
    /// endpoint (Gmail SendAs, JMAP Identity/get). Refreshes replace it.
    #[default]
    Provider,
    /// Declared by the user for a server that cannot list its permitted From
    /// addresses (plain SMTP, Microsoft). Refreshes keep it.
    Manual,
}

impl SenderIdentitySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Manual => "manual",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "manual" => Self::Manual,
            _ => Self::Provider,
        }
    }
}

/// What the last real submission taught us about an address. SMTP has no
/// standard query for permitted senders, so this is the only evidence for
/// manual identities.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum SenderDeliveryStatus {
    #[default]
    Unknown,
    Confirmed,
    Rejected,
}

impl SenderDeliveryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "confirmed" => Self::Confirmed,
            "rejected" => Self::Rejected,
            _ => Self::Unknown,
        }
    }
}

impl SenderIdentity {
    pub fn is_verified(&self) -> bool {
        self.is_primary || self.verification_status == "accepted"
    }

    /// RFC 8621 §6: an identity whose local part is exactly `*` permits any
    /// address at its domain. It is an authorization, not a sendable address.
    pub fn is_wildcard(&self) -> bool {
        self.email
            .split_once('@')
            .is_some_and(|(local, _)| local == "*")
    }

    /// Whether the identity can be offered as a concrete From address.
    pub fn is_selectable(&self) -> bool {
        self.is_verified() && !self.is_wildcard()
    }

    /// Whether this identity authorizes `email` as a From address, either
    /// exactly or through a wildcard domain.
    pub fn authorizes(&self, email: &str) -> bool {
        let email = email.trim();
        if self.email.eq_ignore_ascii_case(email) {
            return !self.is_wildcard();
        }
        let (Some((local, domain)), Some((candidate_local, candidate_domain))) =
            (self.email.rsplit_once('@'), email.rsplit_once('@'))
        else {
            return false;
        };
        local == "*"
            && !candidate_local.is_empty()
            && candidate_local != "*"
            && domain.eq_ignore_ascii_case(candidate_domain)
    }

    pub fn address(&self) -> Address {
        Address {
            name: self.display_name.clone(),
            email: self.email.clone(),
        }
    }

    pub fn reply_to(&self) -> Option<Address> {
        self.reply_to_email
            .as_deref()
            .map(str::trim)
            .filter(|email| !email.is_empty() && !email.eq_ignore_ascii_case(&self.email))
            .map(|email| Address {
                name: None,
                email: email.to_owned(),
            })
    }
}

/// A contact matched by search suggestions, with its interaction affinity
/// (send_count*3 + recv_count) so the UI can show how well-known it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactSuggestion {
    pub name: Option<String>,
    pub email: String,
    pub interactions: i64,
}

/// One email address or phone number of a saved contact. `key` is the
/// property's position in the vCard revision the record was read from, so an
/// edit can replace exactly that property; values added in the editor have an
/// empty key.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContactValue {
    pub key: String,
    pub value: String,
    pub label: String,
    /// 1 is most preferred. 100 means no stated preference.
    pub preference: u8,
}

impl ContactValue {
    pub const NO_PREFERENCE: u8 = 100;
}

/// Where a saved contact stands relative to its address book.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContactSync {
    /// Stored only on this device.
    #[default]
    Local,
    /// Stored only on this device because its address book was removed
    /// before local changes reached the server.
    Detached,
    Synced,
    /// Saved locally and waiting to upload.
    Pending,
    /// The server changed the contact while a local change was pending. Both
    /// versions are kept until the user chooses one.
    Conflict,
    /// The address book no longer exists on the server.
    Unavailable,
    /// The server rejected or could not provide this contact.
    Failed(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContactDetails {
    /// Changes whenever the stored contact changes. Saving or resolving a
    /// conflict from an older revision is rejected.
    pub revision: i64,
    pub emails: Vec<ContactValue>,
    pub phones: Vec<ContactValue>,
    /// Address book name and account, or None for a contact stored only on
    /// this device.
    pub source: Option<String>,
    pub read_only: bool,
    pub sync: ContactSync,
    pub photo_hash: Option<String>,
}

/// A complete address-book entry. Mail sync owns the interaction metadata;
/// the remaining fields can be edited directly from the contacts workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContactRecord {
    #[serde(default)]
    pub details: ContactDetails,
    /// Positive for saved contacts. A mail-derived suggestion uses the
    /// negated id of its mail history row, and 0 is a contact not yet saved.
    pub id: i64,
    pub name: String,
    /// Most preferred entry of `details.emails`, shown in lists. Saving reads
    /// the value lists, not this field.
    pub email: String,
    /// Most preferred entry of `details.phones`, shown in lists.
    pub phone: String,
    pub company: String,
    pub job_title: String,
    pub website: String,
    pub birthday: String,
    pub postal_address: String,
    pub notes: String,
    pub tags: String,
    pub is_favorite: bool,
    pub interactions: i64,
    pub last_interacted: Option<i64>,
    /// Accounts that discovered this contact through sent or received mail.
    #[serde(default)]
    pub account_ids: Vec<i64>,
    /// True for a saved address-book entry, whether managed locally or backed
    /// by CardDAV. False identifies a mail-derived suggestion.
    #[serde(default)]
    pub is_managed: bool,
}

impl ContactRecord {
    /// Derive `email` and `phone` from the value lists. Ties keep the order
    /// the values appear in.
    pub fn refresh_primary_values(&mut self) {
        let primary = |values: &[ContactValue]| {
            values
                .iter()
                .min_by_key(|value| value.preference)
                .map(|value| value.value.trim().to_owned())
                .unwrap_or_default()
        };
        self.email = primary(&self.details.emails).to_lowercase();
        self.phone = primary(&self.details.phones);
    }
}

/// One bounded page for the contacts workspace. Counts are scalar metadata;
/// `records` never contains more than the requested page size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactRecordCursor {
    pub is_favorite: bool,
    pub sort_name: String,
    pub email: String,
    pub id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactRecordPage {
    pub records: Vec<ContactRecord>,
    pub next_cursor: Option<ContactRecordCursor>,
    /// Saved/manual and CardDAV-backed address-book entries.
    pub total_count: usize,
    pub favorite_count: usize,
    /// Mail-derived people that have not been promoted to the address book.
    pub suggestion_count: usize,
    pub account_counts: Vec<(i64, usize)>,
}

/// A CardDAV address-book collection discovered for one mail account.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AddressBook {
    pub id: i64,
    pub account_id: i64,
    pub url: String,
    pub display_name: Option<String>,
    pub read_only: bool,
    pub enabled: bool,
    pub is_default: bool,
    pub last_synced_at: Option<i64>,
}

/// Per-account CardDAV connection shown by account settings.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CardDavConnection {
    pub account_id: i64,
    pub base_url: String,
    pub username: String,
    pub enabled: bool,
    pub last_error: Option<String>,
}

/// Why a contacts file can't be imported at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContactFileProblem {
    /// Larger than [`crate::carddav::import::MAX_FILE_BYTES`].
    TooLarge,
    /// More cards than [`crate::carddav::import::MAX_CARDS`].
    TooManyContacts,
}

/// What importing a contacts file would do, or why it can't.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContactImportPreview {
    /// Contacts not saved yet.
    pub new: usize,
    /// Contacts already saved, with the same UID or an email address in
    /// common, or repeated in the file.
    pub duplicates: usize,
    /// Cards with nothing to call the contact by, or that are malformed.
    pub unreadable: usize,
    pub problem: Option<ContactFileProblem>,
}

/// What importing does with a contact that is already saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportedDuplicates {
    /// Keep the saved contact as it is.
    Skip,
    /// Add to the saved contact what it doesn't have yet: new addresses and
    /// numbers, and fields it leaves empty.
    Merge,
    /// Save the imported card as another contact.
    Add,
}

/// What importing a contacts file did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContactImportOutcome {
    pub added: usize,
    pub merged: usize,
    /// Duplicates left as they were, and saved contacts that can't change,
    /// such as those of a read-only address book.
    pub skipped: usize,
    pub unreadable: usize,
}

// Deliberately no `Debug`: it would print the CardDAV app password.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectCardDavArgs {
    pub account_id: i64,
    pub url: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    Inbox,
    Starred,
    Snoozed,
    Sent,
    /// Drafts waiting to be sent at a time the user chose.
    Scheduled,
    Drafts,
    Done,
    Trash,
    Spam,
    All,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub id: i64,
    pub account_id: i64,
    /// `None` for local mail, which has no address.
    pub account_email: Option<String>,
    /// Verified addresses owned by this account, including send-as aliases.
    pub account_addresses: Vec<String>,
    pub subject: String,
    pub snippet: String,
    pub participants: Vec<Address>,
    pub last_message_at: i64,
    pub message_count: i64,
    pub unread_count: i64,
    pub is_starred: bool,
    pub has_attachments: bool,
    pub has_replied: bool,
    /// A message of the conversation was sent with high importance.
    pub high_importance: bool,
    pub snoozed_until: Option<i64>,
    /// When the user asked to be reminded unless someone replies.
    pub reply_reminder_at: Option<i64>,
    /// That time came with no reply: the conversation waits for one.
    pub reply_reminder_due: bool,
    /// Ids of labels present on any message in the thread.
    pub labels: Vec<i64>,
    /// The one-time code the newest message asks its reader to enter.
    pub verification_code: Option<String>,
    /// The thread's first file, from its newest message that has one.
    /// Inline images are not files.
    pub first_attachment: Option<AttachmentMeta>,
    /// How many files the thread holds.
    pub attachment_total: i64,
}

/// Whether mail lists show conversations or each message on its own, like
/// JMAP's `Email/query` with and without `collapseThreads`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailGrouping {
    #[default]
    Conversations,
    Messages,
}

/// One message as a mail-list row, for lists that do not group messages into
/// conversations. Snooze and tab routing belong to the message's conversation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSummary {
    pub id: i64,
    pub thread_id: i64,
    pub account_id: i64,
    /// `None` for local mail, which has no address.
    pub account_email: Option<String>,
    /// Verified addresses owned by this account, including send-as aliases.
    pub account_addresses: Vec<String>,
    pub subject: String,
    pub snippet: String,
    pub from: Address,
    pub to: Vec<Address>,
    pub date: i64,
    pub is_read: bool,
    pub is_starred: bool,
    pub is_draft: bool,
    pub is_outgoing: bool,
    pub has_attachments: bool,
    /// A sent reply refers to this message.
    pub has_replied: bool,
    /// The message was sent with high importance.
    pub high_importance: bool,
    /// When the message's conversation is snoozed until.
    pub snoozed_until: Option<i64>,
    /// When the user asked to be reminded unless someone replies.
    pub reply_reminder_at: Option<i64>,
    /// That time came with no reply: the conversation waits for one.
    pub reply_reminder_due: bool,
    pub labels: Vec<i64>,
    pub verification_code: Option<String>,
    /// The message's first file. Inline images are not files.
    pub first_attachment: Option<AttachmentMeta>,
    pub attachment_total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Label {
    pub id: i64,
    pub name: String,
    /// Hex swatch shown on chips, e.g. "#6b7280".
    pub color: String,
    /// IMAP keyword atom this label maps to on the server.
    pub keyword: String,
    pub position: i64,
    /// Owning account for a provider/account label. `None` means this is a
    /// local global label or an automatic category.
    #[serde(default)]
    pub owner_account_id: Option<i64>,
    /// System auto-category (Marketing/News/Social/Pitch): classified locally
    /// at sync time, never pushed to IMAP, not deletable.
    #[serde(default)]
    pub is_auto: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    pub id: i64,
    pub filename: Option<String>,
    pub mime_type: Option<String>,
    pub size: Option<i64>,
    pub is_inline: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDetail {
    pub id: i64,
    pub thread_id: i64,
    pub account_id: i64,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub date: i64,
    pub is_read: bool,
    pub is_starred: bool,
    pub is_draft: bool,
    pub is_outgoing: bool,
    pub snippet: String,
    pub body_state: String,
    pub text_body: Option<String>,
    pub html_body: Option<String>,
    /// Local-only note appended by an AI automation. It is displayed after the
    /// immutable received body and is never written back to the IMAP message.
    pub automation_note: Option<String>,
    pub attachments: Vec<AttachmentMeta>,
    pub list_unsubscribe: Option<String>,
    /// Raw List-Unsubscribe-Post header; "List-Unsubscribe=One-Click" marks the
    /// HTTPS URI in `list_unsubscribe` as an RFC 8058 one-click endpoint.
    pub list_unsubscribe_post: Option<String>,
    /// Transmitting party (Sender:, Return-Path or DKIM d=) when its domain
    /// doesn't align with `from` - mailing lists, ESPs, spoofed From:.
    /// Email address or bare domain; the UI shows it as "via <domain>".
    pub via: Option<String>,
    /// Strongest trusted sender-authentication result available to the UI.
    pub sender_verification: SenderVerification,
    /// Delivery state for a local draft that has been queued to send:
    /// `"queued"` while its send is still in flight, `"failed"` once a dispatch
    /// attempt errored (e.g. the account needs re-authentication, or SMTP
    /// rejected it). `None` for received mail and for drafts never sent. Lets the
    /// UI keep a failed send visible with an error instead of it silently
    /// reverting to a plain draft. `"uncertain"` when it may have gone out,
    /// the mail server never having confirmed it.
    pub send_state: Option<String>,
    /// The last delivery error when `send_state` is `"failed"`.
    pub send_error: Option<String>,
    /// It is in its account's spam folder, as a folder or a label.
    pub in_spam: bool,
    /// Its sender is blocked: their mail goes to Spam.
    pub sender_blocked: bool,
    /// The read receipt it asks for, when one may be sent and nothing has
    /// answered it yet.
    pub receipt_request: Option<ReceiptRequest>,
    /// For a message one sent, the read receipts that came back for it,
    /// earliest first.
    pub returned_receipts: Vec<ReturnedReceipt>,
}

/// A read receipt a received message asks for (RFC 8098), which this client
/// may send. Mail in Spam, drafts, sent and automated mail, and requests that
/// go back to the account's own addresses, ask for none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptRequest {
    /// Where the receipt would go.
    pub to: Vec<Address>,
    /// It may only go with the user's agreement: RFC 8098 section 2.1 wants
    /// it for this request, or the receiving server didn't authenticate the
    /// sender (section 6.1).
    pub needs_confirmation: bool,
}

/// What a read receipt that came back says happened to the message (RFC 8098
/// section 3.2.6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Disposition {
    /// Shown to someone reading the recipient's mailbox, which doesn't mean
    /// it was read or understood.
    Displayed,
    /// Deleted, seen or not.
    Deleted,
    /// Printed, faxed or forwarded without being shown.
    Dispatched,
    /// Handled by rules or a server without being shown.
    Processed,
}

impl Disposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Displayed => "displayed",
            Self::Deleted => "deleted",
            Self::Dispatched => "dispatched",
            Self::Processed => "processed",
        }
    }

    /// A disposition type, in any case. The types RFC 3798 also had,
    /// "denied" and "failed", say nothing happened to the message.
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Displayed,
            Self::Deleted,
            Self::Dispatched,
            Self::Processed,
        ]
        .into_iter()
        .find(|disposition| value.eq_ignore_ascii_case(disposition.as_str()))
    }
}

/// A read receipt that came back for a message one sent: whom it's from and
/// what it says happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReturnedReceipt {
    /// The recipient, named as the message addressed them where it did.
    pub recipient: Address,
    pub disposition: Disposition,
    /// When the receipt was written, in milliseconds since the Unix epoch.
    pub reported_at: i64,
}

/// How a read receipt request is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptAnswer {
    /// Send it: the user agreed to this one, or Settings says to always
    /// send and the request needs no confirmation.
    Send { user_agreed: bool },
    /// Don't send one, and don't ask again.
    Decline,
}

/// Automatic replies an account's server sends while the user is away
/// (#239), such as Gmail's vacation responder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoReply {
    pub enabled: bool,
    pub subject: String,
    pub text: String,
    /// When replies start and end, in milliseconds since the epoch. Mail
    /// that arrives before or after gets none.
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
    /// Only senders in the user's contacts get a reply.
    pub contacts_only: bool,
    /// Only senders of the account's own domain get a reply, which Google
    /// Workspace offers.
    pub domain_only: bool,
}

impl AutoReply {
    /// Whether replies go out at `now`, or will once their first day comes.
    pub fn on_at(&self, now: i64) -> bool {
        self.enabled && self.ends_at.is_none_or(|ends_at| ends_at > now)
    }
}

/// What saving automatic replies did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoReplySaved {
    /// The replies as the server keeps them now.
    Done(AutoReply),
    /// Nothing changed: the user must allow this first.
    Ask(AutoReplyQuestion),
}

/// What saving automatic replies would change on a server that runs Sieve
/// scripts, beyond the replies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoReplyQuestion {
    /// The server runs the user's own filter script, named here. The replies
    /// would run first, then that script, unchanged.
    RunBefore(String),
    /// The app's script on the server was changed elsewhere since the app
    /// wrote it. Saving replaces those changes.
    ReplaceChanges,
}

/// Something an AI agent did through the local MCP server, without message
/// content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentActivity {
    pub id: i64,
    pub occurred_at: i64,
    /// The name the agent's app gave itself.
    pub agent: String,
    pub tool: String,
    pub account_id: Option<i64>,
    pub folder_id: Option<i64>,
    /// How many accounts, folders, messages, contacts or events it touched.
    pub items: i64,
    pub outcome: AgentOutcome,
    /// The user can still undo what it changed.
    pub undoable: bool,
    /// The user undid what it changed.
    pub undone: bool,
}

/// How an agent's request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentOutcome {
    Done,
    /// It asked for something outside its access.
    Refused,
    Failed,
}

impl AgentOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn from_column(value: &str) -> Self {
        match value {
            "done" => Self::Done,
            "refused" => Self::Refused,
            _ => Self::Failed,
        }
    }
}

/// A sender whose mail goes to Spam.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedSender {
    /// Lowercase.
    pub address: String,
    pub blocked_at: i64,
}

/// A sender, for the confirmation shown before blocking them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockOffer {
    /// Lowercase.
    pub address: String,
    /// How many of their messages are in an inbox now, which blocking moves
    /// to Spam.
    pub inbox_count: usize,
}

/// What blocking a sender did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockResult {
    pub address: String,
    /// The moves of their inbox mail to Spam, for Undo.
    pub action_ids: Vec<i64>,
    /// How many messages moved.
    pub moved: usize,
}

/// A rule that sorts new mail as it arrives in an Inbox (#238). Rules are
/// tried top to bottom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailRule {
    /// 0 for a rule not saved yet.
    pub id: i64,
    /// The account whose mail it sorts, or None for every account's.
    pub account_id: Option<i64>,
    pub name: String,
    pub enabled: bool,
    /// Whether mail must meet every condition, or any one.
    pub match_all: bool,
    pub conditions: Vec<RuleCondition>,
    pub actions: Vec<RuleAction>,
    /// Whether later rules leave alone the mail this one matched. Filing
    /// mail away (move, archive, trash, junk) always ends the rules.
    pub stop: bool,
}

/// What one condition of a [`MailRule`] tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleCondition {
    pub field: RuleField,
    /// The header a [`RuleField::Header`] condition looks at, such as
    /// "X-Spam-Flag".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub header: String,
    pub op: RuleOp,
    /// The text to look for, the pattern, or the size in KB.
    #[serde(default)]
    pub value: String,
    /// Whether the condition holds when the test fails, as in "doesn't
    /// contain".
    #[serde(default)]
    pub negate: bool,
}

/// The part of a message a [`RuleCondition`] tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleField {
    /// The sender's address or name.
    From,
    To,
    Cc,
    /// Any recipient in To or Cc.
    ToOrCc,
    Subject,
    /// The List-Id of mailing list mail.
    MailingList,
    /// The message's size in KB.
    Size,
    /// A header the condition names.
    Header,
    /// The message's text.
    Body,
    /// Whether files are attached.
    Attachments,
}

/// How a [`RuleCondition`] compares. Text is compared without regard to
/// case, an address by its email and by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleOp {
    Contains,
    Is,
    StartsWith,
    EndsWith,
    /// A regular expression, found anywhere in the text.
    Matches,
    /// The field is there at all, as mailing list mail has a List-Id and
    /// some mail has files attached.
    Exists,
    /// A size larger than the value.
    Over,
    /// A size smaller than the value.
    Under,
}

/// What a [`MailRule`] does with mail it matches. Mail is marked and
/// labeled first, then copied, then filed away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RuleAction {
    /// Into one of the rule's account's folders.
    Move {
        folder: i64,
    },
    /// Into one more folder, where a message can be in several (JMAP).
    Copy {
        folder: i64,
    },
    Label {
        label: i64,
    },
    MarkRead,
    Star,
    Archive,
    Trash,
    Junk,
    /// Under a tab of the Inbox: "important", "other", or "label:<id>" for
    /// an automatic category. The conversation stays there.
    ShowIn {
        tab: String,
    },
}

/// Where an account's rules run (#238), as last checked. The rules its
/// server runs work while the app is closed. The app tries every rule on
/// new mail while it's open, whatever the server ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerRules {
    pub account_id: i64,
    pub state: ServerRulesState,
    /// The rules the server runs, in order.
    pub on_server: Vec<i64>,
    /// The account's other rules, which run in the app alone, and why.
    pub here: Vec<RuleHere>,
    /// Milliseconds since the epoch.
    pub checked_at: i64,
}

/// What an account's server does with its rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "camelCase")]
pub enum ServerRulesState {
    /// The server can't run rules.
    Unavailable,
    /// The server runs the rules it can.
    Runs,
    /// The server runs rules another device put there, so this device's
    /// rules run in the app until the user replaces those.
    Others,
    /// The server runs the user's own filter script, named here. Rules run
    /// on the server once the user lets the app's script run before it.
    RunBefore(String),
    /// Flectar Mail's script on the server was changed elsewhere. Rules run
    /// there again once the user lets the app replace those changes.
    Edited,
    /// The server can't take the rules, for this reason.
    Failed(String),
}

/// A rule that runs in the app alone, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleHere {
    pub rule_id: i64,
    pub why: RuleHereWhy,
}

/// Why a rule runs in the app alone, though its server runs rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleHereWhy {
    /// It looks for a pattern.
    Pattern,
    /// It asks whether files are attached.
    Attachments,
    /// It compares text beyond ASCII, which the server compares with case.
    Case,
    /// It compares a whole name, or the start or end of one.
    Name,
    /// The server can't name one of its folders, or the folder is gone.
    Folder,
    /// One of its labels has a name the server can't keep.
    Label,
    /// The server lacks something the rule needs.
    Unsupported,
    /// A rule before it runs in the app, and server rules run first.
    After,
}

/// The user's go-ahead for a [`ServerRulesState`] that waits for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerRulesAnswer {
    /// Run the app's script before the user's own one, named here.
    RunBefore(String),
    /// Replace the changes made to the app's script elsewhere.
    ReplaceChanges,
    /// Replace the rules another device put on the server.
    ReplaceOthers,
}

/// How the user can leave a mailing list, for the confirmation shown before
/// anything is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UnsubscribeOffer {
    /// The sender's signed RFC 8058 request to a web address on this host.
    OneClick { host: String },
    /// An RFC 2369 request by email, sent from `from` to `to`.
    Mailto {
        from: String,
        to: String,
        subject: String,
        body: String,
    },
    /// Only the list's own web page, for the browser.
    Website { url: String },
}

/// What actually happened when the user asked to unsubscribe - the UI's toast
/// must only claim "unsubscribed" for outcomes that really completed.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UnsubscribeOutcome {
    /// RFC 8058 one-click POST returned 2xx: the unsubscribe is done.
    OneClick,
    /// An unsubscribe request email was sent to the list's mailto: address.
    MailtoSent,
    /// No one-click endpoint (or the POST failed); the user must finish in the
    /// browser at this URL.
    #[serde(rename_all = "camelCase")]
    NeedsBrowser { url: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDetail {
    pub thread: ThreadSummary,
    pub messages: Vec<MessageDetail>,
}

/// Which end of a mail list's timeline comes first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ListOrder {
    #[default]
    NewestFirst,
    OldestFirst,
}

impl ListOrder {
    /// The other order, which pages toward a list's start.
    #[must_use]
    pub const fn reversed(self) -> Self {
        match self {
            Self::NewestFirst => Self::OldestFirst,
            Self::OldestFirst => Self::NewestFirst,
        }
    }
}

/// What a mail list's saved order belongs to. The order goes when its
/// account, folder or label does, so a later one that takes the same id
/// starts newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListOwner {
    /// A list across accounts, such as a unified mailbox or an inbox half.
    Unified,
    Account(i64),
    Folder(i64),
    Label(i64),
}

/// Stable keyset cursor for the thread timeline, in either order. Timestamps
/// alone are not unique, so the thread id is the deterministic tie-breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCursor {
    pub last_message_at: i64,
    pub thread_id: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPage {
    pub threads: Vec<ThreadSummary>,
    pub next_cursor: Option<ThreadCursor>,
}

/// Position in a chronological search. `last_match_at` is the newest matching
/// message in the thread, which can differ from the thread's last message.
/// Keep the relaxed-query choice so every page uses the same match semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchCursor {
    pub last_match_at: i64,
    pub thread_id: i64,
    pub relaxed: bool,
}

#[derive(Debug, Clone)]
pub struct SearchPage {
    pub threads: Vec<ThreadSummary>,
    pub next_cursor: Option<SearchCursor>,
}

/// Where a server search of one account goes on from: Gmail's page token, how
/// many results JMAP gave already, or for IMAP each folder with the UID its
/// next results are below.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSearchCursor {
    pub page_token: Option<String>,
    pub position: u64,
    pub below: Vec<(i64, u32)>,
}

/// A page of what accounts' servers found for a search, past what this
/// device keeps: the conversations of the messages found, newest first. The
/// messages are kept here, their bodies fetched when opened.
#[derive(Debug, Clone, Default)]
pub struct ServerSearchPage {
    pub threads: Vec<ThreadSummary>,
    /// The messages themselves, newest first, for lists without
    /// conversations.
    pub messages: Vec<MessageSummary>,
    /// Where each account's search goes on from. An account that found all
    /// its server has isn't here.
    pub cursors: Vec<(i64, ServerSearchCursor)>,
    /// Accounts whose servers can't be searched: POP3, and mail kept only on
    /// this device.
    pub unsearchable: Vec<i64>,
    /// Accounts whose search failed, with why.
    pub failed: Vec<(i64, String)>,
}

/// Stable keyset cursor for the message timeline, in either order; the
/// message id breaks ties between messages with the same date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageCursor {
    pub date: i64,
    pub message_id: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePage {
    pub messages: Vec<MessageSummary>,
    pub next_cursor: Option<MessageCursor>,
}

/// Position in a chronological message search. Keep the relaxed-query choice
/// so every page uses the same match semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageSearchCursor {
    pub position: MessageCursor,
    pub relaxed: bool,
}

#[derive(Debug, Clone)]
pub struct MessageSearchPage {
    pub messages: Vec<MessageSummary>,
    pub next_cursor: Option<MessageSearchCursor>,
}

// Deliberately no `Debug`: it would print the account password.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddPasswordAccountArgs {
    pub email: String,
    pub display_name: Option<String>,
    pub username: String,
    pub password: String,
    pub mail_protocol: MailProtocol,
    #[serde(default)]
    pub jmap_url: String,
    pub incoming_host: String,
    pub incoming_port: u16,
    /// Empty, with port 0, for an account that only receives mail.
    pub smtp_host: String,
    pub smtp_port: u16,
    /// The SMTP server's own password, when it signs in with its own
    /// username (`connection.smtp_username`). Without one, an edit keeps the
    /// one saved, and the account's password stands in.
    #[serde(default)]
    pub smtp_password: Option<String>,
    pub connection: MailConnectionSettings,
}

/// What an account's mail export wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailExport {
    pub folders: usize,
    pub messages: usize,
    /// Messages left out because their original is missing from this device
    /// and the server no longer has it.
    pub missing: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionTestResult {
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    MarkRead,
    MarkUnread,
    Star,
    Unstar,
    Archive,
    Unarchive,
    Trash,
    DeletePermanently,
    Spam,
    NotSpam,
    Move,
    /// Files mail in one more folder of its account, where a message can be
    /// in several (JMAP).
    Copy,
    Snooze,
    Unsnooze,
    AddLabel,
    RemoveLabel,
    Send,
}

impl Settings {
    pub fn mail_grouping(&self) -> MailGrouping {
        if self.conversation_view {
            MailGrouping::Conversations
        } else {
            MailGrouping::Messages
        }
    }
}

impl ActionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionKind::MarkRead => "mark_read",
            ActionKind::MarkUnread => "mark_unread",
            ActionKind::Star => "star",
            ActionKind::Unstar => "unstar",
            ActionKind::Archive => "archive",
            ActionKind::Unarchive => "unarchive",
            ActionKind::Trash => "trash",
            ActionKind::DeletePermanently => "delete_permanently",
            ActionKind::Spam => "spam",
            ActionKind::NotSpam => "not_spam",
            ActionKind::Move => "move",
            ActionKind::Copy => "copy",
            ActionKind::Snooze => "snooze",
            ActionKind::Unsnooze => "unsnooze",
            ActionKind::AddLabel => "add_label",
            ActionKind::RemoveLabel => "remove_label",
            ActionKind::Send => "send",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "mark_read" => ActionKind::MarkRead,
            "mark_unread" => ActionKind::MarkUnread,
            "star" => ActionKind::Star,
            "unstar" => ActionKind::Unstar,
            "archive" => ActionKind::Archive,
            "unarchive" => ActionKind::Unarchive,
            "trash" => ActionKind::Trash,
            "delete_permanently" => ActionKind::DeletePermanently,
            "spam" => ActionKind::Spam,
            "not_spam" => ActionKind::NotSpam,
            "move" => ActionKind::Move,
            "copy" => ActionKind::Copy,
            "snooze" => ActionKind::Snooze,
            "unsnooze" => ActionKind::Unsnooze,
            "add_label" => ActionKind::AddLabel,
            "remove_label" => ActionKind::RemoveLabel,
            "send" => ActionKind::Send,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionParams {
    pub wake_at: Option<i64>,
    pub target_folder_id: Option<i64>,
    pub label_id: Option<i64>,
}

/// A message to forward as an attachment, as the composer names it before its original is
/// fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardedMessage {
    pub message_id: i64,
    pub account_id: i64,
    pub thread_id: Option<i64>,
    pub subject: String,
}

/// What an action applies to: whole conversations, or single messages, as a
/// mail list without conversations shows them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionTarget {
    Threads(Vec<i64>),
    Messages(Vec<i64>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformActionArgs {
    pub kind: ActionKind,
    pub target: ActionTarget,
    #[serde(default)]
    pub params: Option<ActionParams>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionResult {
    pub action_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftAttachmentIn {
    pub file_path: String,
    pub filename: String,
    /// The part's Content-Type when the name alone doesn't give it, such as
    /// a calendar invitation's `text/calendar; method=REQUEST`.
    #[serde(default)]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveDraftArgs {
    pub draft_id: Option<i64>,
    pub account_id: i64,
    /// Requested provider-authorized From identity. `None` resolves to the
    /// account's configured default sender (then the provider/primary default).
    #[serde(default)]
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    pub body_text: String,
    /// Rich body; goes out as text/html alongside the body_text fallback.
    #[serde(default)]
    pub body_html: Option<String>,
    pub mode: String,
    pub in_reply_to_message_id: Option<i64>,
    /// The Message-ID a reply answers when that message is not stored here,
    /// as a mailto link's In-Reply-To names it. Kept only for a new draft
    /// without `in_reply_to_message_id`.
    #[serde(default)]
    pub in_reply_to_header: Option<String>,
    #[serde(default)]
    pub attachments: Vec<DraftAttachmentIn>,
    /// The message asks for a read receipt (Disposition-Notification-To,
    /// RFC 8098), sent to its From address.
    #[serde(default)]
    pub request_read_receipt: bool,
    /// How important the message says it is.
    #[serde(default)]
    pub importance: Importance,
}

/// How important a message's sender marked it, from its Importance or
/// X-Priority header, or what a draft asks for (#236).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Importance {
    Low,
    #[default]
    Normal,
    High,
}

impl Importance {
    /// As stored: -1, 0 or 1.
    pub const fn as_i64(self) -> i64 {
        match self {
            Self::Low => -1,
            Self::Normal => 0,
            Self::High => 1,
        }
    }

    pub const fn from_i64(value: i64) -> Self {
        match value {
            ..=-1 => Self::Low,
            0 => Self::Normal,
            _ => Self::High,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSendArgs {
    pub draft_id: i64,
    pub send_at: Option<i64>,
    /// Remind the user at this time unless someone replies by then.
    #[serde(default)]
    pub remind_at: Option<i64>,
}

/// A draft waiting to be sent at a time the user chose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledSend {
    pub action_id: i64,
    pub draft_id: i64,
    /// The draft's conversation.
    pub thread_id: Option<i64>,
    pub account_id: i64,
    /// When it goes out, in ms since the epoch.
    pub send_at: i64,
    /// `pending`, `inflight`, `failed` and waiting for the user, or
    /// `uncertain`: it may have gone out, as the mail server never confirmed
    /// it, so it isn't sent again by itself.
    pub state: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSendResult {
    pub action_id: i64,
    pub draft_id: i64,
    pub dispatch_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    pub id: i64,
    pub name: String,
    pub shortcut: Option<String>,
    pub subject: Option<String>,
    pub body_text: String,
    pub usage_count: i64,
}

/// Which folders an IMAP account's roles such as Sent go to (#252).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountFolderRoles {
    /// One per role that can be chosen, as
    /// [`crate::db::repo::folders::CHOOSABLE_ROLES`] lists them.
    pub roles: Vec<FolderRoleChoice>,
    /// The folders a role can go to, by name, parents before their
    /// children.
    pub folders: Vec<FolderChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderRoleChoice {
    pub role: String,
    /// The folder the role goes to unless one is chosen: the one the
    /// server suggests, if any and if it isn't chosen for another role.
    pub suggested: Option<i64>,
    /// The folder chosen by hand, if any and if it's there.
    pub chosen: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderChoice {
    pub id: i64,
    /// The folder's name after its parents' names, outermost first, as
    /// "Archive" and "2026".
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderInfo {
    pub id: i64,
    pub account_id: i64,
    /// Stable local id of the parent mailbox. `None` means a top-level mailbox
    /// or an older cached row whose hierarchy has not been rediscovered yet.
    pub parent_id: Option<i64>,
    /// Unicode name suitable for display. `path` remains the exact remote
    /// identifier used in SELECT and other protocol commands.
    pub display_name: String,
    pub is_jmap: bool,
    /// Full hierarchical folder name, joined with `delimiter`.
    pub path: String,
    /// IMAP hierarchy delimiter (e.g. "/" or "."), for nesting user folders.
    pub delimiter: Option<String>,
    pub role: Option<String>,
    /// False for IMAP hierarchy containers advertised with `\\Noselect` and
    /// other provider mailboxes intentionally excluded from message sync.
    pub selectable: bool,
    pub can_create_children: bool,
    pub can_rename: bool,
    pub can_delete: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncBackgroundProgress {
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub account_id: i64,
    pub state: String,
    pub foreground_phase: String,
    pub background: Option<SyncBackgroundProgress>,
}

/// Structured action parsed by AI from a natural-language palette query.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiIntent {
    /// "create_event" | "compose" | "search" | "go_to" | "none"
    pub kind: String,
    pub summary: Option<String>,
    pub location: Option<String>,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
    pub all_day: Option<bool>,
    pub to: Option<Vec<String>>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub query: Option<String>,
    pub view: Option<String>,
}

/// One chronological beat of a thread: who acted, and what they did.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEntry {
    /// The person who acted ("Ana Moreau", or "You" for the account owner).
    #[serde(default)]
    pub actor: String,
    /// A terse, past-tense description of what they said or did.
    #[serde(default)]
    pub event: String,
}

/// A concrete dated item found in a thread that the user may choose to add to
/// their calendar. ISO-8601 strings preserve any timezone stated by the mail.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiCalendarSuggestion {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// A structured, sidebar-ready read of a whole thread: how it unfolded, what
/// matters, what the user must do next, and a ready-to-send draft reply.
/// (Distinct from [`ThreadSummary`], which is a thread-list *row*.)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiThreadSummary {
    /// Chronological beats, oldest first.
    #[serde(default)]
    pub timeline: Vec<TimelineEntry>,
    /// The essential facts and decisions, as short bullet lines.
    #[serde(default)]
    pub key_points: Vec<String>,
    /// The single next thing the user should do, or `None` if nothing is owed.
    #[serde(default)]
    pub next_action: Option<String>,
    /// A short draft reply the user could send, or `None` if no reply is needed.
    #[serde(default)]
    pub proposed_reply: Option<String>,
    /// A reliable, explicitly dated event/deadline the user may add after review.
    #[serde(default)]
    pub calendar_suggestion: Option<AiCalendarSuggestion>,
    /// The model saw a message try to instruct it, in its words (#241).
    #[serde(default)]
    pub warning: Option<String>,
    /// Older messages the summary leaves out, as the model's input ran out.
    #[serde(default)]
    pub omitted: usize,
    /// Links, addresses and phone numbers the summary has and the messages
    /// don't, which the reader should not trust.
    #[serde(default)]
    pub unverified: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiUsageDay {
    pub date: String,
    pub total_tokens: i64,
    pub requests: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiUsageStats {
    pub total_tokens: i64,
    pub total_requests: i64,
    pub today_tokens: i64,
    pub yesterday_tokens: i64,
    pub last_7_days_tokens: i64,
    pub last_30_days_tokens: i64,
    pub days: Vec<AiUsageDay>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailActivityDay {
    pub date: String,
    pub sent: i64,
    pub received: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailStats {
    pub total_sent: i64,
    pub total_received: i64,
    pub today_sent: i64,
    pub today_received: i64,
    pub last_7_days_sent: i64,
    pub last_7_days_received: i64,
    pub last_30_days_sent: i64,
    pub last_30_days_received: i64,
    pub days: Vec<EmailActivityDay>,
}

/// One account's sidebar badges: the unread conversations or messages of
/// each view the sidebar opens, counted as the mail list shows them. Drafts
/// counts every draft instead. Folders and labels without unread mail are
/// left out of their lists.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MailboxBadgeCounts {
    pub account_id: i64,
    /// Outside snoozed conversations, as the Inbox lists them.
    pub inbox: i64,
    pub starred: i64,
    pub drafts: i64,
    pub sent: i64,
    /// Archived mail that is not also in the inbox, as Archive lists it.
    pub archive: i64,
    pub spam: i64,
    pub trash: i64,
    /// The inbox's split into Important and Other.
    pub important: i64,
    pub other: i64,
    /// Unread per folder id, ascending.
    pub folders: Vec<(i64, i64)>,
    /// Unread per label id, ascending: manual labels count their members,
    /// categories the inbox mail routed to them.
    pub labels: Vec<(i64, i64)>,
}

/// One deterministic action attached to an AI-matched automation. `value` is
/// a route key, label id, or local annotation text depending on `kind`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiAutomationAction {
    pub kind: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiAutomationRule {
    pub id: String,
    pub name: String,
    /// The user's original natural-language request. `instruction` is the
    /// planner's normalized match condition used at classification time.
    #[serde(default)]
    pub source_prompt: String,
    pub instruction: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub actions: Vec<AiAutomationAction>,
}

/// Safe, structured interpretation of a natural-language automation request.
/// The AI proposes this shape, then the core validates every action and target
/// against the local allow-list before returning it to the UI.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct AiAutomationPlan {
    pub supported: bool,
    pub name: String,
    pub instruction: String,
    pub actions: Vec<AiAutomationAction>,
    pub summary: String,
    pub issues: Vec<String>,
}

/// User-selected foundation colors for both OS color schemes. Strings keep
/// this persistence model independent from the UI toolkit and make exported
/// settings human-readable. The desktop shell validates them before use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct CustomTheme {
    pub light_primary: String,
    pub light_page_background: String,
    pub light_surface: String,
    pub light_text: String,
    pub light_border: String,
    pub dark_primary: String,
    pub dark_page_background: String,
    pub dark_surface: String,
    pub dark_text: String,
    pub dark_border: String,
}

impl Default for CustomTheme {
    fn default() -> Self {
        Self {
            light_primary: "#0969DA".into(),
            light_page_background: "#F2F2F0".into(),
            light_surface: "#FFFFFF".into(),
            light_text: "#202120".into(),
            light_border: "#D9D9D6".into(),
            dark_primary: "#0969DA".into(),
            dark_page_background: "#111213".into(),
            dark_surface: "#18191A".into(),
            dark_text: "#F3F3F2".into(),
            dark_border: "#3A3B3C".into(),
        }
    }
}

/// A search kept in the sidebar. Its mail stays where it is: the search only
/// shows it, as the search field would.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedSearch {
    pub id: i64,
    pub name: String,
    /// What it searches for, as typed in the search field.
    pub query: String,
    /// The mail profile whose accounts it covers. Without one it covers the
    /// accounts the mail view shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
}

/// A local, user-owned grouping for connected mail accounts.
///
/// Profiles organize account provenance in Flectar Mail only. They never map
/// to a provider object and therefore cannot mutate remote mailboxes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailProfile {
    pub id: String,
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub account_ids: Vec<i64>,
}

/// The reading surface follows the application unless explicitly overridden.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageAppearance {
    #[default]
    System,
    Light,
    Dark,
}

impl MessageAppearance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

/// Whether the interface reduces motion (#231): as the operating system asks,
/// or always, or never.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReduceMotion {
    #[default]
    System,
    On,
    Off,
}

impl ReduceMotion {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// What to do when a message asks for a read receipt (RFC 8098). The
/// standard wants none sent without the user's consent by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadReceiptPolicy {
    #[default]
    Never,
    /// Ask above the message.
    Ask,
    /// Send them, except where RFC 8098 wants the user asked first.
    Always,
}

impl ReadReceiptPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Ask => "ask",
            Self::Always => "always",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "never" => Some(Self::Never),
            "ask" => Some(Self::Ask),
            "always" => Some(Self::Always),
            _ => None,
        }
    }
}

/// The interface's text size (#231), as a step on the system's own text size.
/// Message zoom and the composer's text size stay separate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceTextSize {
    Smaller,
    #[default]
    Default,
    Large,
    Larger,
    Largest,
}

impl InterfaceTextSize {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smaller => "smaller",
            Self::Default => "default",
            Self::Large => "large",
            Self::Larger => "larger",
            Self::Largest => "largest",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "smaller" => Some(Self::Smaller),
            "default" => Some(Self::Default),
            "large" => Some(Self::Large),
            "larger" => Some(Self::Larger),
            "largest" => Some(Self::Largest),
            _ => None,
        }
    }

    /// How many times the system's text size the interface uses.
    pub fn factor(self) -> f32 {
        match self {
            Self::Smaller => 0.9,
            Self::Default => 1.0,
            Self::Large => 1.15,
            Self::Larger => 1.3,
            Self::Largest => 1.5,
        }
    }
}

/// Which invitations found in mail join the calendar before the user answers
/// them (#228). Answering one, or adding it from its card, always does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AddInvitations {
    /// Every invitation to the user.
    Everyone,
    /// Invitations from people the user has written to or keeps as a
    /// contact, when their mail server confirms who sent them. RFC 9671
    /// section 4 asks for calendar data from known senders only.
    #[default]
    Known,
    /// None: an invitation joins the calendar once the user answers it.
    Answered,
}

impl AddInvitations {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Everyone => "everyone",
            Self::Known => "known",
            Self::Answered => "answered",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "everyone" => Some(Self::Everyone),
            "known" => Some(Self::Known),
            "answered" => Some(Self::Answered),
            _ => None,
        }
    }
}

/// How times of day are written: with the clock the user's region prefers,
/// or always with a 12- or 24-hour clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeFormat {
    #[default]
    #[serde(rename = "automatic")]
    Automatic,
    #[serde(rename = "12h")]
    TwelveHour,
    #[serde(rename = "24h")]
    TwentyFourHour,
}

impl TimeFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::TwelveHour => "12h",
            Self::TwentyFourHour => "24h",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "automatic" => Some(Self::Automatic),
            "12h" => Some(Self::TwelveHour),
            "24h" => Some(Self::TwentyFourHour),
            _ => None,
        }
    }
}

/// The application icon shown by the window, tray, Dock, and launcher.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AppIcon {
    #[default]
    Orange,
    Graphite,
    Light,
    Blue,
    Green,
    Purple,
}

impl AppIcon {
    pub const ALL: [Self; 6] = [
        Self::Orange,
        Self::Graphite,
        Self::Light,
        Self::Blue,
        Self::Green,
        Self::Purple,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Orange => "orange",
            Self::Graphite => "graphite",
            Self::Light => "light",
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Purple => "purple",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|icon| icon.as_str() == value)
    }
}

/// Unknown values, including those written by newer versions, fall back to
/// the default icon instead of failing the whole settings blob.
impl<'de> Deserialize<'de> for AppIcon {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value).unwrap_or_default())
    }
}

/// How tightly the mail list packs its rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ListDensity {
    #[default]
    Comfortable,
    Compact,
}

impl ListDensity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Comfortable => "comfortable",
            Self::Compact => "compact",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [Self::Comfortable, Self::Compact]
            .into_iter()
            .find(|choice| choice.as_str() == value)
    }
}

/// The most lines of a message's text the mail list shows.
pub const MAX_PREVIEW_LINES: u8 = 3;

/// When an unread message the user opens counts as read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MarkRead {
    /// As it opens.
    #[default]
    Immediately,
    /// Once it has stayed open for two seconds.
    AfterTwoSeconds,
    /// Once it has stayed open for five seconds.
    AfterFiveSeconds,
    /// Only when the user marks it.
    Never,
}

impl MarkRead {
    pub const ALL: [Self; 4] = [
        Self::Immediately,
        Self::AfterTwoSeconds,
        Self::AfterFiveSeconds,
        Self::Never,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Immediately => "immediately",
            Self::AfterTwoSeconds => "afterTwoSeconds",
            Self::AfterFiveSeconds => "afterFiveSeconds",
            Self::Never => "never",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|choice| choice.as_str() == value)
    }

    /// How long an opened message stays open before it counts as read, or
    /// `None` when opening never marks it.
    pub fn delay(self) -> Option<std::time::Duration> {
        match self {
            Self::Immediately => Some(std::time::Duration::ZERO),
            Self::AfterTwoSeconds => Some(std::time::Duration::from_secs(2)),
            Self::AfterFiveSeconds => Some(std::time::Duration::from_secs(5)),
            Self::Never => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub theme: String,
    #[serde(default)]
    pub message_appearance: MessageAppearance,
    /// Whether the interface reduces motion.
    #[serde(default)]
    pub reduce_motion: ReduceMotion,
    /// The interface's text size.
    #[serde(default)]
    pub interface_text_size: InterfaceTextSize,
    /// What to do when a message asks for a read receipt.
    #[serde(default)]
    pub read_receipts: ReadReceiptPolicy,
    /// For how many days mail stays in Trash before Flectar Mail deletes
    /// it for good, counted from when it went there. 0 keeps it.
    #[serde(default)]
    pub empty_trash_after_days: u32,
    /// The same for Junk.
    #[serde(default)]
    pub empty_junk_after_days: u32,
    /// Named application palette: "default" | "teal" | "green" | "purple" | "custom".
    #[serde(default = "default_theme_preset")]
    pub theme_preset: String,
    #[serde(default)]
    pub custom_theme: CustomTheme,
    /// Show sender identity images in the mail list and reading pane.
    /// Existing settings blobs predate this option, so avatars remain visible
    /// unless the user explicitly turns them off.
    #[serde(default = "default_true")]
    pub show_avatars: bool,
    /// Group the mail list into collapsible local-date sections.
    #[serde(default = "default_true")]
    pub group_mail_by_date: bool,
    /// How tightly the mail list packs its rows.
    #[serde(default)]
    pub list_density: ListDensity,
    /// How many lines of each message's text the mail list shows, up to
    /// `MAX_PREVIEW_LINES`.
    #[serde(default = "default_preview_lines")]
    pub preview_lines: u8,
    /// Show browser-style tabs: the strip in the desktop top bar, and the tab
    /// switcher in compact layouts. Saved tabs are kept while they are hidden.
    #[serde(default = "default_true")]
    pub show_tabs: bool,
    /// Group replies with the messages they answer into conversations. Off
    /// lists, opens and acts on every message on its own.
    #[serde(default = "default_true")]
    pub conversation_view: bool,
    /// Workspace presentation: "default" keeps lists and details side by
    /// side, while "minimal" navigates between those shared surfaces.
    /// `mailLayout` was the short-lived original key before Contacts shared
    /// the preference; accept it so development builds upgrade cleanly.
    #[serde(default = "default_workspace_layout", alias = "mailLayout")]
    pub workspace_layout: String,
    /// Shared logical-pixel width of the mail, contacts, and files list pane.
    /// The established layout width remains the default and hard minimum.
    #[serde(default = "default_workspace_list_pane_width")]
    pub workspace_list_pane_width: i64,
    /// Desktop window frame: "system" keeps the platform title bar, while
    /// "integrated" draws the title bar and window controls in the app.
    #[serde(default = "default_window_frame")]
    pub window_frame: String,
    /// Render mailbox and folder icons with a single neutral color instead of
    /// the default semantic palette.
    #[serde(default)]
    pub monochrome_sidebar_icons: bool,
    #[serde(default)]
    pub app_icon: AppIcon,
    /// UI language: "system" follows the OS locale, otherwise a code like "en".
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub time_format: TimeFormat,
    /// First day shown in calendar week and month views: "sunday" | "monday".
    #[serde(default = "default_calendar_week_start")]
    pub calendar_week_start: String,
    pub undo_send_seconds: i64,
    /// The font and size new messages start in: "default", which names none, "serif" or
    /// "monospace", and "small", "normal", "large" or "huge".
    #[serde(default = "default_compose_font")]
    pub compose_font: String,
    #[serde(default = "default_compose_text_size")]
    pub compose_text_size: String,
    /// How many days a link to a file sent by link works, or 0 for as long as the file is
    /// kept. A server that limits links shortens it.
    #[serde(default = "default_link_expiry_days")]
    pub link_expiry_days: u32,
    /// A file sent by link asks for a password, which the sender passes on another way.
    #[serde(default)]
    pub link_password: bool,
    #[serde(default)]
    pub load_remote_images: bool,
    /// When an unread message the user opens counts as read.
    #[serde(default)]
    pub mark_read: MarkRead,
    /// Directory desktop save dialogs open in. An empty value follows the
    /// platform default. The path is deliberately not validated when stored:
    /// a saved location can be unavailable later, such as an unmounted network
    /// share, so downloads fall back to the platform default at save time
    /// rather than failing.
    #[serde(default)]
    pub download_directory: String,
    /// The AI server the app's AI features and translation by a model use
    /// (#241). Its key is in the system's credential store.
    #[serde(default)]
    pub ai: AiSettings,
    /// OAuth app registrations supplied by the user. These intentionally
    /// override the FLECTAR_*_DESKTOP_CLIENT_ID values bundled by a release.
    #[serde(default)]
    pub google_client_id: String,
    #[serde(default)]
    pub google_client_secret: String,
    #[serde(default)]
    pub ms_client_id: String,
    /// Only for Web-type Entra registrations; public (desktop) clients must
    /// leave this empty or Microsoft rejects the token request.
    #[serde(default)]
    pub ms_client_secret: String,
    /// Semantic-search embedding backend: "local" | "off". Local runs a small
    /// model on-device; off disables vector indexing (keyword search only).
    #[serde(default = "default_embedding_backend")]
    pub embedding_backend: String,
    /// Registry key of the local embedding model (see `embed::registry`).
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,
    /// When true, AI drafts are written in the user's learned voice.
    #[serde(default)]
    pub voice_drafting: bool,
    /// Distilled style profile learned from the user's sent mail (plain text).
    #[serde(default)]
    pub voice_profile: String,
    /// When the voice profile was last learned (ms epoch; 0 = never).
    #[serde(default)]
    pub voice_learned_at: i64,
    /// Minutes before a meeting to fire a desktop reminder; 0 disables.
    #[serde(default = "default_notify_lead")]
    pub meeting_notify_lead_minutes: i64,
    /// Which invitations found in mail join the calendar before an answer.
    #[serde(default)]
    pub add_invitations: AddInvitations,
    /// Desktop notification on new mail.
    #[serde(default = "default_true")]
    pub notifications_enabled: bool,
    /// Foreground provider refresh cadence in minutes. Push-capable providers
    /// may still deliver mail sooner; this controls the periodic backstop.
    #[serde(default = "default_sync_interval_minutes")]
    pub sync_interval_minutes: i64,
    /// Play a sound on new mail and when sending.
    #[serde(default = "default_true")]
    pub sound_enabled: bool,
    /// Keep the process running in the system tray when the main window is
    /// closed. Disabled by default so close retains normal desktop semantics.
    #[serde(default)]
    pub close_to_tray: bool,
    /// After archiving from a conversation, open the next thread (vs. back to list).
    #[serde(default = "default_true")]
    pub auto_advance: bool,
    /// After selecting a thread with `x`, move the cursor to the next row. Off
    /// makes `x` toggle in place (Gmail-style), leaving the cursor put.
    #[serde(default = "default_true")]
    pub select_advance: bool,
    /// Automatic Marketing/News/Social/Pitch categorization at sync time.
    #[serde(default = "default_true")]
    pub auto_labels_enabled: bool,
    /// When true, mail that no routing rule catches is sorted into a category by
    /// the AI classifier (using `ai_category_prompt`) instead of the built-in
    /// heuristic. Off, or with no API key, falls back to the heuristic.
    #[serde(default)]
    pub ai_categorize: bool,
    /// Natural-language description of the categories, fed to the AI classifier.
    /// Empty uses a built-in default prompt.
    #[serde(default)]
    pub ai_category_prompt: String,
    /// Compound workflows matched by the AI. The model only returns rule ids;
    /// every mailbox mutation comes from this explicit, user-owned allow-list.
    #[serde(default)]
    pub ai_automation_rules: Vec<AiAutomationRule>,
    /// Group the thread list under date headers (Today / Yesterday / …).
    #[serde(default = "default_true")]
    pub group_by_date: bool,
    /// When true, compose "To" autocomplete suggests contacts from every account;
    /// off (default) scopes suggestions to the account you're sending from.
    #[serde(default)]
    pub contact_suggest_all_accounts: bool,
    /// Learn addresses from messages the user sends so they can be offered as
    /// compose suggestions. These remain local suggestions until explicitly
    /// promoted to the address book.
    #[serde(default = "default_true")]
    pub collect_outgoing_contacts: bool,
    /// Learn human senders from received mail. Off by default because inboxes
    /// contain newsletters, receipts, and unsolicited mail that should not
    /// silently become part of the user's people data.
    #[serde(default)]
    pub collect_incoming_contacts: bool,
    /// Include locally learned people alongside saved contacts in compose and
    /// contact-aware search suggestions.
    #[serde(default = "default_true")]
    pub suggest_learned_contacts: bool,
    /// Underline misspelled words in messages being written.
    #[serde(default = "default_true")]
    pub spelling_enabled: bool,
    /// The languages spelling is checked in, as dictionary names such as "en_US". `None`
    /// follows the system's languages; an empty list checks none.
    #[serde(default)]
    pub spelling_languages: Option<Vec<String>>,
    /// Show on the app icon how many unread messages in the Inboxes the
    /// new-mail notifications cover (see `notification_scope`): the Dock,
    /// the taskbar, a Linux dock or a phone's home screen.
    #[serde(default = "default_true")]
    pub unread_badge_enabled: bool,
    /// Ask GitHub once a day whether a newer release is out. Only builds made
    /// for direct download ask; app stores and package managers update the
    /// app themselves.
    #[serde(default = "default_true")]
    pub check_for_updates: bool,
    /// Which incoming mail raises a desktop notification: "important" (mail that
    /// lands in the Important tab, the default and historical behavior), "all"
    /// (every incoming inbox message), or "tabs" (only the tabs named in
    /// `notification_tabs`). The master `notifications_enabled` toggle gates all
    /// three.
    #[serde(default = "default_notification_scope")]
    pub notification_scope: String,
    /// Route keys whose mail notifies when `notification_scope` is "tabs":
    /// "important", "other", "split:<id>", or "label:<id>" (the same keys stored
    /// in `threads.routed_tab`).
    #[serde(default)]
    pub notification_tabs: Vec<String>,
    /// Rich signatures. An account may own several; the composer picks one by
    /// mode via `signature_defaults` (or a manual override).
    #[serde(default)]
    pub signature_list: Vec<Signature>,
    /// Which signature each account defaults to, keyed by stringified account id.
    #[serde(default)]
    pub signature_defaults: std::collections::HashMap<String, SignatureDefaults>,
    /// Per-account theme override ("snow" | "carbon" | "system"), keyed by
    /// stringified account id. Missing = follow the global `theme`.
    #[serde(default)]
    pub account_themes: std::collections::HashMap<String, String>,
    /// Show which account each mail-list row belongs to with a color marker.
    #[serde(default)]
    pub show_account_badges: bool,
    /// Flectar Mail Sync carries passwords and sign-ins between the user's
    /// devices too, end-to-end encrypted, so accounts arrive signed in.
    #[serde(default = "default_true")]
    pub sync_sign_ins: bool,
    /// Per-account marker color (hex), keyed by stringified account id.
    /// Missing = the UI derives a stable hue from the address.
    #[serde(default)]
    pub account_colors: std::collections::HashMap<String, String>,
    /// Optional local groups such as Personal and Work. An account may belong
    /// to at most one profile; provider state is never affected.
    #[serde(default)]
    pub mail_profiles: Vec<MailProfile>,
    /// Searches kept in the sidebar, in its order.
    #[serde(default)]
    pub saved_searches: Vec<SavedSearch>,
    /// The profile whose accounts the mail view shows. `None` shows every
    /// account. Never names a deleted or empty profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_mail_profile: Option<String>,
    /// What AI agents on this computer may do with mail through the local
    /// MCP server. Off until the user turns it on.
    #[serde(default)]
    pub agent_access: AgentAccess,
    /// Translating messages (#240). Off until the user picks a service.
    #[serde(default)]
    pub translation: TranslationSettings,
    /// The proxy the app's connections go through (#250).
    #[serde(default)]
    pub proxy: crate::net::proxy::ProxySettings,
}

/// The AI server (#241): any server with the OpenAI chat API, on this
/// device, such as Ollama or LM Studio, or hosted. Nothing is preset, and AI
/// features stay off while the address or the model is empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiSettings {
    /// The server's address, such as http://localhost:11434/v1.
    pub url: String,
    /// The model it runs, as the server names it.
    pub model: String,
    /// The user's own instructions, run on selected text or a draft.
    pub prompts: Vec<AiPrompt>,
}

/// An instruction the user saved, such as "Make it shorter".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiPrompt {
    pub name: String,
    pub instruction: String,
}

impl AiSettings {
    /// A server and a model are set, so AI features can run.
    pub fn ready(&self) -> bool {
        !self.url.trim().is_empty() && !self.model.trim().is_empty()
    }
}

/// What the AI server answered about selected text (#241).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiAnswer {
    pub text: String,
    /// Links, addresses and phone numbers the answer has and the selected
    /// text doesn't.
    pub unverified: Vec<String>,
}

/// How messages are translated (#240). The services' keys are in the
/// system's credential store, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TranslationSettings {
    /// "deepl", "libretranslate" or "model", the AI server in `Settings::ai`,
    /// or "" while none is set up.
    pub service: String,
    /// The language messages are translated into, as a language tag such as
    /// "de" or "pt-BR". "" follows the app's language.
    pub target: String,
    /// Offer to translate a message in a language the user doesn't read.
    pub offer: bool,
    /// Languages translated as soon as a message opens, and languages never
    /// offered, as primary language subtags. A language is on one list at
    /// most.
    pub always: Vec<String>,
    pub never: Vec<String>,
    /// The LibreTranslate server's address.
    pub libretranslate_url: String,
    /// The language a message being written was last translated into.
    pub compose_target: String,
}

impl Default for TranslationSettings {
    fn default() -> Self {
        TranslationSettings {
            service: String::new(),
            target: String::new(),
            offer: true,
            always: Vec::new(),
            never: Vec::new(),
            libretranslate_url: String::new(),
            compose_target: String::new(),
        }
    }
}

/// What AI agents may see and do through `flectar-mail mcp`, the same for
/// every agent. Reading is the only right that comes with access itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentAccess {
    pub enabled: bool,
    /// Accounts agents can't see.
    pub hidden_accounts: Vec<i64>,
    /// Folders agents can't see.
    pub hidden_folders: Vec<i64>,
    /// Spam and Trash folders agents can see. They're hidden otherwise, since
    /// junk mail is where hostile instructions are most likely to be.
    pub shown_folders: Vec<i64>,
    /// Write drafts for the user to review and send. Agents never send them.
    pub can_write_drafts: bool,
    /// Mark messages read or unread, flagged or not.
    pub can_mark: bool,
    /// Move messages between folders, to the archive or to the trash.
    pub can_move: bool,
    /// Add and remove labels.
    pub can_label: bool,
    /// Send drafts, each one approved in the app first.
    pub can_send: bool,
}

impl AgentAccess {
    /// Whether agents can see the account `account_id`.
    pub fn shows_account(&self, account_id: i64) -> bool {
        self.enabled && !self.hidden_accounts.contains(&account_id)
    }

    /// Whether agents can see `folder`.
    pub fn shows_folder(&self, folder: &FolderInfo) -> bool {
        if !self.shows_account(folder.account_id) || self.hidden_folders.contains(&folder.id) {
            return false;
        }
        let hidden_by_role = matches!(folder.role.as_deref(), Some(roles::SPAM | roles::TRASH));
        !hidden_by_role || self.shown_folders.contains(&folder.id)
    }
}

/// A named, rich-HTML signature belonging to one account.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Signature {
    pub id: String,
    pub account_id: i64,
    pub name: String,
    /// Rich body (sanitized HTML; same markup the composer emits).
    pub html: String,
}

/// Per-account default signature choice, Gmail-style: one for new mail, one for
/// replies/forwards. `None` means "no signature".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureDefaults {
    #[serde(default)]
    pub new_id: Option<String>,
    #[serde(default)]
    pub reply_id: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_notify_lead() -> i64 {
    10
}

fn default_sync_interval_minutes() -> i64 {
    5
}

fn default_embedding_backend() -> String {
    "local".into()
}
fn default_embedding_model() -> String {
    crate::embed::DEFAULT_MODEL.into()
}
fn default_language() -> String {
    "system".into()
}
fn default_calendar_week_start() -> String {
    "monday".into()
}
fn default_notification_scope() -> String {
    "important".into()
}
fn default_workspace_layout() -> String {
    "default".into()
}
pub const MIN_WORKSPACE_LIST_PANE_WIDTH: i64 = 390;
pub const MAX_WORKSPACE_LIST_PANE_WIDTH: i64 = 8192;

fn default_workspace_list_pane_width() -> i64 {
    MIN_WORKSPACE_LIST_PANE_WIDTH
}

pub fn normalized_workspace_list_pane_width(width: i64) -> i64 {
    width.clamp(MIN_WORKSPACE_LIST_PANE_WIDTH, MAX_WORKSPACE_LIST_PANE_WIDTH)
}

/// New installs draw the window frame themselves; a saved choice is kept.
fn default_window_frame() -> String {
    "integrated".into()
}
fn default_compose_font() -> String {
    "default".into()
}
fn default_compose_text_size() -> String {
    "normal".into()
}

/// Links to files sent by link work for 30 days, as Mail Drop's do.
fn default_link_expiry_days() -> u32 {
    30
}

/// Unknown values, including those written by newer versions, fall back to
/// the platform frame so a window can never start without any controls.
pub fn normalized_window_frame(frame: &str) -> &'static str {
    match frame {
        "integrated" => "integrated",
        _ => "system",
    }
}

/// Presentation preferences read before the main window is first shown.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StartupPresentation {
    pub workspace_list_pane_width: Option<i64>,
    pub window_frame: Option<String>,
    /// Whether a background launch may stay in the system tray without
    /// first showing the window.
    pub close_to_tray: bool,
    pub app_icon: AppIcon,
    /// Whether the first mail list, and the cached one shown before it,
    /// group conversations.
    pub grouping: MailGrouping,
}
fn default_theme_preset() -> String {
    "default".into()
}

fn default_preview_lines() -> u8 {
    1
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "system".into(),
            message_appearance: MessageAppearance::default(),
            reduce_motion: ReduceMotion::default(),
            interface_text_size: InterfaceTextSize::default(),
            read_receipts: ReadReceiptPolicy::default(),
            empty_trash_after_days: 0,
            empty_junk_after_days: 0,
            theme_preset: default_theme_preset(),
            custom_theme: CustomTheme::default(),
            show_avatars: true,
            group_mail_by_date: true,
            list_density: ListDensity::Comfortable,
            preview_lines: default_preview_lines(),
            show_tabs: true,
            conversation_view: true,
            workspace_layout: default_workspace_layout(),
            workspace_list_pane_width: default_workspace_list_pane_width(),
            window_frame: default_window_frame(),
            monochrome_sidebar_icons: false,
            app_icon: AppIcon::default(),
            language: "system".into(),
            time_format: TimeFormat::default(),
            calendar_week_start: default_calendar_week_start(),
            undo_send_seconds: 10,
            compose_font: default_compose_font(),
            compose_text_size: default_compose_text_size(),
            link_expiry_days: default_link_expiry_days(),
            link_password: false,
            load_remote_images: false,
            mark_read: MarkRead::Immediately,
            download_directory: String::new(),
            google_client_id: String::new(),
            google_client_secret: String::new(),
            ms_client_id: String::new(),
            ms_client_secret: String::new(),
            embedding_backend: default_embedding_backend(),
            embedding_model: default_embedding_model(),
            voice_drafting: false,
            voice_profile: String::new(),
            voice_learned_at: 0,
            meeting_notify_lead_minutes: 10,
            add_invitations: AddInvitations::default(),
            notifications_enabled: true,
            sync_interval_minutes: default_sync_interval_minutes(),
            sound_enabled: true,
            close_to_tray: false,
            auto_advance: true,
            select_advance: true,
            auto_labels_enabled: true,
            ai_categorize: false,
            ai_category_prompt: String::new(),
            ai_automation_rules: Vec::new(),
            group_by_date: true,
            contact_suggest_all_accounts: false,
            collect_outgoing_contacts: true,
            collect_incoming_contacts: false,
            suggest_learned_contacts: true,
            spelling_enabled: true,
            spelling_languages: None,
            unread_badge_enabled: true,
            check_for_updates: true,
            notification_scope: default_notification_scope(),
            notification_tabs: Vec::new(),
            signature_list: Vec::new(),
            signature_defaults: std::collections::HashMap::new(),
            account_themes: std::collections::HashMap::new(),
            show_account_badges: false,
            sync_sign_ins: true,
            account_colors: std::collections::HashMap::new(),
            mail_profiles: Vec::new(),
            saved_searches: Vec::new(),
            active_mail_profile: None,
            agent_access: AgentAccess::default(),
            translation: TranslationSettings::default(),
            ai: AiSettings::default(),
            proxy: crate::net::proxy::ProxySettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingStatus {
    /// Whether the local embedding backend is enabled.
    pub enabled: bool,
    /// Active model registry key.
    pub model: String,
    /// Messages with a cached body (embedding candidates).
    pub total: i64,
    /// Messages embedded for the active model.
    pub embedded: i64,
    /// Messages queued for embedding.
    pub pending: i64,
    /// Whether the model is loaded and the index is serving.
    pub ready: bool,
}

/// One retrieved source behind a RAG answer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AskCitation {
    pub message_id: i64,
    pub thread_id: i64,
    pub subject: String,
    pub from: String,
    pub date: i64,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AskResult {
    pub answer: String,
    pub citations: Vec<AskCitation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStatus {
    /// A server and a model are set.
    pub configured: bool,
    pub url: String,
    pub model: String,
    /// A key for the server is saved.
    pub key_saved: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    pub id: i64,
    pub account_id: i64,
    pub message_id: Option<i64>,
    pub summary: Option<String>,
    pub location: Option<String>,
    pub organizer: Option<String>,
    pub description: Option<String>,
    pub attendees: Vec<EventAttendee>,
    pub join_url: Option<String>,
    /// Our response to the invite: ACCEPTED | TENTATIVE | DECLINED.
    pub rsvp_status: Option<String>,
    /// Created in Flectar Mail (vs. parsed from an incoming invite).
    pub is_local: bool,
    /// CalDAV collection this event syncs with; None = local-only.
    pub calendar_id: Option<i64>,
    /// Raw RRULE when the event repeats (UI badges "repeats").
    pub rrule: Option<String>,
    pub starts_at: i64,
    pub ends_at: Option<i64>,
    pub all_day: bool,
    pub status: Option<String>,
    pub method: Option<String>,
    /// The user is among its guests and doesn't organize it, so it is theirs
    /// to answer. Set by [`crate::Core::list_events`].
    #[serde(default)]
    pub invited: bool,
    /// For one occurrence of a repeating event, as the calendar lists it: the
    /// start the series gives it, which names it (its RECURRENCE-ID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence_id: Option<i64>,
}

/// How a calendar connection reaches its server. An account holds at most
/// one Google or Microsoft connection, which signs in with the account's own
/// OAuth tokens, and any number of CalDAV connections with their own
/// username and app password (for example a shared mailbox's calendar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CalendarConnectionKind {
    /// Google Calendar over CalDAV, enumerated with the Calendar API.
    Google,
    /// Any CalDAV server.
    Generic,
    /// Outlook and Microsoft 365 over Microsoft Graph.
    Microsoft,
}

impl CalendarConnectionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Generic => "generic",
            Self::Microsoft => "microsoft",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "google" => Some(Self::Google),
            "generic" => Some(Self::Generic),
            "microsoft" => Some(Self::Microsoft),
            _ => None,
        }
    }
}

/// A calendar collection synced through one connection.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    pub id: i64,
    pub account_id: i64,
    pub connection_id: i64,
    /// The owning connection's kind.
    pub kind: CalendarConnectionKind,
    pub url: String,
    pub display_name: Option<String>,
    pub color: Option<String>,
    pub read_only: bool,
    pub enabled: bool,
    pub is_default: bool,
    pub last_synced_at: Option<i64>,
}

/// A calendar server connected to an account, shown in account settings.
/// Events created in its calendars use the account's address as organizer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarConnection {
    pub id: i64,
    pub account_id: i64,
    pub kind: CalendarConnectionKind,
    pub base_url: String,
    pub username: String,
    pub enabled: bool,
    pub last_error: Option<String>,
    /// Display names of the connection's calendars, in sidebar order.
    pub calendars: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventAttendee {
    pub email: String,
    pub name: Option<String>,
    /// NEEDS-ACTION | ACCEPTED | TENTATIVE | DECLINED
    pub partstat: Option<String>,
    /// When the attendee wrote their newest answer to the user's invitation
    /// (its DTSTAMP), so an older answer read later doesn't replace it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateEventArgs {
    pub account_id: i64,
    /// Target calendar collection; None = account's default (or local-only).
    #[serde(default)]
    pub calendar_id: Option<i64>,
    pub summary: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub join_url: Option<String>,
    pub starts_at: i64,
    pub ends_at: i64,
    #[serde(default)]
    pub all_day: bool,
    /// Invites are emailed (ICS METHOD:REQUEST) to every attendee.
    #[serde(default)]
    pub attendees: Vec<Address>,
}

/// What a message's calendar does with its event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InvitationKind {
    /// An invitation, or an update of one.
    Request,
    /// The organizer cancelled the event, or some of its occurrences.
    Cancel,
    /// An attendee's answer to an event the user organizes.
    Reply,
    /// An attendee proposes a change.
    Counter,
    /// The organizer turns down a proposed change.
    DeclineCounter,
    /// An event to add, with nothing to answer.
    Publish,
    /// An attendee asks for the invitation again.
    Refresh,
}

/// A message's calendar as the card above the message shows it (#228): the
/// message's own version of the event, set against the user's calendar now.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvitationCard {
    pub kind: InvitationKind,
    pub title: Option<String>,
    pub starts_at: i64,
    pub ends_at: Option<i64>,
    pub all_day: bool,
    /// The event repeats.
    pub repeats: bool,
    /// The message is about one occurrence of an event that repeats.
    pub occurrence: bool,
    /// That occurrence's RECURRENCE-ID, which answers it alone.
    pub recurrence_id: Option<i64>,
    pub location: Option<String>,
    pub join_url: Option<String>,
    pub organizer: Option<Address>,
    /// The guests and their answers, as the message lists them.
    pub attendees: Vec<EventAttendee>,
    /// The event in the user's calendar, while it is there.
    pub event_id: Option<i64>,
    /// The user's answer, as their calendar has it.
    pub my_answer: Option<String>,
    /// The user's address the invitation names. None when they aren't on
    /// its guest list, such as for a forwarded invitation.
    pub invited_as: Option<String>,
    /// The user organizes the event.
    pub organized: bool,
    /// The calendar has a newer version of the event than this message.
    pub out_of_date: bool,
    /// The event is cancelled.
    pub cancelled: bool,
    /// For an answer: who answered, and how.
    pub answer: Option<EventAttendee>,
    /// The message is in Spam or Trash, or from a blocked sender. Its card
    /// shows, but nothing is answered from it until it is moved out.
    pub kept_out: bool,
    /// The titles of the user's other events at the same time.
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RsvpEventArgs {
    pub event_id: i64,
    /// accepted | tentative | declined
    pub response: String,
    /// A note to the organizer, sent with the answer.
    #[serde(default)]
    pub comment: Option<String>,
    /// Tell the organizer. Off records the answer quietly, where the
    /// calendar server lets that be chosen.
    #[serde(default = "default_true")]
    pub notify: bool,
    /// Answer one occurrence of a repeating event, the one with this
    /// RECURRENCE-ID (ms since the epoch, UTC), instead of the whole event.
    #[serde(default)]
    pub occurrence: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateEventArgs {
    pub event_id: i64,
    pub summary: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub join_url: Option<String>,
    pub starts_at: i64,
    pub ends_at: i64,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub attendees: Vec<Address>,
    /// Email an updated REQUEST ICS to attendees (organizer only).
    #[serde(default = "default_true")]
    pub notify: bool,
}

// Deliberately no `Debug`: it would print the CalDAV app password.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectCaldavArgs {
    pub account_id: i64,
    /// The CalDAV connection to change; None adds a new one.
    #[serde(default)]
    pub connection_id: Option<i64>,
    /// A server address, or one calendar's own address.
    pub url: String,
    #[serde(default)]
    pub username: String,
    /// Required for a new connection; an empty one keeps the saved password.
    #[serde(default)]
    pub password: Option<String>,
}

/// Standard folder roles.
pub mod roles {
    pub const INBOX: &str = "inbox";
    pub const ARCHIVE: &str = "archive";
    pub const SENT: &str = "sent";
    pub const DRAFTS: &str = "drafts";
    pub const TRASH: &str = "trash";
    pub const SPAM: &str = "spam";
    pub const ALL: &str = "all";
    pub const SNOOZED: &str = "snoozed";
    /// Where a JMAP server keeps a message it holds to send later.
    pub const SCHEDULED: &str = "scheduled";
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_are_listed_by_their_name_then_their_owner_then_their_address() {
        assert_eq!(
            account_label(Some("Shop"), Some("Ada Lovelace"), "ada@example.com"),
            "Shop"
        );
        assert_eq!(
            account_label(None, Some("Ada Lovelace"), "ada@example.com"),
            "Ada Lovelace"
        );
        assert_eq!(
            account_label(Some("  "), Some(""), "ada@example.com"),
            "ada@example.com"
        );
        assert_eq!(
            account_label(None, None, "ada@example.com"),
            "ada@example.com"
        );
    }

    #[test]
    fn typed_account_names_are_kept_on_one_line() {
        assert_eq!(clean_account_name("  Office  ").as_deref(), Some("Office"));
        assert_eq!(
            clean_account_name("Corner\n shop\t2").as_deref(),
            Some("Corner shop 2")
        );
        assert_eq!(clean_account_name(" \n "), None);
        assert_eq!(clean_account_name(""), None);
    }

    #[test]
    fn action_kind_string_roundtrip() {
        let kinds = [
            ActionKind::MarkRead,
            ActionKind::MarkUnread,
            ActionKind::Star,
            ActionKind::Unstar,
            ActionKind::Archive,
            ActionKind::Unarchive,
            ActionKind::Trash,
            ActionKind::DeletePermanently,
            ActionKind::Spam,
            ActionKind::NotSpam,
            ActionKind::Move,
            ActionKind::Copy,
            ActionKind::Snooze,
            ActionKind::Unsnooze,
            ActionKind::AddLabel,
            ActionKind::RemoveLabel,
        ];
        for k in kinds {
            assert_eq!(ActionKind::parse(k.as_str()), Some(k), "roundtrip {k:?}");
        }
        assert_eq!(ActionKind::parse("bogus"), None);
    }

    #[test]
    fn provider_string_roundtrip() {
        for p in [Provider::Generic, Provider::Gmail, Provider::Microsoft] {
            assert_eq!(Provider::parse(p.as_str()), Some(p));
        }
        assert_eq!(Provider::parse("imap"), None);
    }

    #[test]
    fn mail_protocol_roundtrip_rejects_unknown_values() {
        for protocol in [
            MailProtocol::Gmail,
            MailProtocol::Imap,
            MailProtocol::Jmap,
            MailProtocol::Pop3,
            MailProtocol::Local,
        ] {
            assert_eq!(MailProtocol::parse(protocol.as_str()), Some(protocol));
        }
        assert_eq!(MailProtocol::parse("future"), None);
        assert_eq!(MailProtocol::parse("JMAP"), None);
    }

    #[test]
    fn auth_kind_roundtrip_rejects_unknown_values() {
        for kind in [AuthKind::Password, AuthKind::Oauth2] {
            assert_eq!(AuthKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(AuthKind::parse("token"), None);
    }

    #[test]
    fn mark_read_choices_keep_their_names_and_waits() {
        use std::time::Duration;
        for choice in MarkRead::ALL {
            assert_eq!(MarkRead::parse(choice.as_str()), Some(choice));
            assert_eq!(serde_json::to_value(choice).unwrap(), choice.as_str());
        }
        assert_eq!(MarkRead::parse("sometimes"), None);
        assert_eq!(MarkRead::Immediately.delay(), Some(Duration::ZERO));
        assert_eq!(
            MarkRead::AfterTwoSeconds.delay(),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            MarkRead::AfterFiveSeconds.delay(),
            Some(Duration::from_secs(5))
        );
        assert_eq!(MarkRead::Never.delay(), None);
    }

    #[test]
    fn settings_serde_defaults_for_new_fields() {
        let s: Settings = serde_json::from_str(
            r#"{"theme":"snow","undoSendSeconds":5,"loadRemoteImages":false}"#,
        )
        .unwrap();
        assert!(s.notifications_enabled);
        assert!(s.auto_advance);
        assert!(s.auto_labels_enabled);
        assert!(s.signature_list.is_empty());
        // Nothing is preset for AI: no server, no model.
        assert_eq!(s.ai, AiSettings::default());
        assert!(!s.ai.ready());
        assert!(s.show_avatars);
        assert_eq!(s.message_appearance, MessageAppearance::System);
        assert_eq!(s.reduce_motion, ReduceMotion::System);
        for motion in [ReduceMotion::System, ReduceMotion::On, ReduceMotion::Off] {
            assert_eq!(ReduceMotion::parse(motion.as_str()), Some(motion));
        }
        assert_eq!(s.interface_text_size, InterfaceTextSize::Default);
        assert_eq!(InterfaceTextSize::Default.factor(), 1.0);
        for size in [
            InterfaceTextSize::Smaller,
            InterfaceTextSize::Default,
            InterfaceTextSize::Large,
            InterfaceTextSize::Larger,
            InterfaceTextSize::Largest,
        ] {
            assert_eq!(InterfaceTextSize::parse(size.as_str()), Some(size));
        }
        // Settings saved before the choice existed take the safe default.
        assert_eq!(s.add_invitations, AddInvitations::Known);
        for adding in [
            AddInvitations::Everyone,
            AddInvitations::Known,
            AddInvitations::Answered,
        ] {
            assert_eq!(AddInvitations::parse(adding.as_str()), Some(adding));
        }
        assert_eq!(s.app_icon, AppIcon::Orange);
        assert!(s.group_mail_by_date);
        // Rows stay as they were before the list could be made denser.
        assert_eq!(s.list_density, ListDensity::Comfortable);
        assert_eq!(s.preview_lines, 1);
        for density in [ListDensity::Comfortable, ListDensity::Compact] {
            assert_eq!(ListDensity::parse(density.as_str()), Some(density));
        }
        assert_eq!(ListDensity::parse("cozy"), None);
        assert!(s.show_tabs);
        assert!(s.conversation_view);
        // New installs draw their own frame; a saved choice stays.
        assert_eq!(s.window_frame, "integrated");
        let saved: Settings = serde_json::from_str(
            r#"{"theme":"system","undoSendSeconds":10,"windowFrame":"system"}"#,
        )
        .unwrap();
        assert_eq!(saved.window_frame, "system");
        assert_eq!(s.workspace_layout, "default");
        assert_eq!(s.workspace_list_pane_width, MIN_WORKSPACE_LIST_PANE_WIDTH);
        assert_eq!(s.time_format, TimeFormat::Automatic);
        // Spelling is checked in the system's languages until the user picks theirs.
        assert!(s.spelling_enabled);
        assert_eq!(s.spelling_languages, None);
        let chosen: Settings = serde_json::from_str(
            r#"{"theme":"system","undoSendSeconds":10,"spellingLanguages":[]}"#,
        )
        .unwrap();
        assert_eq!(chosen.spelling_languages, Some(Vec::new()));
    }

    #[test]
    fn time_formats_round_trip_through_their_stored_names() {
        for format in [
            TimeFormat::Automatic,
            TimeFormat::TwelveHour,
            TimeFormat::TwentyFourHour,
        ] {
            assert_eq!(TimeFormat::parse(format.as_str()), Some(format));
            let settings = Settings {
                time_format: format,
                ..Settings::default()
            };
            let serialized = serde_json::to_value(&settings).unwrap();
            assert_eq!(serialized["timeFormat"], format.as_str());
            let restored: Settings = serde_json::from_value(serialized).unwrap();
            assert_eq!(restored.time_format, format);
        }
        assert_eq!(TimeFormat::parse("system"), None);
    }

    #[test]
    fn app_icon_round_trips_and_tolerates_unknown_values() {
        for icon in AppIcon::ALL {
            assert_eq!(AppIcon::parse(icon.as_str()), Some(icon));
            let json = serde_json::to_string(&icon).unwrap();
            assert_eq!(json, format!("\"{}\"", icon.as_str()));
            assert_eq!(serde_json::from_str::<AppIcon>(&json).unwrap(), icon);
        }
        assert_eq!(AppIcon::parse("Graphite"), None);
        let settings: Settings = serde_json::from_str(
            r#"{"theme":"system","undoSendSeconds":10,"appIcon":"from-a-newer-version"}"#,
        )
        .unwrap();
        assert_eq!(settings.app_icon, AppIcon::Orange);
    }

    #[test]
    fn workspace_layout_accepts_legacy_mail_layout_key() {
        let settings: Settings = serde_json::from_str(
            r#"{"theme":"system","undoSendSeconds":10,"mailLayout":"minimal"}"#,
        )
        .unwrap();
        assert_eq!(settings.workspace_layout, "minimal");

        let serialized = serde_json::to_value(Settings::default()).unwrap();
        assert_eq!(serialized["workspaceLayout"], "default");
        assert_eq!(serialized["groupMailByDate"], true);
        assert_eq!(serialized["showTabs"], true);
        assert_eq!(serialized["conversationView"], true);
        assert_eq!(
            serialized["workspaceListPaneWidth"],
            MIN_WORKSPACE_LIST_PANE_WIDTH
        );
        assert!(serialized.get("mailLayout").is_none());
    }

    #[test]
    fn workspace_list_pane_width_is_bounded() {
        assert_eq!(
            normalized_workspace_list_pane_width(120),
            MIN_WORKSPACE_LIST_PANE_WIDTH
        );
        assert_eq!(normalized_workspace_list_pane_width(640), 640);
        assert_eq!(
            normalized_workspace_list_pane_width(i64::MAX),
            MAX_WORKSPACE_LIST_PANE_WIDTH
        );
    }
}
