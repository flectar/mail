use crate::favicon::domain_from_address;
use crate::mail_scope::{LabelKind, Scope, StandardMailbox};
use flectar_mail_core::{
    Core,
    config::Paths,
    events::CoreEvent,
    models::{
        Account, AccountConfig, ActionKind, ActionParams, AddPasswordAccountArgs, Address,
        CalendarConnection, CalendarEvent, CardDavConnection, ConnectCalendarArgs,
        ConnectCardDavArgs, ContactRecordCursor, ContactRecordPage, CreateEventArgs, CustomTheme,
        DraftAttachmentIn, FolderInfo, Label, MailHistory, MailProfile, MailProtocol,
        MailboxBadgeCounts, MessageDetail, PerformActionArgs, PortableAccountConfig, Provider,
        QueueSendArgs, QueueSendResult, SaveDraftArgs, SearchCursor, Settings, Snippet,
        ThreadCursor, ThreadSummary, View, normalized_workspace_list_pane_width,
    },
};
#[cfg(test)]
use pulldown_cmark::{Event, Tag, TagEnd};
use pulldown_cmark::{Options, Parser, html};
use std::sync::Arc;

#[cfg(test)]
#[derive(serde::Deserialize)]
pub struct EmailFixture {
    pub id: i32,
    pub account: String,
    pub folder: String,
    pub sender: String,
    pub address: String,
    pub initials: String,
    pub subject: String,
    pub preview: String,
    #[serde(default)]
    pub to: String,
    #[serde(rename = "label_summary")]
    pub label: String,
    pub unread: bool,
    pub starred: bool,
    pub has_attachments: bool,
    #[serde(default)]
    pub has_replied: bool,
    #[serde(skip)]
    pub html: String,
}

/// Owned view data used by the Slint shell.
#[derive(Clone, Debug)]
pub struct MailMessage {
    pub id: i32,
    pub thread_id: Option<i64>,
    pub account_id: i64,
    pub account: String,
    pub folder: String,
    pub sender: String,
    pub address: String,
    pub domain: String,
    pub initials: String,
    pub subject: String,
    pub preview: String,
    /// Milliseconds since the Unix epoch. Views format it when they project
    /// the message, so the text follows the current language.
    pub date_ms: i64,
    pub to: String,
    pub label: String,
    pub unread: bool,
    pub starred: bool,
    pub has_attachments: bool,
    pub message_count: i64,
    pub attachments: Vec<flectar_mail_core::models::AttachmentMeta>,
    pub has_replied: bool,
    pub is_outgoing: bool,
    pub labels: Vec<i64>,
    pub html: Option<String>,
    pub text: Option<String>,
    /// The row is showing its snippet while the core fetches the real MIME
    /// body. The shell uses this to retry a missed/late MailUpdated event.
    pub body_pending: bool,
    /// Empty, or the strongest receiver-reported authentication kind:
    /// domain, microsoft, bimi, or brand.
    pub sender_verification: String,
}

