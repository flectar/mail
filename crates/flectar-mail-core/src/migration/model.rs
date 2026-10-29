//! Source-neutral description of what another mail client stores.
//!
//! Adapters translate a foreign format into these types without deciding what
//! Flectar can use. Every value keeps its source meaning (an unencrypted
//! connection stays unencrypted, a separate outgoing login stays separate) so
//! the planner can explain differences instead of silently rewriting them.

/// The application or file format an inventory was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Thunderbird for Android or K-9 Mail settings export (`.k9s`).
    K9Settings,
    /// A Thunderbird desktop profile (`prefs.js`).
    ThunderbirdProfile,
}

impl SourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::K9Settings => "k9",
            Self::ThunderbirdProfile => "thunderbird",
        }
    }
}

/// Everything an adapter found in one source, before any selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportInventory {
    pub source: SourceKind,
    /// Format version declared by the source, when it has one.
    pub source_version: Option<String>,
    pub accounts: Vec<AccountCandidate>,
    /// Mail the source keeps on its device, which can be imported too.
    pub mail: Vec<MailStore>,
    /// Source entries that can't be imported (feeds, newsgroups, local
    /// folders kept outside the profile). They are listed so the summary can
    /// say what was left out.
    pub other_entries: Vec<OtherEntry>,
}

/// Mail a source app keeps on its device: Thunderbird's Local Folders, or
/// what one of its POP3 accounts downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailStore {
    /// The [`AccountCandidate::source_id`] of the account whose mail it is,
    /// or `None` for mail that belongs to no account.
    pub account: Option<String>,
    /// The name the source shows for it.
    pub label: String,
    pub mail: crate::mail_import::MailSource,
}

/// One mail account as the source application configured it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountCandidate {
    /// Stable identifier inside the source (K-9 UUID, Thunderbird account key).
    pub source_id: String,
    /// The name the user gave the account in the source application.
    pub label: Option<String>,
    /// Sender identities in the source's order; the first is the default.
    pub identities: Vec<IdentityCandidate>,
    pub incoming: Option<ServerCandidate>,
    pub outgoing: Option<ServerCandidate>,
    /// Account marker color as `#RRGGBB`.
    pub color: Option<String>,
    /// What the source does with mail it downloads with POP3. `None` for
    /// other incoming protocols.
    pub retention: Option<SourceRetention>,
}

/// How long the source leaves mail it downloads on a POP3 server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRetention {
    /// Downloaded mail stays on the server. Otherwise it is removed as soon
    /// as it is downloaded.
    pub keep: bool,
    /// While kept, mail is removed this many days after it was downloaded.
    pub remove_after_days: Option<u32>,
    /// While kept, mail is removed once it is deleted in the source.
    pub remove_deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityCandidate {
    pub email: String,
    pub display_name: Option<String>,
    pub reply_to: Option<String>,
    pub signature: Option<SignatureCandidate>,
    /// The identity sends through a different outgoing server than the
    /// account's default identity.
    pub separate_outgoing_server: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureCandidate {
    Text(String),
    Html(String),
    /// The source loads the signature from a file outside the selected input.
    ExternalFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCandidate {
    pub protocol: ServerProtocol,
    pub host: String,
    /// `None` when the source relies on the protocol's default port.
    pub port: Option<u16>,
    pub security: SourceSecurity,
    pub auth: SourceAuth,
    pub username: String,
}

impl ServerCandidate {
    /// The port the source application would connect to.
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(match (self.protocol, self.security) {
            (ServerProtocol::Imap, SourceSecurity::Tls) => 993,
            (ServerProtocol::Imap, _) => 143,
            (ServerProtocol::Pop3, SourceSecurity::Tls) => 995,
            (ServerProtocol::Pop3, _) => 110,
            (ServerProtocol::Smtp, SourceSecurity::Tls) => 465,
            (ServerProtocol::Smtp, _) => 587,
            (ServerProtocol::Ews | ServerProtocol::Other, _) => 443,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerProtocol {
    Imap,
    Pop3,
    Smtp,
    /// Exchange Web Services.
    Ews,
    /// Anything else the source supports (WebDAV, proprietary add-ons).
    Other,
}

/// Transport security exactly as configured in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceSecurity {
    Tls,
    Starttls,
    /// Unencrypted, or encryption only when the server offers it.
    None,
    /// A value this reader does not recognize.
    Unknown,
}

/// Sign-in method exactly as configured in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAuth {
    /// A password sent inside the encrypted connection (PLAIN or LOGIN).
    Password,
    /// A challenge-response password mechanism such as CRAM-MD5.
    EncryptedPassword,
    /// Whatever password mechanism the server offers.
    Automatic,
    OAuth2,
    ClientCertificate,
    Kerberos,
    Ntlm,
    None,
    /// A value this reader does not recognize.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherEntry {
    pub kind: OtherEntryKind,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtherEntryKind {
    /// Local folders whose files aren't in what was chosen: kept outside the
    /// profile, or only `prefs.js` was chosen.
    LocalFolders,
    Feeds,
    Newsgroups,
    Unknown,
}
