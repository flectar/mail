//! Data models shared by the core and native application. Serialized field
//! names remain stable for persisted JSON and provider-facing payloads.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Imap,
    Gmail,
    Microsoft,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Imap => "imap",
            Provider::Gmail => "gmail",
            Provider::Microsoft => "microsoft",
        }
    }
    pub fn from_storage(s: &str) -> Self {
        match s {
            "gmail" => Provider::Gmail,
            "microsoft" => Provider::Microsoft,
            _ => Provider::Imap,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    Password,
    Oauth2,
}

/// Remote mail transport used by an account. Provider identifies the service
/// (Google, Microsoft, or a standards-based server); this value identifies the
/// wire protocol. Keeping them separate lets Stalwart and other generic
/// providers use JMAP without pretending to be a different provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailProtocol {
    /// RFC 8620/8621 JMAP Mail and EmailSubmission.
    Jmap,
    /// IMAP access plus SMTP submission compatibility mode.
    #[default]
    Imap,
}

impl MailProtocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jmap => "jmap",
            Self::Imap => "imap",
        }
    }

    pub fn from_storage(value: &str) -> Self {
        if value.eq_ignore_ascii_case("jmap") {
            Self::Jmap
        } else {
            Self::Imap
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
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthKind::Password => "password",
            AuthKind::Oauth2 => "oauth2",
        }
    }
    pub fn from_storage(s: &str) -> Self {
        if s == "oauth2" {
            AuthKind::Oauth2
        } else {
            AuthKind::Password
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

/// Typed contents of `accounts.settings_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSettings {
    // Missing fields can occur in imported development-era configuration.
    // Apply the same bounded default as a newly created account so an absent
    // value never triggers an unbounded mailbox download.
    #[serde(default = "default_mail_history")]
    pub mail_history: MailHistory,
}

impl Default for AccountSettings {
    fn default() -> Self {
        Self {
            mail_history: default_mail_history(),
        }
    }
}

const fn default_mail_history() -> MailHistory {
    MailHistory::SixMonths
}

#[cfg(test)]
mod account_settings_tests {
    use super::*;

    #[test]
    fn new_missing_and_imported_settings_share_the_bounded_default() {
        assert_eq!(
            AccountSettings::default().mail_history,
            MailHistory::SixMonths
        );
        let missing: AccountSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(missing.mail_history, MailHistory::SixMonths);

        let imported: PortableAccountConfig = serde_json::from_str(
            r#"{
                "email":"person@example.com",
                "displayName":null,
                "provider":"imap",
                "authKind":"password",
                "username":"person",
                "imapHost":"imap.example.com",
                "imapPort":993,
                "smtpHost":"smtp.example.com",
                "smtpPort":465
            }"#,
        )
        .unwrap();
        assert_eq!(imported.settings.mail_history, MailHistory::SixMonths);
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
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub provider: Provider,
    pub auth_kind: AuthKind,
    pub mail_protocol: MailProtocol,
    pub sync_state: String,
    pub sync_error: Option<String>,
}

/// Full account row including server config; internal to the native application.
#[derive(Debug, Clone)]
pub struct AccountConfig {
    pub id: i64,
    pub email: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub provider: Provider,
    pub auth_kind: AuthKind,
    pub mail_protocol: MailProtocol,
    pub username: String,
    /// HTTPS origin used to discover `/.well-known/jmap`.
    pub jmap_url: String,
    /// Selected JMAP Mail account id advertised by the Session resource.
    pub jmap_account_id: Option<String>,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub settings: AccountSettings,
}

/// Portable account connection metadata used by backup/restore. Credentials,
/// cached mail, and local database ids are intentionally not represented.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PortableAccountConfig {
    pub email: String,
    pub display_name: Option<String>,
    pub provider: Provider,
    pub auth_kind: AuthKind,
    #[serde(default)]
    pub mail_protocol: MailProtocol,
    pub username: String,
    #[serde(default)]
    pub jmap_url: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    #[serde(default)]
    pub settings: AccountSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

/// A contact matched by search suggestions, with its interaction affinity
/// (send_count*3 + recv_count) so the UI can show how well-known it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactSuggestion {
    pub name: Option<String>,
    pub email: String,
    pub interactions: i64,
}

