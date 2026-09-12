use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("network error: {0}")]
    Network(String),
    #[error("imap error: {0}")]
    Imap(String),
    /// A selective FETCH response could not be parsed. Its session must be
    /// discarded; explicit opens may retry using the complete message.
    #[error("imap response parse error: {0}")]
    ImapParse(String),
    #[error("jmap error: {0}")]
    Jmap(String),
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
    #[error("not found: {0}")]
    NotFound(String),
    #[error("offline")]
    Offline,
    #[error("AI is not configured")]
    AiNotConfigured,
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
            CoreError::Network(_) => "network",
            CoreError::Imap(_) | CoreError::ImapParse(_) => "imap",
            CoreError::Jmap(_) => "jmap",
            CoreError::SendUncertain(_) => "send_uncertain",
            CoreError::Smtp(_) => "smtp",
            CoreError::Tls(_) => "tls",
            CoreError::Auth(_) => "auth",
            CoreError::NeedsReauth => "needs_reauth",
            CoreError::CredentialStoreUnavailable(_) => "credential_store_unavailable",
            CoreError::Keyring(_) => "keyring",
            CoreError::Mime(_) => "mime",
            CoreError::CalDav(_) => "caldav",
            CoreError::NotFound(_) => "not_found",
            CoreError::Offline => "offline",
            CoreError::AiNotConfigured => "ai_not_configured",
            CoreError::Other(_) => "other",
        }
    }

    /// Serialize the stable code and fallback message for a UI-facing result.
    pub fn to_client_json(&self) -> String {
        serde_json::json!({ "code": self.code(), "message": self.to_string() }).to_string()
    }
}

impl From<anyhow::Error> for CoreError {
    fn from(e: anyhow::Error) -> Self {
        CoreError::Other(e.to_string())
    }
}

impl From<keyring::Error> for CoreError {
    fn from(e: keyring::Error) -> Self {
        match e {
            keyring::Error::PlatformFailure(error) => {
                tracing::warn!(%error, "platform credential service failed");
                CoreError::CredentialStoreUnavailable(
                    "the system keyring could not be reached; start or unlock it and try again"
                        .into(),
                )
            }
            keyring::Error::NoStorageAccess(error) => {
                tracing::warn!(%error, "platform credential service denied access");
                CoreError::CredentialStoreUnavailable(
                    "the system keyring is locked or denied access; unlock it and try again".into(),
                )
            }
            error => CoreError::Keyring(error.to_string()),
        }
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(e: serde_json::Error) -> Self {
        CoreError::Other(format!("json: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;