impl MailMessage {
    #[cfg(test)]
    pub fn from_fixture(email: EmailFixture) -> Self {
        Self {
            id: email.id,
            thread_id: None,
            account_id: -1,
            account: email.account.to_owned(),
            folder: email.folder.to_owned(),
            sender: email.sender.to_owned(),
            address: email.address.to_owned(),
            domain: domain_from_address(&email.address).unwrap_or_default(),
            initials: email.initials.to_owned(),
            subject: email.subject.to_owned(),
            preview: email.preview.to_owned(),
            date_ms: (chrono::Local::now() - chrono::Duration::days(i64::from(email.id - 1)))
                .timestamp_millis(),
            to: email.to.to_owned(),
            label: email.label.to_owned(),
            unread: email.unread,
            starred: email.starred,
            has_attachments: email.has_attachments,
            message_count: 1,
            attachments: Vec::new(),
            has_replied: email.has_replied,
            is_outgoing: false,
            labels: Vec::new(),
            html: Some(email.html.to_owned()),
            text: None,
            body_pending: false,
            sender_verification: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MailboxEntry {
    pub account_id: i64,
    pub folder_id: i64,
    pub parent_folder_id: i64,
    pub depth: usize,
    pub has_children: bool,
    pub is_standard: bool,
    pub is_selectable: bool,
    pub can_create_children: bool,
    pub can_rename: bool,
    pub can_delete: bool,
    pub label: String,
    /// Absent for account headings, which are not navigation targets.
    pub scope: Option<Scope>,
    pub context: String,
    pub detail: String,
    pub avatar: String,
    pub is_account: bool,
    pub count: String,
}

/// Borrowed compose fields passed together so draft and send operations share
/// one validated input shape without a long, error-prone positional argument
/// list.
pub struct ComposeMessage<'a> {
    pub draft_id: Option<i64>,
    pub account_id: i64,
    pub sender_email: Option<&'a str>,
    pub to: &'a str,
    pub cc: &'a str,
    pub bcc: &'a str,
    pub subject: &'a str,
    pub body: &'a str,
    pub body_html: Option<&'a str>,
    pub attachments: &'a [DraftAttachmentIn],
    pub mode: &'a str,
    pub in_reply_to_message_id: Option<i64>,
}

/// The latest concrete message in a selected thread, with the addressing data
/// needed to start a reply or forward without teaching the Slint view about
/// provider/thread internals.
#[derive(Clone, Debug)]
pub struct ComposeSource {
    pub message_id: i64,
    pub account_id: i64,
    pub account_email: String,
    pub sender_identities: Vec<flectar_mail_core::models::SenderIdentity>,
    pub default_sender_email: String,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub date: i64,
    pub text_body: String,
}

#[derive(Clone, Debug)]
pub struct MailPage {
    pub account_revision: u64,
    pub messages: Vec<MailMessage>,
    pub labels: Vec<Label>,
    pub mailboxes: Vec<MailboxEntry>,
    pub next_cursor: Option<MailCursor>,
    pub account_count: usize,
    pub unified_mailboxes: Vec<MailboxEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailCursor {
    Thread(ThreadCursor),
    Search(SearchCursor),
}

#[derive(Clone, Debug)]
pub struct MailConversation {
    pub messages: Vec<MailMessage>,
}

#[derive(Clone, Debug)]
pub struct MailMetadata {
    pub account_revision: u64,
    pub scope: Scope,
    pub inbox_count: usize,
    pub mailboxes: Vec<MailboxEntry>,
    pub unified_mailboxes: Vec<MailboxEntry>,
}

struct PageLoadContext<'a> {
    scope: Scope,
    query: &'a str,
    cursor: Option<MailCursor>,
    limit: i64,
    include_counts: bool,
    /// The accounts and folders the mail view shows.
    accounts: &'a [Account],
    folders: &'a [FolderInfo],
    /// The active profile's accounts, if one is set.
    profile: Option<&'a [i64]>,
    /// Every connected account, shown or not.
    account_count: usize,
}

/// Whether the mail view shows `account_id`: every account does unless a
/// profile, given by its sorted accounts, is active.
fn profile_shows(profile: Option<&[i64]>, account_id: i64) -> bool {
    profile.is_none_or(|accounts| accounts.binary_search(&account_id).is_ok())
}

/// Keep the accounts and folders the mail view shows with `profile`.
fn retain_profile_accounts(
    profile: Option<&[i64]>,
    accounts: &mut Vec<Account>,
    folders: &mut Vec<FolderInfo>,
) {
    accounts.retain(|account| profile_shows(profile, account.id));
    folders.retain(|folder| profile_shows(profile, folder.account_id));
}

/// Keep shared labels and the labels of accounts shown with `profile`.
fn retain_profile_labels(profile: Option<&[i64]>, labels: &mut Vec<Label>) {
    labels.retain(|label| {
        label
            .owner_account_id
            .is_none_or(|owner| profile_shows(profile, owner))
    });
}

/// Keep empty snippets explicit in both the mailbox row and the temporary
/// body shown while a message has no readable MIME content.
pub fn display_preview(preview: &str) -> String {
    if preview.trim().is_empty() {
        "(No content)".to_owned()
    } else {
        preview.to_owned()
    }
}

/// Thin native host adapter over the existing offline-first mail core.
/// Authentication, synchronization, persistence, folder discovery, draft
/// saving, and sending remain owned by flectar-mail-core rather than being
/// copied
/// into the Slint application.
#[derive(Clone)]
pub struct CoreMailSource {
    core: Arc<Core>,
    account_revision: Arc<std::sync::atomic::AtomicU64>,
    /// The active profile's accounts, sorted; the mail view shows only these.
    /// `None` shows every account.
    profile_accounts: Arc<std::sync::Mutex<Option<Arc<[i64]>>>>,
}

pub struct AccountSnapshot {
    pub revision: u64,
    pub accounts: Vec<Account>,
    pub configs: Vec<AccountConfig>,
}

impl CoreMailSource {
    pub fn account_revision(&self) -> u64 {
        self.account_revision
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Show only these accounts in the mail view, or every account with
    /// `None`. Returns whether that changed; pages and counts loaded for the
    /// previous accounts are then out of date.
    pub fn set_profile_accounts(&self, accounts: Option<Vec<i64>>) -> bool {
        let accounts = accounts.map(|mut accounts| {
            accounts.sort_unstable();
            accounts.dedup();
            Arc::<[i64]>::from(accounts)
        });
        {
            let mut current = self
                .profile_accounts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *current == accounts {
                return false;
            }
            *current = accounts;
        }
        // After the new accounts are in place: a load that reads the new
        // revision also reads them.
        self.account_revision
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        true
    }

    fn profile_accounts(&self) -> Option<Arc<[i64]>> {
        self.profile_accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub async fn load_account_snapshot(&self) -> Result<AccountSnapshot, String> {
        let revision = self.account_revision();
        let (accounts, configs) =
            tokio::try_join!(self.load_accounts(), self.load_account_configs())?;
        Ok(AccountSnapshot {
            revision,
            accounts,
            configs,
        })
    }

    pub(crate) fn account_preferences_core(&self) -> Arc<Core> {
        Arc::clone(&self.core)
    }

    pub(crate) fn file_core(&self) -> Arc<Core> {
        Arc::clone(&self.core)
    }
    pub async fn start(
        paths: Paths,
        credentials: flectar_mail_core::accounts::credentials::CredentialStoreHandle,
        oauth_redirects: flectar_mail_core::oauth::redirect::OAuthRedirectBrokerHandle,
    ) -> Result<Self, String> {
        let core = Core::start_mail_ui_with_platform(paths, credentials, oauth_redirects)
            .await
            .map_err(|error| error.to_string())?;
        Ok(Self {
            core: Arc::new(core),
            account_revision: Arc::default(),
            profile_accounts: Arc::default(),
        })
    }

    /// Release background synchronization only after Slint has accepted the
    /// startup snapshot. This keeps migrations and sync from competing with
    /// the first visible frame.
    pub fn notify_ui_ready(&self) {
        self.core.notify_ui_ready();
    }

    pub async fn load_page(
        &self,
        scope: Scope,
        query: &str,
        cursor: Option<MailCursor>,
        limit: i64,
        include_counts: bool,
    ) -> Result<MailPage, String> {
        let revision = self.account_revision();
        let profile = self.profile_accounts();
        let mut accounts = self
            .core
            .list_accounts()
            .await
            .map_err(|error| error.to_string())?;
        let mut folders = self
            .core
            .list_folders(None)
            .await
            .map_err(|error| error.to_string())?;
        let account_count = accounts.len();
        retain_profile_accounts(profile.as_deref(), &mut accounts, &mut folders);
        self.load_page_with_context(PageLoadContext {
            scope,
            query,
            cursor,
            limit,
            include_counts,
            accounts: &accounts,
            folders: &folders,
            profile: profile.as_deref(),
            account_count,
        })
        .await
        .map(|mut page| {
            page.account_revision = revision;
            page
        })
    }

    /// Startup needs the account list and first page together. Loading them in
    /// one adapter call avoids querying accounts a second time while retaining
    /// the account list if folder/page projection happens to fail.
    pub async fn load_startup_page(
        &self,
        preferred_scope: Scope,
        limit: i64,
    ) -> Result<(Vec<Account>, Scope, Option<MailPage>), String> {
        let profile = self.profile_accounts();
        let accounts = self
            .core
            .list_accounts()
            .await
            .map_err(|error| error.to_string())?;
        let mut shown = accounts.clone();
        let (scope, page) = match self.core.list_folders(None).await {
            Ok(mut folders) => {
                retain_profile_accounts(profile.as_deref(), &mut shown, &mut folders);
                let mut labels = self.core.list_labels().await.unwrap_or_default();
                retain_profile_labels(profile.as_deref(), &mut labels);
                let scope = validated_startup_scope(preferred_scope, &shown, &folders, &labels);
                let page = self
                    .load_page_with_context(PageLoadContext {
                        scope,
                        query: "",
                        cursor: None,
                        limit,
                        include_counts: false,
                        accounts: &shown,
                        folders: &folders,
                        profile: profile.as_deref(),
                        account_count: accounts.len(),
                    })
                    .await
                    .ok();
                (scope, page)
            }
            Err(_) => {
                shown.retain(|account| profile_shows(profile.as_deref(), account.id));
                (Scope::UNIFIED_INBOX.visible_for(&shown), None)
            }
        };
        Ok((accounts, scope, page))
    }

    /// Load exact sidebar badges independently of the latency-sensitive message
    /// page. Core events can therefore update counters live without delaying
    /// list rendering.
    pub async fn load_mail_metadata(&self, scope: Scope) -> Result<MailMetadata, String> {
        let revision = self.account_revision();
        let profile = self.profile_accounts();
        let mut accounts = self
            .core
            .list_accounts()
            .await
            .map_err(|error| error.to_string())?;
        let mut folders = self
            .core
            .list_folders(None)
            .await
            .map_err(|error| error.to_string())?;
        retain_profile_accounts(profile.as_deref(), &mut accounts, &mut folders);
        self.load_mail_metadata_with_context(scope, &accounts, &folders, profile.as_deref())
            .await
            .map(|mut metadata| {
                metadata.account_revision = revision;
                metadata
            })
    }

    async fn load_mail_metadata_with_context(
        &self,
        scope: Scope,
        accounts: &[Account],
        folders: &[FolderInfo],
        profile: Option<&[i64]>,
    ) -> Result<MailMetadata, String> {
        let mut badges = self
            .core
            .mailbox_badge_counts()
            .await
            .map_err(|error| error.to_string())?;
        // Unified totals add up the shown accounts only.
        badges.retain(|counts| profile_shows(profile, counts.account_id));
        Ok(MailMetadata {
            account_revision: self.account_revision(),
            scope,
            inbox_count: badges
                .iter()
                .map(|counts| counts.inbox.max(0) as usize)
                .sum(),
            mailboxes: mailbox_entries_with_counts(accounts, folders, &badges),
            unified_mailboxes: unified_mailbox_entries(&badges),
        })
    }

    async fn load_page_with_context(
        &self,
        context: PageLoadContext<'_>,
    ) -> Result<MailPage, String> {
        let PageLoadContext {
            scope,
            query,
            cursor,
            limit,
            include_counts,
            accounts,
            folders,
            profile,
            account_count,
        } = context;
        let mut labels = self
            .core
            .list_labels()
            .await
            .map_err(|error| error.to_string())?;
        retain_profile_labels(profile, &mut labels);
        let metadata = if include_counts {
            Some(
                self.load_mail_metadata_with_context(scope, accounts, folders, profile)
                    .await?,
            )
        } else {
            None
        };
        let mailboxes = metadata
            .as_ref()
            .map(|metadata| metadata.mailboxes.clone())
            .unwrap_or_else(|| mailbox_entries(accounts, folders));
        let mut resolved = resolve_scope(scope, folders, &labels);
        // The mailbox of an account the profile leaves out shows the
        // profile's inbox instead.
        if resolved
            .account_id
            .is_some_and(|account_id| !profile_shows(profile, account_id))
        {
            resolved = resolve_scope(Scope::UNIFIED_INBOX, folders, &labels);
        }
        // Unified mailboxes, categories and shared labels cover the profile's
        // accounts; an account's own mailboxes need no more than its id.
        let profile_accounts = profile.filter(|_| resolved.account_id.is_none());

        let (threads, next_cursor) = if query.trim().is_empty() {
            let cursor = match cursor {
                None => None,
                Some(MailCursor::Thread(cursor)) => Some(cursor),
                Some(MailCursor::Search(_)) => {
                    return Err("search cursor used for a mailbox".into());
                }
            };
            let page = match profile_accounts {
                Some(accounts) => {
                    self.core
                        .list_threads_for_accounts(
                            resolved.view,
                            resolved.split_id,
                            accounts.to_vec(),
                            resolved.label_id,
                            resolved.folder_id,
                            (cursor, limit),
                        )
                        .await
                }
                None => {
                    self.core
                        .list_threads(
                            resolved.view,
                            resolved.split_id,
                            resolved.account_id,
                            resolved.label_id,
                            resolved.folder_id,
                            (cursor, limit),
                        )
                        .await
                }
            }
            .map_err(|error| error.to_string())?;
            (page.threads, page.next_cursor.map(MailCursor::Thread))
        } else {
            // The mail list is a timeline: search narrows it, then presents
            // the newest matching threads first. Relevance-ranked retrieval
            // remains available to non-UI core consumers.
            let cursor = match cursor {
                None => None,
                Some(MailCursor::Search(cursor)) => Some(cursor),
                Some(MailCursor::Thread(_)) => return Err("mailbox cursor used for search".into()),
            };
            let query = query.trim().to_owned();
            let page = match profile_accounts {
                Some(accounts) => {
                    self.core
                        .search_chronological_page_for_accounts(
                            query,
                            accounts.to_vec(),
                            cursor,
                            limit,
                        )
                        .await
                }
                None => {
                    self.core
                        .search_chronological_page(query, resolved.account_id, cursor, limit)
                        .await
                }
            }
            .map_err(|error| error.to_string())?;
            (page.threads, page.next_cursor.map(MailCursor::Search))
        };

        let messages = threads
            .into_iter()
            .filter_map(|thread| summary_to_message(thread, accounts, resolved.title.as_str()))
            .collect();

        Ok(MailPage {
            account_revision: self.account_revision(),
            messages,
            labels,
            mailboxes,
            next_cursor,
            account_count,
            unified_mailboxes: metadata
                .map(|metadata| metadata.unified_mailboxes)
                .unwrap_or_default(),
        })
    }

    /// Load the selected thread as independent messages. Bodies are still
    /// fetched lazily by the core and the UI expands only one message at a
    /// time, so opening a long conversation does not create a renderer per
    /// message.
    pub async fn load_conversation(
        &self,
        row: &MailMessage,
        selected_message_id: Option<i64>,
    ) -> Result<MailConversation, String> {
        let thread_id = row
            .thread_id
            .ok_or_else(|| "message is not backed by a core thread".to_owned())?;
        let mut thread = self
            .core
            .get_thread_outline(thread_id)
            .await
            .map_err(|error| error.to_string())?;
        let selected_message_id = selected_message_id
            .filter(|id| thread.messages.iter().any(|message| message.id == *id))
            .or_else(|| thread.messages.last().map(|message| message.id))
            .ok_or_else(|| "thread has no messages".to_owned())?;
        let selected = self
            .core
            .get_body(selected_message_id)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(message) = thread
            .messages
            .iter_mut()
            .find(|message| message.id == selected_message_id)
        {
            *message = selected;
        }
        let messages = thread
            .messages
            .iter()
            .filter_map(|message| {
                detail_to_conversation_message(row, message, message.id == selected_message_id)
            })
            .collect::<Vec<_>>();
        if messages.is_empty() {
            return Err("thread has no messages".to_owned());
        }
        Ok(MailConversation { messages })
    }

    pub async fn load_compose_source(
        &self,
        row: &MailMessage,
        message_id: Option<i64>,
    ) -> Result<ComposeSource, String> {
        let thread_id = row
            .thread_id
            .ok_or_else(|| "message is not backed by a core thread".to_owned())?;
        let thread = self
            .core
            .get_thread(thread_id)
            .await
            .map_err(|error| error.to_string())?;
        let message = thread
            .messages
            .iter()
            .find(|message| Some(message.id) == message_id)
            .or_else(|| thread.messages.last())
            .ok_or_else(|| "thread has no messages".to_owned())?;
        let account_email = self
            .core
            .list_accounts()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|account| account.id == message.account_id)
            .map(|account| account.email)
            .ok_or_else(|| "message account is no longer connected".to_owned())?;

        let sender_identities = self
            .core
            .list_sender_identities(message.account_id)
            .await
            .map_err(|error| error.to_string())?;
        let default_sender_email = self
            .core
            .default_sender_identity(message.account_id)
            .await
            .map_err(|error| error.to_string())?
            .email;

        Ok(ComposeSource {
            message_id: message.id,
            account_id: message.account_id,
            account_email,
            sender_identities,
            default_sender_email,
            from: message.from.clone(),
            to: message.to.clone(),
            cc: message.cc.clone(),
            subject: message.subject.clone(),
            date: message.date,
            text_body: message
                .text_body
                .clone()
                .filter(|body| !body.trim().is_empty())
                .unwrap_or_else(|| message.snippet.clone()),
        })
    }

    pub async fn load_draft(&self, row: &MailMessage) -> Result<SaveDraftArgs, String> {
        let thread_id = row
            .thread_id
            .ok_or_else(|| "draft is not backed by a core thread".to_owned())?;
        let thread = self
            .core
            .get_thread(thread_id)
            .await
            .map_err(|error| error.to_string())?;
        let draft_id = thread
            .messages
            .iter()
            .rev()
            .find(|message| message.is_draft)
            .map(|message| message.id)
            .ok_or_else(|| "thread has no editable draft".to_owned())?;
        self.core
            .get_draft(draft_id)
            .await
            .map_err(|error| error.to_string())
    }

    /// Subscribe before opening a message so the native shell can replace the
    /// temporary header/snippet view as soon as the asynchronously fetched
    /// body is committed by the sync engine.
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<CoreEvent> {
        self.core.bus.subscribe()
    }

    pub async fn set_load_remote_images(&self, enabled: bool) -> Result<(), String> {
        let mut settings = self
            .core
            .get_settings()
            .await
            .map_err(|error| error.to_string())?;
        settings.load_remote_images = enabled;
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn load_accounts(&self) -> Result<Vec<Account>, String> {
        self.core
            .list_accounts()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn create_folder(
        &self,
        account_id: i64,
        parent_folder_id: Option<i64>,
        name: String,
    ) -> Result<(), String> {
        self.core
            .create_folder(account_id, parent_folder_id, name)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn rename_folder(&self, folder_id: i64, name: String) -> Result<(), String> {
        self.core
            .rename_folder(folder_id, name)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn delete_folder(&self, folder_id: i64) -> Result<(), String> {
        self.core
            .delete_folder(folder_id)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn load_account_configs(&self) -> Result<Vec<AccountConfig>, String> {
        self.core
            .list_account_configs()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn import_account_configs(
        &self,
        configs: Vec<PortableAccountConfig>,
    ) -> Result<usize, String> {
        self.core
            .import_account_configs(configs)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn import_accounts(
        &self,
        drafts: Vec<flectar_mail_core::migration::AccountDraft>,
    ) -> Result<flectar_mail_core::migration::AccountImportReport, String> {
        self.core
            .import_accounts(drafts)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn save_settings(&self, settings: Settings) -> Result<(), String> {
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn delete_all_local_data(&self) -> Result<(), String> {
        self.core
            .delete_all_local_data()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn create_database_snapshot(
        &self,
        destination: std::path::PathBuf,
    ) -> Result<flectar_mail_core::DatabaseSnapshotManifest, String> {
        self.core
            .create_database_snapshot(destination)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn export_account_mail(
        &self,
        account_id: i64,
        destination: std::path::PathBuf,
    ) -> Result<flectar_mail_core::models::MailExport, String> {
        self.core
            .export_account_mail(account_id, destination)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn load_settings(&self) -> Result<Settings, String> {
        self.core
            .get_settings()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_notification_settings(
        &self,
        notifications_enabled: bool,
        sound_enabled: bool,
        notification_scope: &str,
    ) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.notifications_enabled = notifications_enabled;
        settings.sound_enabled = sound_enabled;
        settings.notification_scope = match notification_scope {
            "all" => "all",
            _ => "important",
        }
        .to_owned();
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_sync_interval_minutes(&self, minutes: i64) -> Result<(), String> {
        self.core
            .set_sync_interval_minutes(minutes)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_account_mail_history(
        &self,
        account_id: i64,
        value: &str,
    ) -> Result<(), String> {
        let mail_history = MailHistory::from_storage(value)
            .ok_or_else(|| format!("unsupported mail history window: {value}"))?;
        self.core
            .set_account_mail_history(account_id, mail_history)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_account_keep_on_server(
        &self,
        account_id: i64,
        value: &str,
    ) -> Result<(), String> {
        let keep = flectar_mail_core::models::KeepOnServer::from_storage(value)
            .ok_or_else(|| format!("unsupported choice for mail on the server: {value}"))?;
        self.core
            .set_account_keep_on_server(account_id, keep)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_account_sent_copy_policy(&self, account_id: i64, value: &str) -> Result<(), String> {
        let policy = flectar_mail_core::models::SentCopyPolicy::from_storage(value)
            .ok_or_else(|| format!("unsupported Sent copy policy: {value}"))?;
        self.core.set_account_sent_copy_policy(account_id, policy).await.map_err(|e| e.to_string())
    }

    pub async fn set_mark_read_on_open(&self, enabled: bool) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.mark_read_on_open = enabled;
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_contact_discovery_settings(
        &self,
        collect_outgoing: bool,
        collect_incoming: bool,
        suggest_all_accounts: bool,
        suggest_learned: bool,
    ) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.collect_outgoing_contacts = collect_outgoing;
        settings.collect_incoming_contacts = collect_incoming;
        settings.contact_suggest_all_accounts = suggest_all_accounts;
        settings.suggest_learned_contacts = suggest_learned;
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn clear_contact_suggestions(&self) -> Result<usize, String> {
        self.core
            .clear_contact_suggestions()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_theme(&self, theme: &str) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.theme = match theme {
            "dark" => "carbon",
            "light" => "snow",
            _ => "system",
        }
        .to_owned();
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_theme_preset(&self, preset: &str) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.theme_preset = match preset {
            "teal" | "green" | "purple" | "custom" => preset,
            _ => "default",
        }
        .to_owned();
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_message_appearance(&self, preference: &str) -> Result<(), String> {
        let preference = flectar_mail_core::models::MessageAppearance::parse(preference)
            .ok_or("Unknown message appearance")?;
        let mut settings = self.load_settings().await?;
        settings.message_appearance = preference;
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_custom_theme(&self, custom_theme: CustomTheme) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.custom_theme = custom_theme;
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_language(&self, language: &str) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.language = crate::normalized_language_preference(language).to_owned();
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_app_icon(
        &self,
        icon: flectar_mail_core::models::AppIcon,
    ) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.app_icon = icon;
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_monochrome_sidebar_icons(&self, enabled: bool) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.monochrome_sidebar_icons = enabled;
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_show_avatars(&self, enabled: bool) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.show_avatars = enabled;
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_group_mail_by_date(&self, enabled: bool) -> Result<(), String> {
        self.core
            .set_group_mail_by_date(enabled)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_time_format(
        &self,
        format: flectar_mail_core::models::TimeFormat,
    ) -> Result<(), String> {
        self.core
            .set_time_format(format)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_workspace_layout(&self, layout: &str) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.workspace_layout = match layout {
            "minimal" => "minimal",
            _ => "default",
        }
        .to_owned();
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_workspace_list_pane_width(&self, width: i64) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.workspace_list_pane_width = normalized_workspace_list_pane_width(width);
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn save_mail_profile(
        &self,
        profile_id: Option<String>,
        name: String,
        color: String,
    ) -> Result<(Settings, MailProfile), String> {
        self.core
            .save_mail_profile(profile_id, name, color)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn assign_account_profile(
        &self,
        account_id: i64,
        profile_id: Option<String>,
    ) -> Result<Settings, String> {
        self.core
            .assign_account_profile(account_id, profile_id)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn delete_mail_profile(&self, profile_id: String) -> Result<Settings, String> {
        self.core
            .delete_mail_profile(profile_id)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_active_mail_profile(
        &self,
        profile_id: Option<String>,
    ) -> Result<Settings, String> {
        self.core
            .set_active_mail_profile(profile_id)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_show_account_badges(&self, enabled: bool) -> Result<Settings, String> {
        self.core
            .set_show_account_badges(enabled)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_account_color(
        &self,
        account_id: i64,
        color: Option<String>,
    ) -> Result<Settings, String> {
        self.core
            .set_account_color(account_id, color)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_close_to_tray(&self, enabled: bool) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.close_to_tray = enabled;
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_window_frame(&self, frame: &str) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.window_frame = frame.to_owned();
        self.core
            .set_settings(settings)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn reorder_account(
        &self,
        source_id: i64,
        target_id: i64,
        after: bool,
    ) -> Result<(), String> {
        self.core
            .reorder_account(source_id, target_id, after)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn remove_account(&self, account_id: i64) -> Result<(), String> {
        let result = self
            .core
            .remove_account(account_id)
            .await
            .map_err(|error| error.to_string());
        // Includes partial failures: snapshots read before the cross-store
        // operation finished must never restore deleted account data.
        self.account_revision
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        result
    }

    pub async fn list_contacts(
        &self,
        prefix: String,
        account_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Address>, String> {
        self.core
            .list_contacts(prefix, account_id, limit)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn list_email_templates(&self) -> Result<Vec<Snippet>, String> {
        self.core
            .list_snippets()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn save_email_template(
        &self,
        id: Option<i64>,
        name: String,
        shortcut: Option<String>,
        subject: Option<String>,
        body: String,
    ) -> Result<Snippet, String> {
        self.core
            .save_snippet(id, name, shortcut, subject, body)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn delete_email_template(&self, id: i64) -> Result<(), String> {
        self.core
            .delete_snippet(id)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn use_email_template(&self, id: i64) -> Result<(), String> {
        self.core
            .use_snippet(id)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn load_contact_page(
        &self,
        scope: String,
        query: String,
        cursor: Option<ContactRecordCursor>,
        limit: i64,
    ) -> Result<ContactRecordPage, String> {
        let favorites_only = scope == "Favorites";
        let suggestions_only = scope == "Suggestions";
        let account_id = scope
            .strip_prefix("Account:")
            .and_then(|value| value.parse().ok());
        self.core
            .list_contact_record_page(
                query,
                account_id,
                favorites_only,
                suggestions_only,
                cursor,
                limit,
            )
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn contact_destination(&self, account_id: i64) -> Result<Option<String>, String> {
        self.core
            .list_addressbooks(Some(account_id))
            .await
            .map(|books| {
                books
                    .into_iter()
                    .find(|book| book.enabled && !book.read_only)
                    .map(|book| book.display_name.unwrap_or(book.url))
            })
            .map_err(|e| e.to_string())
    }

    pub async fn contact_photo(&self, hash: String) -> Result<Option<Vec<u8>>, String> {
        self.core
            .contact_photo(hash)
            .await
            .map_err(|e| e.to_string())
    }
    pub async fn resolve_contact_conflict(
        &self,
        id: i64,
        revision: i64,
        keep_local: bool,
    ) -> Result<(), String> {
        self.core
            .resolve_contact_conflict(id, revision, keep_local)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn save_contact(
        &self,
        contact: flectar_mail_core::models::ContactRecord,
    ) -> Result<flectar_mail_core::models::ContactRecord, String> {
        self.core
            .save_contact(contact)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn delete_contact(&self, id: i64, revision: i64) -> Result<(), String> {
        self.core
            .delete_contact(id, revision)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn connect_carddav(
        &self,
        account_id: i64,
        url: String,
        username: String,
        password: String,
    ) -> Result<(), String> {
        self.core
            .connect_carddav(ConnectCardDavArgs {
                account_id,
                url,
                username,
                password,
            })
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub async fn load_carddav_connections(&self) -> Result<Vec<CardDavConnection>, String> {
        self.core
            .list_carddav_connections()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_account_carddav_enabled(
        &self,
        account_id: i64,
        enabled: bool,
    ) -> Result<(), String> {
        self.core
            .set_account_carddav_enabled(account_id, enabled)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn disconnect_carddav(&self, account_id: i64) -> Result<(), String> {
        self.core
            .disconnect_carddav(account_id)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn perform_message_action(&self, thread_id: i64, action: &str) -> Result<(), String> {
        let kind = ActionKind::parse(action)
            .ok_or_else(|| format!("unsupported message action: {action}"))?;
        self.core
            .perform_action(PerformActionArgs {
                kind,
                thread_ids: vec![thread_id],
                params: None,
            })
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub async fn empty_trash(&self, account_id: Option<i64>) -> Result<(), String> {
        self.core
            .empty_trash(account_id)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub async fn perform_label_action(
        &self,
        thread_id: i64,
        label_id: i64,
        add: bool,
    ) -> Result<(), String> {
        self.core
            .perform_action(PerformActionArgs {
                kind: if add {
                    ActionKind::AddLabel
                } else {
                    ActionKind::RemoveLabel
                },
                thread_ids: vec![thread_id],
                params: Some(ActionParams {
                    wake_at: None,
                    target_folder_id: None,
                    label_id: Some(label_id),
                }),
            })
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub async fn move_thread_to_folder(
        &self,
        thread_id: i64,
        target_folder_id: i64,
    ) -> Result<(), String> {
        self.core
            .perform_action(PerformActionArgs {
                kind: ActionKind::Move,
                thread_ids: vec![thread_id],
                params: Some(ActionParams {
                    wake_at: None,
                    target_folder_id: Some(target_folder_id),
                    label_id: None,
                }),
            })
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub async fn route_thread_to_tab(&self, thread_id: i64, target: String) -> Result<(), String> {
        self.core
            .route_thread_to_tab(thread_id, target)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn save_label(
        &self,
        id: Option<i64>,
        name: &str,
        color: &str,
        owner_account_id: Option<i64>,
    ) -> Result<Label, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("enter a label name".to_owned());
        }
        let labels = self
            .core
            .list_labels()
            .await
            .map_err(|error| error.to_string())?;
        let position = match id {
            Some(id) => labels
                .iter()
                .find(|label| label.id == id)
                .map(|label| label.position)
                .ok_or_else(|| "label no longer exists".to_owned())?,
            None => labels
                .iter()
                .filter(|label| !label.is_auto && label.owner_account_id == owner_account_id)
                .map(|label| label.position)
                .max()
                .unwrap_or(-1)
                .saturating_add(1),
        };
        self.core
            .save_label(
                id,
                name.to_owned(),
                color.to_owned(),
                position,
                owner_account_id,
            )
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn delete_label(&self, id: i64) -> Result<(), String> {
        self.core
            .delete_label(id)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn sync_now(&self, account_id: Option<i64>) -> Result<(), String> {
        self.core
            .sync_now(account_id)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn add_password_account(
        &self,
        args: AddPasswordAccountArgs,
    ) -> Result<Account, String> {
        self.core
            .add_account_password(args)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn start_oauth_with_calendar<F>(
        &self,
        provider: Provider,
        connect_calendar: bool,
        open_url: F,
    ) -> Result<Account, String>
    where
        F: Fn(String) -> Result<(), String> + Clone + Send + 'static,
    {
        self.core
            .start_oauth_with_calendar(provider, connect_calendar, open_url)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn load_events(
        &self,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<CalendarEvent>, String> {
        self.core
            .list_events(start_ms, end_ms)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn create_event(&self, args: CreateEventArgs) -> Result<CalendarEvent, String> {
        self.core
            .create_event(args)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn update_event(
        &self,
        args: flectar_mail_core::models::UpdateEventArgs,
    ) -> Result<CalendarEvent, String> {
        self.core
            .update_event(args)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn delete_event(&self, event_id: i64) -> Result<(), String> {
        self.core
            .delete_event(event_id, false)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn load_calendar_connections(&self) -> Result<Vec<CalendarConnection>, String> {
        self.core
            .list_calendar_connections()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn load_calendars(
        &self,
        account_id: Option<i64>,
    ) -> Result<Vec<flectar_mail_core::models::Calendar>, String> {
        self.core
            .list_calendars(account_id)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn connect_provider_calendar<F>(
        &self,
        account_id: i64,
        provider: Provider,
        open_url: F,
    ) -> Result<(), String>
    where
        F: FnOnce(String) -> Result<(), String> + Send + 'static,
    {
        match provider {
            Provider::Gmail => self
                .core
                .connect_google_calendar(account_id, open_url)
                .await
                .map_err(|error| error.to_string())?,
            Provider::Microsoft => self
                .core
                .connect_microsoft_calendar(account_id, open_url)
                .await
                .map_err(|error| error.to_string())?,
            Provider::Generic => return Err("use CalDAV for an IMAP account".into()),
        };
        Ok(())
    }

    pub async fn connect_caldav(
        &self,
        account_id: i64,
        url: String,
        username: String,
        password: String,
    ) -> Result<(), String> {
        self.core
            .connect_calendar(ConnectCalendarArgs {
                account_id,
                kind: "generic".into(),
                url: Some(url),
                username: Some(username),
                password: Some(password),
            })
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub async fn set_account_calendar_enabled(
        &self,
        account_id: i64,
        enabled: bool,
    ) -> Result<(), String> {
        self.core
            .set_account_calendar_enabled(account_id, enabled)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn set_calendar_enabled(
        &self,
        calendar_id: i64,
        enabled: bool,
    ) -> Result<(), String> {
        self.core
            .set_calendar_enabled(calendar_id, enabled)
            .await
            .map_err(|error| error.to_string())
    }

    pub fn cancel_oauth(&self) {
        self.core.cancel_oauth();
    }

    pub async fn set_oauth_apps(
        &self,
        google: Option<(String, String)>,
        microsoft: Option<String>,
    ) -> Result<flectar_mail_core::models::Settings, String> {
        self.core
            .set_oauth_apps(google, microsoft)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn reauth_account<F>(&self, account_id: i64, open_url: F) -> Result<Account, String>
    where
        F: FnOnce(String) -> Result<(), String> + Send + 'static,
    {
        self.core
            .reauth_account(account_id, open_url)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn save_draft(&self, args: SaveDraftArgs) -> Result<i64, String> {
        self.core
            .save_draft(args)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn queue_send(&self, args: QueueSendArgs) -> Result<QueueSendResult, String> {
        self.core
            .queue_send(args)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn save_new_draft(&self, message: ComposeMessage<'_>) -> Result<i64, String> {
        self.save_draft(compose_args(message, false)?).await
    }

    pub async fn send_new_message(
        &self,
        message: ComposeMessage<'_>,
    ) -> Result<QueueSendResult, String> {
        let draft_id = self.save_draft(compose_args(message, true)?).await?;
        let queued = self
            .queue_send(QueueSendArgs {
                draft_id,
                send_at: None,
            })
            .await?;
        self.core
            .send_now(queued.action_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(queued)
    }

    pub async fn wait_for_send(&self, action_id: i64) -> Result<(), String> {
        self.core
            .wait_for_send(action_id)
            .await
            .map_err(|error| error.to_string())
    }
}

fn compose_args(
    message: ComposeMessage<'_>,
    require_recipient: bool,
) -> Result<SaveDraftArgs, String> {
    let recipients = parse_recipients(message.to)?;
    if require_recipient && recipients.is_empty() {
        return Err("enter at least one recipient".to_owned());
    }
    let body_text = message.body.to_owned();
    let body_html = message
        .body_html
        .map(str::to_owned)
        .or_else(|| (!message.body.trim().is_empty()).then(|| markdown_to_html(message.body)));
    Ok(SaveDraftArgs {
        draft_id: message.draft_id,
        account_id: message.account_id,
        from: message.sender_email.map(|email| Address {
            name: None,
            email: email.to_owned(),
        }),
        to: recipients,
        cc: parse_recipients(message.cc)?,
        bcc: parse_recipients(message.bcc)?,
        subject: message.subject.trim().to_owned(),
        body_text,
        body_html,
        mode: message.mode.to_owned(),
        in_reply_to_message_id: message.in_reply_to_message_id,
        attachments: message.attachments.to_vec(),
    })
}

fn parse_recipients(value: &str) -> Result<Vec<Address>, String> {
    value
        .split([',', ';'])
        .map(str::trim)
        .filter(|recipient| !recipient.is_empty())
        .map(parse_address)
        .collect()
}

fn parse_address(value: &str) -> Result<Address, String> {
    let (name, email) = value
        .rsplit_once('<')
        .and_then(|(name, email)| email.strip_suffix('>').map(|email| (Some(name), email)))
        .unwrap_or((None, value));
    let email = email.trim();
    if !email.contains('@') {
        return Err(format!("invalid recipient: {value}"));
    }
    Ok(Address {
        name: name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        email: email.to_owned(),
    })
}

const fn standard_mailbox_view(mailbox: StandardMailbox) -> View {
    match mailbox {
        StandardMailbox::Inbox => View::Inbox,
        StandardMailbox::Starred => View::Starred,
        StandardMailbox::Sent => View::Sent,
        StandardMailbox::Archive => View::Done,
        StandardMailbox::Spam => View::Spam,
        StandardMailbox::Trash => View::Trash,
        StandardMailbox::Drafts => View::Drafts,
    }
}

/// Provider folder role backing a standard mailbox. Starred is a flag, not a
/// folder.
const fn standard_mailbox_role(mailbox: StandardMailbox) -> Option<&'static str> {
    match mailbox {
        StandardMailbox::Inbox => Some("inbox"),
        StandardMailbox::Starred => None,
        StandardMailbox::Sent => Some("sent"),
        StandardMailbox::Archive => Some("archive"),
        StandardMailbox::Spam => Some("spam"),
        StandardMailbox::Trash => Some("trash"),
        StandardMailbox::Drafts => Some("drafts"),
    }
}

fn is_standard_folder(folder: &FolderInfo) -> bool {
    matches!(
        folder.role.as_deref(),
        Some("inbox" | "sent" | "archive" | "spam" | "trash" | "drafts")
    )
}

#[derive(Clone, Copy)]
struct FolderTreeRoot<'a> {
    folder: &'a FolderInfo,
    depth: usize,
    parent_folder_id: i64,
    standard_mailbox: Option<StandardMailbox>,
}

fn append_folder_tree<'a>(
    entries: &mut Vec<MailboxEntry>,
    root: FolderTreeRoot<'a>,
    account_label: &str,
    children: &std::collections::HashMap<i64, Vec<&'a FolderInfo>>,
    visited: &mut std::collections::HashSet<i64>,
) {
    let mut pending = vec![root];
    while let Some(FolderTreeRoot {
        folder,
        depth,
        parent_folder_id,
        standard_mailbox,
    }) = pending.pop()
    {
        if !visited.insert(folder.id) {
            continue;
        }
        let descendants = children.get(&folder.id);
        let has_children = descendants.is_some_and(|descendants| {
            descendants
                .iter()
                .any(|descendant| !visited.contains(&descendant.id))
        });
        let is_standard = standard_mailbox.is_some();
        let label = standard_mailbox
            .map(|mailbox| mailbox.label().to_owned())
            .unwrap_or_else(|| folder_label(folder));
        entries.push(MailboxEntry {
            account_id: folder.account_id,
            folder_id: folder.id,
            parent_folder_id,
            depth,
            has_children,
            is_standard,
            is_selectable: folder.selectable,
            can_create_children: folder.can_create_children,
            can_rename: folder.can_rename,
            can_delete: folder.can_delete,
            label,
            scope: Some(standard_mailbox.map_or(Scope::Folder(folder.id), |mailbox| {
                Scope::Account {
                    account_id: folder.account_id,
                    mailbox,
                }
            })),
            context: account_label.to_owned(),
            detail: String::new(),
            avatar: String::new(),
            is_account: false,
            count: String::new(),
        });

        if let Some(descendants) = descendants {
            for descendant in descendants.iter().rev() {
                if !visited.contains(&descendant.id) {
                    pending.push(FolderTreeRoot {
                        folder: descendant,
                        depth: depth + 1,
                        parent_folder_id: folder.id,
                        standard_mailbox: None,
                    });
                }
            }
        }
    }
}

fn mailbox_entries(accounts: &[Account], folders: &[FolderInfo]) -> Vec<MailboxEntry> {
    let mut entries = Vec::new();
    for account in accounts {
        let account_label = account_label(account);
        entries.push(MailboxEntry {
            account_id: account.id,
            folder_id: -1,
            parent_folder_id: -1,
            depth: 0,
            has_children: false,
            is_standard: false,
            is_selectable: false,
            can_create_children: account.can_create_top_level_mailbox,
            can_rename: false,
            can_delete: false,
            label: account_label.clone(),
            scope: None,
            context: account_label.clone(),
            detail: account.email.clone(),
            avatar: avatar_initials(&account_label),
            is_account: true,
            count: String::new(),
        });

        let account_folders = folders
            .iter()
            .filter(|folder| folder.account_id == account.id)
            .collect::<Vec<_>>();
        let role_folders = StandardMailbox::ALL
            .into_iter()
            .filter_map(|mailbox| {
                let role = standard_mailbox_role(mailbox)?;
                account_folders
                    .iter()
                    .copied()
                    .find(|folder| folder.role.as_deref() == Some(role))
                    .map(|folder| (mailbox, folder))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let role_folder_ids = role_folders
            .values()
            .map(|folder| folder.id)
            .collect::<std::collections::HashSet<_>>();
        let mut tree_folders = account_folders
            .into_iter()
            .filter(|folder| {
                role_folder_ids.contains(&folder.id)
                    || (account.mail_protocol != MailProtocol::Gmail && !is_standard_folder(folder))
            })
            .collect::<Vec<_>>();
        tree_folders.sort_by_key(|folder| (folder.display_name.to_lowercase(), folder.id));

        // Fresh discovery provides explicit local parent ids. Path inference
        // preserves an upgraded database's existing hierarchy until its first
        // authoritative provider discovery populates those new ids.
        let ids_by_name = tree_folders
            .iter()
            .map(|folder| (folder.path.as_str(), folder.id))
            .collect::<std::collections::HashMap<_, _>>();
        let folder_ids = tree_folders
            .iter()
            .map(|folder| folder.id)
            .collect::<std::collections::HashSet<_>>();
        let parent_ids = tree_folders
            .iter()
            .map(|folder| {
                let path_parent_id = folder
                    .is_jmap
                    .then_some(" / ")
                    .or(folder.delimiter.as_deref())
                    .filter(|delimiter| !delimiter.is_empty())
                    .and_then(|delimiter| folder.path.rsplit_once(delimiter))
                    .and_then(|(parent, _)| ids_by_name.get(parent).copied());
                let parent_id = folder
                    .parent_id
                    .filter(|parent| *parent != folder.id && folder_ids.contains(parent))
                    .or(path_parent_id)
                    .unwrap_or(-1);
                // Special-use mailboxes remain pinned roots, matching the
                // standard ordering used by major desktop mail clients.
                (
                    folder.id,
                    if role_folder_ids.contains(&folder.id) {
                        -1
                    } else {
                        parent_id
                    },
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        let mut children = std::collections::HashMap::<i64, Vec<&FolderInfo>>::new();
        for folder in &tree_folders {
            let parent_id = parent_ids.get(&folder.id).copied().unwrap_or(-1);
            if parent_id >= 0 {
                children.entry(parent_id).or_default().push(folder);
            }
        }
        for descendants in children.values_mut() {
            descendants.sort_by_key(|folder| (folder.display_name.to_lowercase(), folder.id));
        }

        let mut visited = std::collections::HashSet::new();
        for mailbox in StandardMailbox::ALL {
            if let Some(folder) = role_folders.get(&mailbox).copied() {
                append_folder_tree(
                    &mut entries,
                    FolderTreeRoot {
                        folder,
                        depth: 0,
                        parent_folder_id: -1,
                        standard_mailbox: Some(mailbox),
                    },
                    &account_label,
                    &children,
                    &mut visited,
                );
                continue;
            }
            entries.push(MailboxEntry {
                account_id: account.id,
                folder_id: -1,
                parent_folder_id: -1,
                depth: 0,
                has_children: false,
                is_standard: true,
                is_selectable: true,
                can_create_children: false,
                can_rename: false,
                can_delete: false,
                label: mailbox.label().to_owned(),
                scope: Some(Scope::Account {
                    account_id: account.id,
                    mailbox,
                }),
                context: account_label.clone(),
                detail: String::new(),
                avatar: String::new(),
                is_account: false,
                count: String::new(),
            });
        }

        let roots = tree_folders
            .iter()
            .copied()
            .filter(|folder| {
                !role_folder_ids.contains(&folder.id)
                    && parent_ids.get(&folder.id).copied().unwrap_or(-1) < 0
            })
            .collect::<Vec<_>>();
        for root in roots {
            append_folder_tree(
                &mut entries,
                FolderTreeRoot {
                    folder: root,
                    depth: 0,
                    parent_folder_id: -1,
                    standard_mailbox: None,
                },
                &account_label,
                &children,
                &mut visited,
            );
        }

        // Broken or cyclic provider data must not make a mailbox disappear.
        // Promote one unvisited node at a time to a safe root; the visited set
        // then bounds traversal and prevents duplicates.
        for folder in tree_folders {
            if !visited.contains(&folder.id) {
                append_folder_tree(
                    &mut entries,
                    FolderTreeRoot {
                        folder,
                        depth: 0,
                        parent_folder_id: -1,
                        standard_mailbox: None,
                    },
                    &account_label,
                    &children,
                    &mut visited,
                );
            }
        }
    }
    entries
}

fn mailbox_entries_with_counts(
    accounts: &[Account],
    folders: &[FolderInfo],
    badges: &[MailboxBadgeCounts],
) -> Vec<MailboxEntry> {
    let mut entries = mailbox_entries(accounts, folders);
    for entry in &mut entries {
        // Badges belong to the standard mailboxes, not to custom folders that
        // merely share a name with one.
        let Some(Scope::Account { mailbox, .. }) = entry.scope else {
            continue;
        };
        let Some(counts) = badges
            .iter()
            .find(|counts| counts.account_id == entry.account_id)
        else {
            continue;
        };
        entry.count = mailbox_badge(mailbox, counts);
    }
    entries
}

fn unified_mailbox_entries(badges: &[MailboxBadgeCounts]) -> Vec<MailboxEntry> {
    let total = MailboxBadgeCounts {
        account_id: 0,
        inbox: badges.iter().map(|counts| counts.inbox).sum(),
        starred: badges.iter().map(|counts| counts.starred).sum(),
        drafts: badges.iter().map(|counts| counts.drafts).sum(),
    };
    // The unified inbox has a dedicated sidebar row.
    let mailboxes = StandardMailbox::ALL
        .into_iter()
        .filter(|mailbox| *mailbox != StandardMailbox::Inbox);
    let mut entries = Vec::with_capacity(StandardMailbox::ALL.len() - 1);
    for mailbox in mailboxes {
        entries.push(MailboxEntry {
            account_id: 0,
            folder_id: -1,
            parent_folder_id: -1,
            depth: 0,
            has_children: false,
            is_standard: true,
            is_selectable: true,
            can_create_children: false,
            can_rename: false,
            can_delete: false,
            label: mailbox.label().to_owned(),
            scope: Some(Scope::Unified(mailbox)),
            context: "Unified".to_owned(),
            detail: String::new(),
            avatar: String::new(),
            is_account: false,
            count: mailbox_badge(mailbox, &total),
        });
    }
    entries
}

fn mailbox_badge(mailbox: StandardMailbox, counts: &MailboxBadgeCounts) -> String {
    let count = match mailbox {
        StandardMailbox::Inbox => counts.inbox,
        StandardMailbox::Starred => counts.starred,
        StandardMailbox::Drafts => counts.drafts,
        StandardMailbox::Sent
        | StandardMailbox::Archive
        | StandardMailbox::Spam
        | StandardMailbox::Trash => return String::new(),
    };
    if count > 0 {
        count.to_string()
    } else {
        String::new()
    }
}

struct ScopeResolution {
    account_id: Option<i64>,
    folder_id: Option<i64>,
    split_id: Option<i64>,
    label_id: Option<i64>,
    view: View,
    title: String,
}

fn resolve_scope(scope: Scope, folders: &[FolderInfo], labels: &[Label]) -> ScopeResolution {
    let unfiltered = |view, title: &str| ScopeResolution {
        account_id: None,
        folder_id: None,
        split_id: None,
        label_id: None,
        view,
        title: title.to_owned(),
    };
    match scope {
        Scope::Unified(mailbox) => unfiltered(standard_mailbox_view(mailbox), mailbox.label()),
        Scope::Important => ScopeResolution {
            split_id: Some(-1),
            ..unfiltered(View::Inbox, "Important")
        },
        Scope::Other => ScopeResolution {
            split_id: Some(-2),
            ..unfiltered(View::Inbox, "Other")
        },
        Scope::Label { id, .. } => match labels.iter().find(|label| label.id == id) {
            Some(label) => ScopeResolution {
                account_id: label.owner_account_id,
                label_id: Some(id),
                ..unfiltered(
                    if label.is_auto {
                        View::Inbox
                    } else {
                        View::All
                    },
                    &label.name,
                )
            },
            None => resolve_scope(Scope::UNIFIED_INBOX, folders, labels),
        },
        Scope::Folder(id) => match folders.iter().find(|folder| folder.id == id) {
            Some(folder) => ScopeResolution {
                account_id: Some(folder.account_id),
                folder_id: Some(folder.id),
                ..unfiltered(view_for_folder(folder), &folder_label(folder))
            },
            None => resolve_scope(Scope::UNIFIED_INBOX, folders, labels),
        },
        Scope::Account {
            account_id,
            mailbox,
        } => ScopeResolution {
            account_id: Some(account_id),
            ..unfiltered(standard_mailbox_view(mailbox), mailbox.label())
        },
    }
}

/// A warm-start scope is presentation data and can outlive an account,
/// folder or label. Accept only scopes represented by the current local
/// database; otherwise fall back to the visible inbox before querying its
/// first page.
fn validated_startup_scope(
    preferred: Scope,
    accounts: &[Account],
    folders: &[FolderInfo],
    labels: &[Label],
) -> Scope {
    let scope = match preferred {
        Scope::Unified(_) | Scope::Important | Scope::Other => Some(preferred),
        // A label can move between sections, e.g. when it becomes automatic.
        Scope::Label { id, .. } => labels
            .iter()
            .find(|label| label.id == id)
            .map(|label| Scope::Label {
                kind: LabelKind::of(label.is_auto, label.owner_account_id),
                id,
            }),
        Scope::Folder(_) => mailbox_entries(accounts, folders)
            .iter()
            .any(|mailbox| mailbox.scope == Some(preferred))
            .then_some(preferred),
        Scope::Account { account_id, .. } => accounts
            .iter()
            .any(|account| account.id == account_id)
            .then_some(preferred),
    };
    scope.unwrap_or(Scope::UNIFIED_INBOX).visible_for(accounts)
}

fn summary_to_message(
    thread: ThreadSummary,
    accounts: &[Account],
    folder: &str,
) -> Option<MailMessage> {
    let id = i32::try_from(thread.id).ok()?;
    // A conversation can be visible in Inbox and Sent at the same time. Keep
    // the row identified by the people on the other side of the exchange so a
    // newly sent reply does not make an Inbox row look like mail from oneself.
    let mut display_participants = thread
        .participants
        .iter()
        .filter(|person| {
            !thread
                .account_addresses
                .iter()
                .any(|own| own.eq_ignore_ascii_case(&person.email))
        })
        .collect::<Vec<_>>();
    if display_participants.is_empty() {
        display_participants.extend(thread.participants.iter());
    }
    let participant = display_participants.first().copied();
    let address = participant
        .map(|person| person.email.clone())
        .unwrap_or_default();
    fn display_name(person: &Address) -> &str {
        person
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(&person.email)
    }
    let sender = match display_participants.as_slice() {
        [] => address.clone(),
        [only] => display_name(only).to_owned(),
        [first, second] => format!("{}, {}", display_name(first), display_name(second)),
        [first, second, rest @ ..] => format!(
            "{}, {} +{}",
            display_name(first),
            display_name(second),
            rest.len()
        ),
    };
    let account = accounts
        .iter()
        .find(|account| account.id == thread.account_id)
        .map(account_label)
        .unwrap_or_else(|| thread.account_email.clone());
    let domain = domain_from_address(&address).unwrap_or_default();

    let initials = initials(&sender);
    Some(MailMessage {
        id,
        thread_id: Some(thread.id),
        account_id: thread.account_id,
        account,
        folder: folder.to_owned(),
        sender,
        address,
        domain,
        initials,
        subject: display_thread_subject(&thread.subject),
        preview: thread.snippet,
        date_ms: thread.last_message_at,
        to: String::new(),
        label: if thread.unread_count > 0 {
            "UNREAD".to_owned()
        } else {
            "MAIL".to_owned()
        },
        unread: thread.unread_count > 0,
        starred: thread.is_starred,
        has_attachments: thread.has_attachments,
        message_count: thread.message_count.max(1),
        attachments: Vec::new(),
        has_replied: thread.has_replied,
        is_outgoing: false,
        labels: thread.labels,
        html: None,
        text: None,
        body_pending: true,
        sender_verification: String::new(),
    })
}

/// Keep the newest message's display casing and local prefixes while removing
/// transport reply prefixes that belong to individual messages, not the
/// conversation title.
fn display_thread_subject(subject: &str) -> String {
    let mut remaining = subject.trim();
    let mut leading_tags = String::new();
    while let Some(tag_end) = remaining
        .strip_prefix('[')
        .and_then(|rest| rest.find(']').map(|index| index + 1))
    {
        let (tag, rest) = remaining.split_at(tag_end + 1);
        if !leading_tags.is_empty() {
            leading_tags.push(' ');
        }
        leading_tags.push_str(tag.trim());
        remaining = rest.trim_start();
    }
    while let Some(colon) = remaining.find(':') {
        let prefix = remaining[..colon].trim();
        let token = prefix
            .split_once('[')
            .map_or(prefix, |(token, _)| token)
            .trim();
        if !matches!(
            token.to_ascii_lowercase().as_str(),
            "re" | "fw" | "fwd" | "aw" | "sv"
        ) {
            break;
        }
        remaining = remaining[colon + 1..].trim_start();
    }
    match (leading_tags.is_empty(), remaining.is_empty()) {
        (true, _) => remaining.to_owned(),
        (false, true) => leading_tags,
        (false, false) => format!("{leading_tags} {remaining}"),
    }
}

fn detail_to_message(row: &MailMessage, message: &MessageDetail, body_loaded: bool) -> MailMessage {
    let sender = message
        .from
        .name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| message.from.email.clone());
    let html = body_loaded.then(|| {
        readable_message_html(
            message.html_body.as_deref(),
            message.text_body.as_deref(),
            &message.snippet,
        )
    });

    let initials = initials(&sender);
    MailMessage {
        id: row.id,
        thread_id: row.thread_id,
        account_id: row.account_id,
        account: row.account.clone(),
        folder: row.folder.clone(),
        sender,
        address: message.from.email.clone(),
        domain: domain_from_address(&message.from.email).unwrap_or_default(),
        initials,
        subject: message.subject.clone(),
        preview: message.snippet.clone(),
        date_ms: message.date,
        to: message
            .to
            .first()
            .map(|address| address.email.clone())
            .unwrap_or_default(),
        label: if message.is_draft {
            "DRAFT".to_owned()
        } else if message.is_outgoing {
            "SENT".to_owned()
        } else if message.is_read {
            "MAIL".to_owned()
        } else {
            "UNREAD".to_owned()
        },
        unread: !message.is_read,
        starred: row.starred,
        has_attachments: !message.attachments.is_empty(),
        message_count: row.message_count,
        attachments: if body_loaded {
            message.attachments.clone()
        } else {
            Vec::new()
        },
        has_replied: row.has_replied,
        is_outgoing: message.is_outgoing,
        labels: row.labels.clone(),
        html,
        text: if body_loaded {
            message.text_body.clone()
        } else {
            None
        },
        body_pending: message.body_state != "cached",
        sender_verification: message.sender_verification.as_str().to_owned(),
    }
}

fn detail_to_conversation_message(
    row: &MailMessage,
    message: &MessageDetail,
    body_loaded: bool,
) -> Option<MailMessage> {
    let mut projected = detail_to_message(row, message, body_loaded);
    projected.id = i32::try_from(message.id).ok()?;
    projected.thread_id = Some(message.thread_id);
    projected.account_id = message.account_id;
    projected.message_count = 1;
    projected.is_outgoing = message.is_outgoing;
    Some(projected)
}

fn readable_message_html(html: Option<&str>, text: Option<&str>, snippet: &str) -> String {
    html.filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            text.filter(|value| !value.trim().is_empty())
                .map(text_to_html)
        })
        // A newly opened message can have an allocated, but still empty,
        // body row while its MIME sections are in flight. Never turn that
        // transient state into a completely white reading pane.
        .unwrap_or_else(|| text_to_html(&display_preview(snippet)))
}

fn account_label(account: &Account) -> String {
    account
        .display_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| account.email.clone())
}

fn avatar_initials(label: &str) -> String {
    let mut words = label
        .split_whitespace()
        .filter_map(|word| word.chars().next());
    let Some(first) = words.next() else {
        return "?".to_owned();
    };
    let second = words.next();
    second.map_or_else(
        || first.to_uppercase().collect(),
        |second| format!("{}{}", first.to_uppercase(), second.to_uppercase()),
    )
}

fn folder_label(folder: &FolderInfo) -> String {
    match folder.role.as_deref() {
        Some("inbox") => "Inbox".to_owned(),
        Some("sent") => "Sent".to_owned(),
        Some("archive") => "Archive".to_owned(),
        Some("drafts") => "Drafts".to_owned(),
        Some("trash") => "Trash".to_owned(),
        Some("spam") => "Spam".to_owned(),
        _ => folder
            .is_jmap
            .then_some(" / ")
            .or(folder.delimiter.as_deref())
            .filter(|delimiter| !delimiter.is_empty())
            .and_then(|delimiter| folder.display_name.rsplit_once(delimiter))
            .map(|(_, leaf)| leaf.trim().to_owned())
            .unwrap_or_else(|| folder.display_name.clone()),
    }
}

fn view_for_folder(folder: &FolderInfo) -> View {
    match folder.role.as_deref() {
        Some("inbox") => View::Inbox,
        Some("sent") => View::Sent,
        Some("drafts") => View::Drafts,
        Some("archive") => View::Done,
        Some("trash") => View::Trash,
        Some("spam") => View::Spam,
        _ => View::All,
    }
}

fn initials(name: &str) -> String {
    let mut letters = name
        .split_whitespace()
        .filter_map(|part| part.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase();
    if letters.is_empty() {
        letters.push('?');
    }
    letters
}

fn text_to_html(text: &str) -> String {
    const PREFIX: &str = "<html><body style=\"font-family:Arial,sans-serif;padding:32px;line-height:1.6;color:#303348\"><pre style=\"margin:0;font:inherit;white-space:pre-wrap;overflow-wrap:anywhere\">";
    const SUFFIX: &str = "</pre></body></html>";
    let mut html = String::with_capacity(PREFIX.len() + text.len() + SUFFIX.len());
    html.push_str(PREFIX);
    crate::email_document::append_escaped(&mut html, text);
    html.push_str(SUFFIX);
    html
}

fn markdown_options() -> Options {
    Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_SMART_PUNCTUATION
}

/// Render the compose document to safe email HTML. CommonMark is the native
/// editor format; sanitizing the generated fragment keeps manually typed HTML
/// inert while retaining email-safe links, underline tags, and formatting.
fn markdown_to_html(markdown: &str) -> String {
    let mut fragment = String::new();
    html::push_html(&mut fragment, Parser::new_ext(markdown, markdown_options()));
    let fragment = flectar_mail_core::mime::sanitize_html(&fragment);
    format!(
        "<html><body style=\"margin:0;padding:0;font-family:Arial,sans-serif;font-size:14px;line-height:1.55;color:#202124\">{fragment}</body></html>"
    )
}

/// Produce the text/plain MIME alternative without leaking CommonMark tokens
/// such as `**` or link destinations into clients that prefer plain text.
#[cfg(test)]
fn markdown_to_plain_text(markdown: &str) -> String {
    fn ensure_break(output: &mut String) {
        if !output.ends_with('\n') {
            output.push('\n');
        }
    }

    let mut output = String::new();
    for event in Parser::new_ext(markdown, markdown_options()) {
        match event {
            Event::Text(text)
            | Event::Code(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => output.push_str(&text),
            Event::SoftBreak | Event::HardBreak => ensure_break(&mut output),
            Event::Rule => {
                ensure_break(&mut output);
                output.push_str("---\n");
            }
            Event::TaskListMarker(done) => output.push_str(if done { "[x] " } else { "[ ] " }),
            Event::Start(Tag::Item) => output.push_str("• "),
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::BlockQuote(_)
                | TagEnd::CodeBlock
                | TagEnd::Item,
            ) => ensure_break(&mut output),
            Event::Start(_)
            | Event::End(_)
            | Event::Html(_)
            | Event::InlineHtml(_)
            | Event::FootnoteReference(_) => {}
        }
    }
    output.trim_end().to_owned()
}

/// Fixture rows with one stable id per fixture account, in first-seen order.
#[cfg(test)]
pub fn fixture_messages() -> Vec<MailMessage> {
    let mut accounts = Vec::new();
    fixtures()
        .into_iter()
        .map(|fixture| {
            let mut message = MailMessage::from_fixture(fixture);
            let index = accounts
                .iter()
                .position(|account| *account == message.account)
                .unwrap_or_else(|| {
                    accounts.push(message.account.clone());
                    accounts.len() - 1
                });
            message.account_id = index as i64 + 1;
            message
        })
        .collect()
}

#[cfg(test)]
pub fn fixtures() -> Vec<EmailFixture> {
    #[derive(serde::Deserialize)]
    struct DemoData {
        emails: Vec<EmailFixture>,
        selected_to: String,
    }
    let mut data: DemoData =
        serde_json::from_str(include_str!("../resources/screenshots/demo-data.json"))
            .expect("valid shared screenshot fixture");
    for email in &mut data.emails {
        email.to.clone_from(&data.selected_to);
        // Renderer tests need an HTML document; screenshots render the shared
        // demo-email.html instead. Build test HTML from the same rows.
        email.html = markdown_to_html(&format!(
            "# {}\n\n{}\n\n[Open message](https://{}) · [Flectar Mail](https://flectar.com)",
            email.subject,
            email.preview,
            domain_from_address(&email.address).unwrap_or_default()
        ));
    }
    data.emails
}

#[cfg(test)]
mod tests {
    use super::{
        ComposeMessage, compose_args, display_thread_subject, mailbox_entries, markdown_to_html,
        markdown_to_plain_text, readable_message_html, resolve_scope, summary_to_message,
        validated_startup_scope,
    };
    use crate::mail_scope::{LabelKind, Scope, StandardMailbox};
    use flectar_mail_core::models::{
        Account, Address, AuthKind, FolderInfo, Label, MailProtocol, Provider, ThreadSummary, View,
    };

    fn test_account() -> Account {
        Account {
            id: 1,
            email: "person@example.com".into(),
            display_name: Some("Person".into()),
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            sync_state: "idle".into(),
            sync_error: None,
            can_create_top_level_mailbox: true,
            server_keeps_mail_days: None,
        }
    }

    #[test]
    fn single_account_scopes_preserve_every_standard_view() {
        for (provider, mail_protocol) in [
            (Provider::Generic, MailProtocol::Imap),
            (Provider::Gmail, MailProtocol::Gmail),
            (Provider::Microsoft, MailProtocol::Imap),
        ] {
            let mut account = test_account();
            account.provider = provider;
            account.mail_protocol = mail_protocol;
            account.display_name = Some("Work / Personal".into());
            let accounts = [account];
            let entries = mailbox_entries(&accounts, &[]);
            for mailbox in StandardMailbox::ALL {
                let unified = Scope::Unified(mailbox);
                let scope = unified.visible_for(&accounts);
                assert!(entries.iter().any(|entry| entry.scope == Some(scope)));
                assert_eq!(
                    validated_startup_scope(unified, &accounts, &[], &[]),
                    scope
                );
                let before = resolve_scope(unified, &[], &[]);
                let after = resolve_scope(scope, &[], &[]);
                assert_eq!(after.view, before.view);
                assert_eq!(after.account_id, Some(1));
                assert_eq!(after.folder_id, None);
                assert_eq!(after.title, mailbox.label());
            }
            let removed = Scope::Account {
                account_id: 2,
                mailbox: StandardMailbox::Sent,
            };
            assert_eq!(
                validated_startup_scope(removed, &accounts, &[], &[]),
                Scope::Account {
                    account_id: 1,
                    mailbox: StandardMailbox::Inbox
                }
            );
        }
    }

    #[test]
    fn startup_keeps_known_navigation_and_drops_stale_targets() {
        let mut second = test_account();
        second.id = 2;
        let accounts = [test_account(), second];
        let folders = [FolderInfo {
            id: 42,
            account_id: 2,
            parent_id: None,
            display_name: "Receipts".into(),
            is_jmap: false,
            path: "Receipts".into(),
            delimiter: Some("/".into()),
            role: None,
            selectable: true,
            can_create_children: true,
            can_rename: true,
            can_delete: true,
        }];
        let kept = [
            Scope::Unified(StandardMailbox::Drafts),
            Scope::Important,
            Scope::Other,
            Scope::Folder(42),
            Scope::Account {
                account_id: 2,
                mailbox: StandardMailbox::Spam,
            },
        ];
        for scope in kept {
            assert_eq!(
                validated_startup_scope(scope, &accounts, &folders, &[]),
                scope
            );
        }
        let stale = [
            Scope::Folder(43),
            Scope::Account {
                account_id: 3,
                mailbox: StandardMailbox::Inbox,
            },
            Scope::Label {
                kind: LabelKind::Global,
                id: 9,
            },
        ];
        for scope in stale {
            assert_eq!(
                validated_startup_scope(scope, &accounts, &folders, &[]),
                Scope::UNIFIED_INBOX
            );
        }
    }

    #[test]
    fn empty_in_flight_body_falls_back_to_the_message_snippet() {
        let html = readable_message_html(Some("  \n"), Some(""), "Instagram message preview");

        assert!(html.contains("Instagram message preview"));
        assert!(!html.trim().is_empty());
    }

    #[test]
    fn rich_body_wins_when_it_is_available() {
        let html = readable_message_html(
            Some("<main><strong>Rich message</strong></main>"),
            Some("plain message"),
            "preview",
        );

        assert_eq!(html, "<main><strong>Rich message</strong></main>");
    }

    #[test]
    fn plain_text_body_preserves_reply_lines_and_quote_depth() {
        let text = "Hi Tomas,\n\n> Earlier reply\n>> Older reply";
        let html = readable_message_html(None, Some(text), "preview");
        let prepared = crate::renderer::prepare_email_html(&html).unwrap();

        assert!(html.contains("white-space:pre-wrap"));
        assert!(html.contains("&gt; Earlier reply\n&gt;&gt; Older reply"));
        assert_eq!(prepared.plain_text, text);
    }

    #[test]
    fn thread_summary_projects_participants_and_message_count() {
        let row = summary_to_message(
            ThreadSummary {
                id: 22,
                account_id: 1,
                account_email: "person@example.com".into(),
                account_addresses: vec!["PERSON@EXAMPLE.COM".into()],
                subject: "Re: Launch review".into(),
                snippet: "Tuesday works for everyone.".into(),
                participants: vec![
                    Address {
                        name: Some("Me".into()),
                        email: "person@example.com".into(),
                    },
                    Address {
                        name: Some("Maya".into()),
                        email: "maya@example.com".into(),
                    },
                    Address {
                        name: Some("Alex".into()),
                        email: "alex@example.com".into(),
                    },
                    Address {
                        name: None,
                        email: "lee@example.com".into(),
                    },
                ],
                last_message_at: 0,
                message_count: 4,
                unread_count: 1,
                is_starred: false,
                has_attachments: false,
                has_replied: true,
                snoozed_until: None,
                labels: Vec::new(),
            },
            &[test_account()],
            "Inbox",
        )
        .unwrap();

        assert_eq!(row.sender, "Maya, Alex +1");
        assert_eq!(row.address, "maya@example.com");
        assert_eq!(row.subject, "Launch review");
        assert_eq!(row.message_count, 4);
        assert_eq!(row.id, 22);
    }

    #[test]
    fn conversation_subject_drops_reply_prefixes_and_keeps_local_tags() {
        assert_eq!(display_thread_subject("Re: Re[2]: Launch"), "Launch");
        assert_eq!(
            display_thread_subject("[INVOICE] Re: Payment"),
            "[INVOICE] Payment"
        );
        assert_eq!(
            display_thread_subject("Re: [team] Standup"),
            "[team] Standup"
        );
    }

    #[test]
    fn compose_markdown_generates_rich_and_plain_mime_bodies() {
        let source = "Hello **Maya** — see [the plan](https://example.com).\n\n<u>Important</u>";
        let html = markdown_to_html(source);
        let plain = markdown_to_plain_text(source);

        assert!(html.contains("<strong>Maya</strong>"));
        assert!(html.contains("href=\"https://example.com\""));
        assert!(html.contains("<u>Important</u>"));
        assert_eq!(plain, "Hello Maya — see the plan.\nImportant");
    }

    #[test]
    fn compose_html_strips_executable_markup() {
        let html = markdown_to_html("Safe<script>alert('no')</script> text");

        assert!(!html.contains("<script"));
        assert!(!html.contains("alert('no')"));
        assert!(html.contains("Safe"));
    }

    #[test]
    fn incomplete_drafts_do_not_require_a_recipient() {
        let message = || ComposeMessage {
            draft_id: None,
            account_id: 7,
            sender_email: None,
            to: "",
            cc: "",
            bcc: "",
            subject: "",
            body: "notes",
            body_html: None,
            attachments: &[],
            mode: "new",
            in_reply_to_message_id: None,
        };
        let draft = compose_args(message(), false).unwrap();

        assert!(draft.to.is_empty());
        assert_eq!(draft.account_id, 7);
        assert_eq!(draft.body_text, "notes");
        assert!(compose_args(message(), true).is_err());
    }

    #[test]
    fn compose_uses_selected_account_and_direct_rich_html() {
        let draft = compose_args(
            ComposeMessage {
                draft_id: Some(17),
                account_id: 42,
                sender_email: Some("work@example.com"),
                to: "maya@example.com",
                cc: "",
                bcc: "",
                subject: "Update",
                body: "Hello Maya",
                body_html: Some("<div>Hello <strong>Maya</strong></div>"),
                attachments: &[],
                mode: "reply",
                in_reply_to_message_id: Some(99),
            },
            true,
        )
        .unwrap();

        assert_eq!(draft.account_id, 42);
        assert_eq!(draft.draft_id, Some(17));
        assert_eq!(draft.body_text, "Hello Maya");
        assert_eq!(draft.mode, "reply");
        assert_eq!(draft.in_reply_to_message_id, Some(99));
        assert_eq!(
            draft.body_html.as_deref(),
            Some("<div>Hello <strong>Maya</strong></div>")
        );
    }

    #[test]
    fn mailbox_entries_preserve_unicode_hierarchy_and_stable_ids() {
        let folders = vec![
            FolderInfo {
                id: 10,
                account_id: 1,
                parent_id: None,
                display_name: "☺ Projects".into(),
                is_jmap: false,
                path: "&Jjo- Projects".into(),
                delimiter: Some("/".into()),
                role: None,
                selectable: true,
                can_create_children: true,
                can_rename: true,
                can_delete: true,
            },
            FolderInfo {
                id: 11,
                account_id: 1,
                parent_id: Some(10),
                display_name: "☺ Projects/2026".into(),
                is_jmap: false,
                path: "&Jjo- Projects/2026".into(),
                delimiter: Some("/".into()),
                role: None,
                selectable: true,
                can_create_children: true,
                can_rename: true,
                can_delete: true,
            },
            FolderInfo {
                id: 12,
                account_id: 1,
                parent_id: None,
                display_name: "Archive copy/2026".into(),
                is_jmap: false,
                path: "Archive copy/2026".into(),
                delimiter: Some("/".into()),
                role: None,
                selectable: true,
                can_create_children: true,
                can_rename: true,
                can_delete: true,
            },
            FolderInfo {
                id: 13,
                account_id: 1,
                parent_id: None,
                display_name: "Inbox".into(),
                is_jmap: false,
                path: "Inbox".into(),
                delimiter: Some("/".into()),
                role: None,
                selectable: true,
                can_create_children: true,
                can_rename: true,
                can_delete: true,
            },
        ];

        let entries = mailbox_entries(&[test_account()], &folders);
        let parent = entries.iter().find(|entry| entry.folder_id == 10).unwrap();
        let child = entries.iter().find(|entry| entry.folder_id == 11).unwrap();
        let duplicate_leaf = entries.iter().find(|entry| entry.folder_id == 12).unwrap();
        let custom_inbox = entries.iter().find(|entry| entry.folder_id == 13).unwrap();
        assert_eq!(parent.label, "☺ Projects");
        assert!(parent.has_children);
        assert_eq!(child.label, "2026");
        assert_eq!(child.parent_folder_id, parent.folder_id);
        assert_eq!(child.depth, 1);
        assert_eq!(child.scope, Some(Scope::Folder(11)));
        assert_eq!(duplicate_leaf.scope, Some(Scope::Folder(12)));
        assert_eq!(custom_inbox.scope, Some(Scope::Folder(13)));
        assert!(custom_inbox.count.is_empty());
    }

    #[test]
    fn gmail_user_labels_are_not_duplicated_as_account_folders() {
        let mut account = test_account();
        account.provider = Provider::Gmail;
        account.auth_kind = AuthKind::Oauth2;
        account.mail_protocol = MailProtocol::Gmail;
        let folders = vec![FolderInfo {
            id: 10,
            account_id: account.id,
            parent_id: None,
            display_name: "Travel".into(),
            is_jmap: false,
            path: "Travel".into(),
            delimiter: Some("/".into()),
            role: None,
            selectable: true,
            can_create_children: true,
            can_rename: true,
            can_delete: true,
        }];

        let entries = mailbox_entries(&[account], &folders);

        assert!(entries.iter().all(|entry| entry.folder_id != 10));
        assert_eq!(
            entries.iter().filter(|entry| !entry.is_account).count(),
            StandardMailbox::ALL.len()
        );
    }

    #[test]
    fn standard_archive_is_the_parent_of_its_complete_folder_tree() {
        let folder = |id, parent_id, name: &str, role: Option<String>, selectable| FolderInfo {
            id,
            account_id: 1,
            parent_id,
            display_name: name.into(),
            is_jmap: false,
            path: name.into(),
            delimiter: Some("/".into()),
            role: role.clone(),
            selectable,
            can_create_children: true,
            can_rename: role.is_none(),
            can_delete: role.is_none(),
        };
        let folders = vec![
            folder(10, None, "Archive", Some("archive".into()), true),
            folder(11, Some(10), "Archive/2025", None, false),
            folder(12, Some(11), "Archive/2025/GitHub", None, true),
            folder(13, Some(10), "Archive/2026", None, false),
            folder(14, Some(13), "Archive/2026/GitHub", None, true),
        ];

        let entries = mailbox_entries(&[test_account()], &folders);
        let projected = entries
            .iter()
            .filter(|entry| !entry.is_account)
            .map(|entry| {
                (
                    entry.folder_id,
                    entry.parent_folder_id,
                    entry.depth,
                    entry.label.as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            projected,
            vec![
                (-1, -1, 0, "Inbox"),
                (-1, -1, 0, "Starred"),
                (-1, -1, 0, "Sent"),
                (10, -1, 0, "Archive"),
                (11, 10, 1, "2025"),
                (12, 11, 2, "GitHub"),
                (13, 10, 1, "2026"),
                (14, 13, 2, "GitHub"),
                (-1, -1, 0, "Spam"),
                (-1, -1, 0, "Trash"),
                (-1, -1, 0, "Drafts"),
            ]
        );

        let archive = entries.iter().find(|entry| entry.folder_id == 10).unwrap();
        let year = entries.iter().find(|entry| entry.folder_id == 11).unwrap();
        assert!(archive.is_standard);
        assert!(archive.has_children);
        assert!(archive.can_create_children);
        assert!(!archive.can_rename);
        assert!(!archive.can_delete);
        assert!(!year.is_selectable);
        assert!(year.has_children);
        assert!(year.can_create_children);
    }

    #[test]
    fn category_and_label_scopes_keep_their_core_filters() {
        let labels = vec![
            Label {
                id: 31,
                name: "Projects".into(),
                color: "#16a765".into(),
                keyword: "Projects".into(),
                position: 0,
                owner_account_id: None,
                is_auto: false,
            },
            Label {
                id: 32,
                name: "Newsletters".into(),
                color: "#a479e2".into(),
                keyword: "newsletters".into(),
                position: 1,
                owner_account_id: None,
                is_auto: true,
            },
        ];

        let label = |kind, id| Scope::Label { kind, id };
        let important = resolve_scope(Scope::Important, &[], &labels);
        assert_eq!(important.view, View::Inbox);
        assert_eq!(important.split_id, Some(-1));

        let manual = resolve_scope(label(LabelKind::Global, 31), &[], &labels);
        assert_eq!(manual.view, View::All);
        assert_eq!(manual.label_id, Some(31));
        assert_eq!(manual.account_id, None);
        assert_eq!(manual.title, "Projects");

        let automatic = resolve_scope(label(LabelKind::Category, 32), &[], &labels);
        assert_eq!(automatic.view, View::Inbox);
        assert_eq!(automatic.label_id, Some(32));
        assert_eq!(automatic.title, "Newsletters");
        // Saved selections follow a label into its current sidebar section.
        assert_eq!(
            validated_startup_scope(label(LabelKind::Global, 32), &[], &[], &labels),
            label(LabelKind::Category, 32)
        );
        assert_eq!(
            validated_startup_scope(label(LabelKind::Category, 31), &[], &[], &labels),
            label(LabelKind::Global, 31)
        );
        assert_eq!(
            validated_startup_scope(label(LabelKind::Global, 999), &[], &[], &labels),
            Scope::UNIFIED_INBOX
        );

        let account_label = Label {
            id: 33,
            name: "Travel".into(),
            color: "#2563eb".into(),
            keyword: "Travel".into(),
            position: 0,
            owner_account_id: Some(7),
            is_auto: false,
        };
        let account_scope = resolve_scope(
            label(LabelKind::Account, 33),
            &[],
            std::slice::from_ref(&account_label),
        );
        assert_eq!(account_scope.account_id, Some(7));
        assert_eq!(account_scope.label_id, Some(33));
        assert_eq!(
            validated_startup_scope(label(LabelKind::Global, 33), &[], &[], &[account_label]),
            label(LabelKind::Account, 33)
        );
    }
}

#[cfg(test)]
mod search_pagination_adapter_tests {
    use super::*;
    use flectar_mail_core::accounts::credentials::DevelopmentFileCredentialStore;

    #[tokio::test]
    async fn search_and_inbox_pages_keep_their_own_cursors() {
        let dir = tempfile::tempdir().unwrap();
        let source = CoreMailSource::start(
            Paths::for_tests(dir.path()),
            Arc::new(DevelopmentFileCredentialStore::new(
                dir.path().join("credentials.json"),
            )),
            Arc::new(flectar_mail_core::oauth::redirect::LoopbackRedirectBroker::default()),
        )
        .await
        .unwrap();
        source
            .core
            .db
            .write(|conn| {
                conn.execute(
                    "INSERT INTO accounts (id,email,provider,auth_kind,username,incoming_host,incoming_port,smtp_host,smtp_port,created_at,mail_protocol,settings_json)
                     VALUES (1,'fixture@example.test','generic','password','fixture','imap.example.test',993,'smtp.example.test',465,0,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO folders (id,account_id,path,role) VALUES (1,1,'INBOX','inbox')",
                    [],
                )?;
                for id in 1..=27_i64 {
                    conn.execute(
                        "INSERT INTO threads (id,account_id,subject_norm,last_message_at,message_count)
                         VALUES (?1,1,'needle paging',?1,1)",
                        [id],
                    )?;
                    conn.execute(
                        "INSERT INTO messages (id,account_id,thread_id,folder_id,uid,subject,from_addr,date,is_read,body_state)
                         VALUES (?1,1,?1,1,?1,'Needle paging','sender@example.test',?1,1,'cached')",
                        [id],
                    )?;
                    flectar_mail_core::db::repo::search::index_message(conn, id)?;
                }
                Ok(())
            })
            .await
            .unwrap();

        for query in ["needle", ""] {
            let first = source
                .load_page(Scope::UNIFIED_INBOX, query, None, 25, false)
                .await
                .unwrap();
            assert_eq!(first.messages.len(), 25);
            let cursor = first.next_cursor.expect("more mail remains");
            assert_eq!(matches!(cursor, MailCursor::Search(_)), !query.is_empty());
            let second = source
                .load_page(Scope::UNIFIED_INBOX, query, Some(cursor), 25, false)
                .await
                .unwrap();
            assert_eq!(second.messages.len(), 2);
            assert!(second.next_cursor.is_none());
            let ids = first
                .messages
                .into_iter()
                .chain(second.messages)
                .map(|message| message.id)
                .collect::<Vec<_>>();
            assert_eq!(ids, (1..=27).rev().collect::<Vec<_>>());
        }
    }

    #[tokio::test]
    async fn profile_accounts_limit_pages_search_counts_and_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let source = CoreMailSource::start(
            Paths::for_tests(dir.path()),
            Arc::new(DevelopmentFileCredentialStore::new(
                dir.path().join("credentials.json"),
            )),
            Arc::new(flectar_mail_core::oauth::redirect::LoopbackRedirectBroker::default()),
        )
        .await
        .unwrap();
        // Account 1 has threads 1-3 and account 2 threads 11-12, all unread
        // in their inboxes and matching "needle".
        source
            .core
            .db
            .write(|conn| {
                for account in [1_i64, 2] {
                    conn.execute(
                        "INSERT INTO accounts (id,email,provider,auth_kind,username,incoming_host,incoming_port,smtp_host,smtp_port,created_at,mail_protocol,settings_json)
                         VALUES (?1,'fixture'||?1||'@example.test','generic','password','fixture','imap.example.test',993,'smtp.example.test',465,0,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
                        [account],
                    )?;
                    conn.execute(
                        "INSERT INTO folders (id,account_id,path,role) VALUES (?1,?1,'INBOX','inbox')",
                        [account],
                    )?;
                }
                for (account, id) in [(1_i64, 1_i64), (1, 2), (1, 3), (2, 11), (2, 12)] {
                    conn.execute(
                        "INSERT INTO threads (id,account_id,subject_norm,last_message_at,message_count,unread_count)
                         VALUES (?2,?1,'needle',?2,1,1)",
                        [account, id],
                    )?;
                    conn.execute(
                        "INSERT INTO messages (id,account_id,thread_id,folder_id,uid,subject,from_addr,date,is_read,body_state)
                         VALUES (?2,?1,?2,?1,?2,'Needle','sender@example.test',?2,0,'cached')",
                        [account, id],
                    )?;
                    flectar_mail_core::db::repo::search::index_message(conn, id)?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let listed = |page: &MailPage| {
            page.messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>()
        };
        let shown_accounts = |mailboxes: &[MailboxEntry]| {
            let mut accounts = mailboxes
                .iter()
                .map(|mailbox| mailbox.account_id)
                .collect::<Vec<_>>();
            accounts.dedup();
            accounts
        };

        let all = source
            .load_page(Scope::UNIFIED_INBOX, "", None, 25, false)
            .await
            .unwrap();
        assert_eq!(listed(&all), vec![12, 11, 3, 2, 1]);
        assert_eq!(
            source
                .load_mail_metadata(Scope::UNIFIED_INBOX)
                .await
                .unwrap()
                .inbox_count,
            5
        );

        let revision = source.account_revision();
        assert!(source.set_profile_accounts(Some(vec![2])));
        assert!(!source.set_profile_accounts(Some(vec![2, 2])));
        assert_ne!(
            source.account_revision(),
            revision,
            "older pages are retired"
        );

        let inbox = source
            .load_page(Scope::UNIFIED_INBOX, "", None, 25, false)
            .await
            .unwrap();
        assert_eq!(listed(&inbox), vec![12, 11]);
        assert_eq!(shown_accounts(&inbox.mailboxes), vec![2]);
        assert_eq!(inbox.account_count, 2, "startup still sees every account");
        let search = source
            .load_page(Scope::UNIFIED_INBOX, "needle", None, 25, false)
            .await
            .unwrap();
        assert_eq!(listed(&search), vec![12, 11]);
        // An account the profile leaves out shows the profile's inbox.
        let hidden = source
            .load_page(
                Scope::Account {
                    account_id: 1,
                    mailbox: StandardMailbox::Inbox,
                },
                "",
                None,
                25,
                false,
            )
            .await
            .unwrap();
        assert_eq!(listed(&hidden), vec![12, 11]);
        let metadata = source
            .load_mail_metadata(Scope::UNIFIED_INBOX)
            .await
            .unwrap();
        assert_eq!(metadata.inbox_count, 2);
        assert_eq!(shown_accounts(&metadata.mailboxes), vec![2]);

        // Startup opens a saved mailbox of a hidden account at the shown
        // account's inbox, but still reports every account.
        let (accounts, scope, page) = source
            .load_startup_page(
                Scope::Account {
                    account_id: 1,
                    mailbox: StandardMailbox::Inbox,
                },
                25,
            )
            .await
            .unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(
            scope,
            Scope::Account {
                account_id: 2,
                mailbox: StandardMailbox::Inbox,
            }
        );
        assert_eq!(listed(&page.unwrap()), vec![12, 11]);

        assert!(source.set_profile_accounts(None));
        let all = source
            .load_page(Scope::UNIFIED_INBOX, "needle", None, 25, false)
            .await
            .unwrap();
        assert_eq!(listed(&all), vec![12, 11, 3, 2, 1]);
    }
}

#[cfg(test)]
mod account_removal_snapshot_tests {
    use super::*;
    use flectar_mail_core::accounts::credentials::{
        CredentialStore, DevelopmentFileCredentialStore, Slot,
    };
    use flectar_mail_core::error::{CoreError, Result as CoreResult};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FailingCredentials {
        inner: DevelopmentFileCredentialStore,
        fail: AtomicBool,
    }
    impl CredentialStore for FailingCredentials {
        fn store(&self, account: i64, slot: Slot, value: &str) -> CoreResult<()> {
            self.inner.store(account, slot, value)
        }
        fn load(&self, account: i64, slot: Slot) -> CoreResult<String> {
            self.inner.load(account, slot)
        }
        fn delete(&self, account: i64, slot: Slot) -> CoreResult<()> {
            self.inner.delete(account, slot)
        }
        fn delete_all(&self, account: i64) -> CoreResult<()> {
            if self.fail.load(Ordering::Relaxed) {
                return Err(CoreError::CredentialStoreUnavailable(
                    "fixture failure".into(),
                ));
            }
            self.inner.delete_all(account)
        }
    }
    async fn seed(source: &CoreMailSource) {
        source.core.db.write(|conn| {
            conn.execute("INSERT INTO accounts (id, email, provider, auth_kind, username, incoming_host, incoming_port, smtp_host, smtp_port, created_at, mail_protocol, settings_json)
                VALUES (1, 'fixture@example.test', 'generic', 'password', 'fixture', 'imap.example.test', 993, 'smtp.example.test', 465, 0, 'imap', '{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')", [])?;
            Ok(())
        }).await.unwrap();
    }
    #[tokio::test]
    async fn account_removal_failure_preserves_account_and_invalidates_old_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FailingCredentials {
            inner: DevelopmentFileCredentialStore::new(dir.path().join("credentials.json")),
            fail: AtomicBool::new(false),
        });
        let source = CoreMailSource::start(
            Paths::for_tests(dir.path()),
            store.clone(),
            Arc::new(flectar_mail_core::oauth::redirect::LoopbackRedirectBroker::default()),
        )
        .await
        .unwrap();
        seed(&source).await;
        let before = source.load_account_snapshot().await.unwrap();
        let metadata = source.load_mail_metadata(Scope::UNIFIED_INBOX).await.unwrap();
        let page = source
            .load_page(Scope::UNIFIED_INBOX, "", None, 50, false)
            .await
            .unwrap();
        store.fail.store(true, Ordering::Relaxed);
        let result = source.remove_account(1).await;
        assert!(result.is_err());
        let after = source.load_account_snapshot().await.unwrap();
        assert!(!crate::account_controller::account_was_removed(
            1,
            &result,
            Some(&after)
        ));
        assert_eq!(after.accounts.len(), 1);
        assert_ne!(source.account_revision(), before.revision);
        assert_ne!(source.account_revision(), metadata.account_revision);
        assert_ne!(source.account_revision(), page.account_revision);
        store.fail.store(false, Ordering::Relaxed);
        source.remove_account(1).await.unwrap();
        assert!(source.load_accounts().await.unwrap().is_empty());
        // Account IDs may be reused by SQLite. Revisions must not act as
        // permanent tombstones that hide a newly connected account.
        seed(&source).await;
        let replacement = source.load_account_snapshot().await.unwrap();
        assert_eq!(replacement.accounts.len(), 1);
        assert_eq!(replacement.revision, source.account_revision());
    }
    #[tokio::test]
    async fn account_removal_late_failure_reflects_deleted_account() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::for_tests(dir.path());
        let source = CoreMailSource::start(
            paths.clone(),
            Arc::new(DevelopmentFileCredentialStore::new(
                dir.path().join("credentials.json"),
            )),
            Arc::new(flectar_mail_core::oauth::redirect::LoopbackRedirectBroker::default()),
        )
        .await
        .unwrap();
        seed(&source).await;
        let cache = paths.mail_dir(1);
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(cache, b"not a directory").unwrap();
        let result = source.remove_account(1).await;
        assert!(result.is_err());
        let after = source.load_account_snapshot().await.unwrap();
        assert!(crate::account_controller::account_was_removed(
            1,
            &result,
            Some(&after)
        ));
        assert!(after.accounts.is_empty());
    }
}