/// A complete address-book entry. Mail sync owns the interaction metadata;
/// the remaining fields can be edited directly from the contacts workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContactRecord {
    pub id: i64,
    pub name: String,
    pub email: String,
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
    /// User-managed contacts remain available from every account scope, just
    /// like account-scoped compose autocomplete.
    #[serde(default)]
    pub is_managed: bool,
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
    pub matching_count: usize,
    pub total_count: usize,
    pub favorite_count: usize,
    pub account_counts: Vec<(i64, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    Inbox,
    Starred,
    Snoozed,
    Sent,
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
    pub account_email: String,
    pub subject: String,
    pub snippet: String,
    pub participants: Vec<Address>,
    pub last_message_at: i64,
    pub message_count: i64,
    pub unread_count: i64,
    pub is_starred: bool,
    pub has_attachments: bool,
    pub has_replied: bool,
    pub snoozed_until: Option<i64>,
    /// Ids of labels present on any message in the thread.
    pub labels: Vec<i64>,
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
    /// reverting to a plain draft.
    pub send_state: Option<String>,
    /// The last delivery error when `send_state` is `"failed"`.
    pub send_error: Option<String>,
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

/// Stable keyset cursor for the descending thread timeline. Timestamps alone
/// are not unique, so the thread id is the deterministic tie-breaker.
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

// Deliberately no `Debug`: it would print the account password.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddPasswordAccountArgs {
    pub email: String,
    pub display_name: Option<String>,
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub mail_protocol: MailProtocol,
    #[serde(default)]
    pub jmap_url: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
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
    Spam,
    NotSpam,
    Move,
    Snooze,
    Unsnooze,
    AddLabel,
    RemoveLabel,
    Send,
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
            ActionKind::Spam => "spam",
            ActionKind::NotSpam => "not_spam",
            ActionKind::Move => "move",
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
            "spam" => ActionKind::Spam,
            "not_spam" => ActionKind::NotSpam,
            "move" => ActionKind::Move,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformActionArgs {
    pub kind: ActionKind,
    pub thread_ids: Vec<i64>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveDraftArgs {
    pub draft_id: Option<i64>,
    pub account_id: i64,
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
    #[serde(default)]
    pub attachments: Vec<DraftAttachmentIn>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSendArgs {
    pub draft_id: i64,
    pub send_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSendResult {
    pub action_id: i64,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitRuleQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub senders: Option<Vec<String>>,
    /// Never match threads from these addresses/domains, even when every
    /// positive criterion holds. Exclusions alone never make a rule match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_senders: Option<Vec<String>>,
    /// Match a recipient (To or Cc) address/domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipients: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_contains: Option<Vec<String>>,
    /// Match threads carrying any of these user (non-auto) label ids. Lets an
    /// AI-applied label like "INVOICE" route mail into a tab even when the
    /// subject holds no keyword.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_automated: Option<bool>,
    /// Match threads that have (true) or lack (false) an attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_attachment: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitRule {
    pub id: i64,
    pub name: String,
    pub position: i64,
    pub query: SplitRuleQuery,
    /// Where matching mail is routed. `None` = the rule is its own tab (legacy,
    /// route key `"split:<id>"`); otherwise a route key: `"important"`,
    /// `"other"`, or `"label:<id>"` to drop matches into an existing tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderInfo {
    pub id: i64,
    pub account_id: i64,
    /// Unicode name suitable for display. `imap_name` remains the exact remote
    /// identifier used in SELECT and other protocol commands.
    pub display_name: String,
    pub is_jmap: bool,
    pub imap_name: String,
    /// IMAP hierarchy delimiter (e.g. "/" or "."), for nesting user folders.
    pub delimiter: Option<String>,
    pub role: Option<String>,
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

/// Exact unread badge counts for split tabs and sidebar rows.
/// Map keys are stringified ids (JSON object keys must be strings).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnreadCounts {
    pub inbox: i64,
    pub important: i64,
    pub other: i64,
    pub splits: std::collections::HashMap<String, i64>,
    pub labels: std::collections::HashMap<String, i64>,
    /// "starred" | "snoozed" | "drafts" (drafts counts all drafts, not unread)
    pub views: std::collections::HashMap<String, i64>,
}

/// The badges rendered by the native mailbox sidebar for one account.
/// Inbox and Starred count unread threads; Drafts counts all draft threads.
/// Keeping this projection small lets the native shell refresh every account
/// with one grouped SQLite query after a mail mutation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MailboxBadgeCounts {
    pub account_id: i64,
    pub inbox: i64,
    pub starred: i64,
    pub drafts: i64,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub theme: String,
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
    /// Workspace presentation: "default" keeps lists and details side by
    /// side, while "minimal" navigates between those shared surfaces.
    /// `mailLayout` was the short-lived original key before Contacts shared
    /// the preference; accept it so development builds upgrade cleanly.
    #[serde(default = "default_workspace_layout", alias = "mailLayout")]
    pub workspace_layout: String,
    /// Render mailbox and folder icons with a single neutral color instead of
    /// the default semantic palette.
    #[serde(default)]
    pub monochrome_sidebar_icons: bool,
    /// UI language: "system" follows the OS locale, otherwise a code like "en".
    #[serde(default = "default_language")]
    pub language: String,
    /// First day shown in calendar week and month views: "sunday" | "monday".
    #[serde(default = "default_calendar_week_start")]
    pub calendar_week_start: String,
    pub undo_send_seconds: i64,
    #[serde(default)]
    pub load_remote_images: bool,
    /// Mark unread conversations as read when the user opens them.
    #[serde(default = "default_true")]
    pub mark_read_on_open: bool,
    #[serde(default = "default_ai_base_url")]
    pub ai_base_url: String,
    /// Per-tier model ids, all sharing `ai_base_url` and the stored API key.
    #[serde(default = "default_ai_model")]
    pub ai_model_instant: String,
    #[serde(default = "default_ai_model")]
    pub ai_model_cheap: String,
    #[serde(default = "default_ai_model")]
    pub ai_model_intelligent: String,
    /// Which tier each AI scenario uses: "instant" | "cheap" | "intelligent".
    #[serde(default = "default_tier_intelligent")]
    pub ai_tier_ask: String,
    #[serde(default = "default_tier_intelligent")]
    pub ai_tier_draft: String,
    #[serde(default = "default_tier_instant")]
    pub ai_tier_summarize: String,
    #[serde(default = "default_tier_cheap")]
    pub ai_tier_voice: String,
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
    /// Which model tier the AI classifier uses: "instant" | "cheap" | "intelligent".
    #[serde(default = "default_tier_instant")]
    pub ai_tier_categorize: String,
    /// Group the thread list under date headers (Today / Yesterday / …).
    #[serde(default = "default_true")]
    pub group_by_date: bool,
    /// When true, compose "To" autocomplete suggests contacts from every account;
    /// off (default) scopes suggestions to the account you're sending from.
    #[serde(default)]
    pub contact_suggest_all_accounts: bool,
    /// Show the unread count on the app icon (macOS Dock badge).
    #[serde(default = "default_true")]
    pub dock_badge_enabled: bool,
    /// Which count the badge shows: "inbox" (all unread) | "important".
    #[serde(default = "default_badge_source")]
    pub dock_badge_source: String,
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
    /// Show which account each unified-inbox row belongs to (dot + short name).
    #[serde(default = "default_true")]
    pub show_account_badges: bool,
    /// Per-account marker color (hex), keyed by stringified account id.
    /// Missing = the UI derives a stable hue from the address.
    #[serde(default)]
    pub account_colors: std::collections::HashMap<String, String>,
    /// Short name shown on inbox rows, keyed by stringified account id.
    /// Missing = display name, else the address local part.
    #[serde(default)]
    pub account_short_names: std::collections::HashMap<String, String>,
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
fn default_ai_base_url() -> String {
    crate::ai::DEFAULT_BASE_URL.into()
}
fn default_ai_model() -> String {
    crate::ai::DEFAULT_MODEL.into()
}
fn default_tier_intelligent() -> String {
    "intelligent".into()
}
fn default_tier_instant() -> String {
    "instant".into()
}
fn default_tier_cheap() -> String {
    "cheap".into()
}
fn default_badge_source() -> String {
    "inbox".into()
}
fn default_notification_scope() -> String {
    "important".into()
}
fn default_workspace_layout() -> String {
    "default".into()
}
fn default_theme_preset() -> String {
    "default".into()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "system".into(),
            theme_preset: default_theme_preset(),
            custom_theme: CustomTheme::default(),
            show_avatars: true,
            workspace_layout: default_workspace_layout(),
            monochrome_sidebar_icons: false,
            language: "system".into(),
            calendar_week_start: default_calendar_week_start(),
            undo_send_seconds: 10,
            load_remote_images: false,
            mark_read_on_open: true,
            ai_base_url: default_ai_base_url(),
            ai_model_instant: default_ai_model(),
            ai_model_cheap: default_ai_model(),
            ai_model_intelligent: default_ai_model(),
            ai_tier_ask: default_tier_intelligent(),
            ai_tier_draft: default_tier_intelligent(),
            ai_tier_summarize: default_tier_instant(),
            ai_tier_voice: default_tier_cheap(),
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
            ai_tier_categorize: default_tier_instant(),
            group_by_date: true,
            contact_suggest_all_accounts: false,
            dock_badge_enabled: true,
            dock_badge_source: default_badge_source(),
            notification_scope: default_notification_scope(),
            notification_tabs: Vec::new(),
            signature_list: Vec::new(),
            signature_defaults: std::collections::HashMap::new(),
            account_themes: std::collections::HashMap::new(),
            show_account_badges: true,
            account_colors: std::collections::HashMap::new(),
            account_short_names: std::collections::HashMap::new(),
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
    pub configured: bool,
    pub model_instant: String,
    pub model_cheap: String,
    pub model_intelligent: String,
    pub base_url: String,
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
}

/// A discovered CalDAV calendar collection.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    pub id: i64,
    pub account_id: i64,
    pub url: String,
    pub display_name: Option<String>,
    pub color: Option<String>,
    pub read_only: bool,
    pub enabled: bool,
    pub is_default: bool,
    pub last_synced_at: Option<i64>,
}

/// Per-account calendar-provider connection shown in account settings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarConnection {
    pub account_id: i64,
    /// google | microsoft | generic
    pub kind: String,
    pub enabled: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventAttendee {
    pub email: String,
    pub name: Option<String>,
    /// NEEDS-ACTION | ACCEPTED | TENTATIVE | DECLINED
    pub partstat: Option<String>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RsvpEventArgs {
    pub event_id: i64,
    /// accepted | tentative | declined
    pub response: String,
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
pub struct ConnectCalendarArgs {
    pub account_id: i64,
    /// "google" | "generic"
    pub kind: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

/// Standard IMAP folder roles.
pub mod roles {
    pub const INBOX: &str = "inbox";
    pub const ARCHIVE: &str = "archive";
    pub const SENT: &str = "sent";
    pub const DRAFTS: &str = "drafts";
    pub const TRASH: &str = "trash";
    pub const SPAM: &str = "spam";
    pub const ALL: &str = "all";
    pub const SNOOZED: &str = "snoozed";
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            ActionKind::Spam,
            ActionKind::NotSpam,
            ActionKind::Move,
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
        for p in [Provider::Imap, Provider::Gmail, Provider::Microsoft] {
            assert_eq!(Provider::from_storage(p.as_str()), p);
        }
    }

    #[test]
    fn mail_protocol_roundtrip_defaults_unknown_storage_to_imap() {
        for protocol in [MailProtocol::Imap, MailProtocol::Jmap] {
            assert_eq!(MailProtocol::from_storage(protocol.as_str()), protocol);
        }
        assert_eq!(MailProtocol::from_storage("future"), MailProtocol::Imap);
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
        assert_eq!(s.ai_base_url, crate::ai::DEFAULT_BASE_URL);
        assert!(s.show_avatars);
        assert_eq!(s.workspace_layout, "default");
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
        assert!(serialized.get("mailLayout").is_none());
    }
}
