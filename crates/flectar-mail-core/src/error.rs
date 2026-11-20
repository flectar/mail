use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("network error: {0}")]
    Network(String),
    /// A response body exceeded the size the caller accepts. Classified like
    /// other network errors, but distinguishable so a caller can request less.
    #[error("network error: {context} exceeded the {max_bytes}-byte response limit")]
    ResponseTooLarge {
        context: &'static str,
        max_bytes: usize,
    },
    #[error("imap error: {0}")]
    Imap(String),
    /// The server answered the command with a tagged NO or BAD, as in
    /// `NO [NONEXISTENT] Unknown Mailbox`. Only the command failed: the
    /// connection stays in step and serves the next one (RFC 9051 7.1).
    #[error("imap error: {0}")]
    ImapRefused(String),
    /// A selective FETCH response could not be parsed. Its session must be
    /// discarded; explicit opens may retry using the complete message.
    #[error("imap response parse error: {0}")]
    ImapParse(String),
    #[error("jmap error: {0}")]
    Jmap(String),
    #[error("pop3 error: {0}")]
    Pop3(String),
    #[error("send status uncertain: {0}")]
    SendUncertain(String),
    #[error("smtp error: {0}")]
    Smtp(String),
    #[error("tls error: {0}")]
    Tls(String),
    #[error("auth failed: {0}")]
    Auth(String),
    #[error("account needs re-authentication")]
    NeedsReauth,
    #[error("secure credential storage is unavailable: {0}")]
    CredentialStoreUnavailable(String),
    #[error("keyring error: {0}")]
    Keyring(String),
    #[error("mime error: {0}")]
    Mime(String),
    #[error("caldav error: {0}")]
    CalDav(String),
    #[error("carddav error: {0}")]
    CardDav(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("offline")]
    Offline,
    /// The server asked to slow down or wait: a rate limit or quota for now,
    /// too many open connections, or a delay between sign-ins. Sync waits
    /// before it asks again, as long as the server said if it said.
    #[error("{service} asked to wait: {detail}")]
    RateLimited {
        /// Who asked, such as the server's host name or "Gmail".
        service: String,
        kind: LimitKind,
        /// The wait the server named, if it named one.
        retry_after: Option<std::time::Duration>,
        /// The server's own words.
        detail: String,
    },
    #[error("AI is not configured")]
    AiNotConfigured,
    #[error("{0}")]
    Translation(crate::translate::TranslationError),
    #[error("{0}")]
    Other(String),
}

impl CoreError {
    /// Stable, language-agnostic token identifying the error variant. A UI may
    /// map this to localized copy; the human-readable display string remains a
    /// fallback.
    pub fn code(&self) -> &'static str {
        match self {
            CoreError::Db(_) => "db",
            CoreError::Io(_) => "io",
            CoreError::Network(_) | CoreError::ResponseTooLarge { .. } => "network",
            CoreError::Imap(_) | CoreError::ImapRefused(_) | CoreError::ImapParse(_) => "imap",
            CoreError::Jmap(_) => "jmap",
            CoreError::Pop3(_) => "pop3",
            CoreError::SendUncertain(_) => "send_uncertain",
            CoreError::Smtp(_) => "smtp",
            CoreError::Tls(_) => "tls",
            CoreError::Auth(_) => "auth",
            CoreError::NeedsReauth => "needs_reauth",
            CoreError::CredentialStoreUnavailable(_) => "credential_store_unavailable",
            CoreError::Keyring(_) => "keyring",
            CoreError::Mime(_) => "mime",
            CoreError::CalDav(_) => "caldav",
            CoreError::CardDav(_) => "carddav",
            CoreError::NotFound(_) => "not_found",
            CoreError::Offline => "offline",
            CoreError::RateLimited { .. } => "rate_limited",
            CoreError::AiNotConfigured => "ai_not_configured",
            CoreError::Translation(_) => "translation",
            CoreError::Other(_) => "other",
        }
    }

    /// Serialize the stable code and fallback message for a UI-facing result.
    pub fn to_client_json(&self) -> String {
        serde_json::json!({ "code": self.code(), "message": self.to_string() }).to_string()
    }
}

/// What a server's request to wait was about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    /// Too many requests, or too much data, for now: a rate limit or a
    /// quota that refills.
    Requests,
    /// Too many connections open at once.
    Connections,
    /// Signed in too recently: POP3's LOGIN-DELAY (RFC 2449).
    LoginDelay,
    /// Something else holds the mailbox for now, as with RFC 5530's INUSE
    /// and POP3's IN-USE.
    Busy,
}

impl LimitKind {
    /// Whether a mail server's words ask to slow down, and about what.
    /// Servers say it in their own words: Gmail's "Too many simultaneous
    /// connections" and "Account exceeded command or bandwidth limits",
    /// Dovecot's "Maximum number of connections from user+IP exceeded",
    /// Yahoo's "Rate limit hit" and Exchange Online's "Request is throttled".
    pub(crate) fn in_text(text: &str) -> Option<Self> {
        let text = text.to_ascii_lowercase();
        let says = |words: &[&str]| words.iter().any(|word| text.contains(word));
        if says(&[
            "too many simultaneous connections",
            "too many connections",
            "maximum number of connections",
        ]) {
            Some(Self::Connections)
        } else if says(&[
            "rate limit",
            "throttled",
            "exceeded command or bandwidth",
            "bandwidth limit",
            "too many requests",
        ]) {
            Some(Self::Requests)
        } else {
            None
        }
    }
}

impl From<anyhow::Error> for CoreError {
    fn from(e: anyhow::Error) -> Self {
        CoreError::Other(e.to_string())
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(e: serde_json::Error) -> Self {
        CoreError::Other(format!("json: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;
