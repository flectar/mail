use crate::favicon::domain_from_address;
use chrono::{DateTime, Datelike, Local, Utc};
use flectar_mail_core::{
    Core,
    config::Paths,
    events::CoreEvent,
    models::{
        Account, AccountConfig, ActionKind, ActionParams, AddPasswordAccountArgs, Address,
        CalendarConnection, CalendarEvent, ConnectCalendarArgs, ContactRecordCursor,
        ContactRecordPage, CreateEventArgs, CustomTheme, DraftAttachmentIn, FolderInfo, Label,
        MailHistory, MailboxBadgeCounts, MessageDetail, PerformActionArgs, PortableAccountConfig,
        Provider, QueueSendArgs, QueueSendResult, SaveDraftArgs, Settings, ThreadCursor,
        ThreadSummary, View,
    },
};
#[cfg(test)]
use pulldown_cmark::{Event, Tag, TagEnd};
use pulldown_cmark::{Options, Parser, html};
use std::sync::Arc;

#[cfg(test)]
#[derive(Clone, Copy)]
pub struct EmailFixture {
    pub id: i32,
    pub account: &'static str,
    pub folder: &'static str,
    pub sender: &'static str,
    pub address: &'static str,
    pub initials: &'static str,
    pub subject: &'static str,
    pub preview: &'static str,
    pub time: &'static str,
    pub to: &'static str,
    pub label: &'static str,
    pub unread: bool,
    pub html: &'static str,
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
    pub time: String,
    pub to: String,
    pub label: String,
    pub unread: bool,
    pub starred: bool,
    pub has_attachments: bool,
    pub attachments: Vec<flectar_mail_core::models::AttachmentMeta>,
    pub has_replied: bool,
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
            domain: domain_from_address(email.address).unwrap_or_default(),
            initials: email.initials.to_owned(),
            subject: email.subject.to_owned(),
            preview: email.preview.to_owned(),
            time: email.time.to_owned(),
            to: email.to.to_owned(),
            label: email.label.to_owned(),
            unread: email.unread,
            starred: false,
            has_attachments: false,
            attachments: Vec::new(),
            has_replied: false,
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
    pub label: String,
    pub scope: String,
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
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub date: i64,
    pub text_body: String,
}

#[derive(Clone, Debug)]
pub struct MailPage {
    pub messages: Vec<MailMessage>,
    pub labels: Vec<Label>,
    pub mailboxes: Vec<MailboxEntry>,
    pub next_cursor: Option<ThreadCursor>,
    pub account_count: usize,
    pub unified_mailboxes: Vec<MailboxEntry>,
}

#[derive(Clone, Debug)]
pub struct MailMetadata {
    pub scope: String,
    pub scope_total: usize,
    pub inbox_count: usize,
    pub mailboxes: Vec<MailboxEntry>,
    pub unified_mailboxes: Vec<MailboxEntry>,
}

struct PageLoadContext<'a> {
    scope: &'a str,
    query: &'a str,
    cursor: Option<ThreadCursor>,
    limit: i64,
    include_counts: bool,
    accounts: &'a [Account],
    folders: &'a [FolderInfo],
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
}

impl CoreMailSource {
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
        scope: &str,
        query: &str,
        cursor: Option<ThreadCursor>,
        limit: i64,
        include_counts: bool,
    ) -> Result<MailPage, String> {
        let accounts = self
            .core
            .list_accounts()
            .await
            .map_err(|error| error.to_string())?;
        let folders = self
            .core
            .list_folders(None)
            .await
            .map_err(|error| error.to_string())?;
        self.load_page_with_context(PageLoadContext {
            scope,
            query,
            cursor,
            limit,
            include_counts,
            accounts: &accounts,
            folders: &folders,
        })
        .await
    }

    /// Startup needs the account list and first page together. Loading them in
    /// one adapter call avoids querying accounts a second time while retaining
    /// the account list if folder/page projection happens to fail.
    pub async fn load_startup_page(
        &self,
        preferred_scope: &str,
        limit: i64,
    ) -> Result<(Vec<Account>, String, Option<MailPage>), String> {
        let accounts = self
            .core
            .list_accounts()
            .await
            .map_err(|error| error.to_string())?;
        let (scope, page) = match self.core.list_folders(None).await {
            Ok(folders) => {
                let labels = self.core.list_labels().await.unwrap_or_default();
                let scope = validated_startup_scope(preferred_scope, &accounts, &folders, &labels);
                let page = self
                    .load_page_with_context(PageLoadContext {
                        scope: &scope,
                        query: "",
                        cursor: None,
                        limit,
                        include_counts: false,
                        accounts: &accounts,
                        folders: &folders,
                    })
                    .await
                    .ok();
                (scope, page)
            }
            Err(_) => ("Unified Inbox".to_owned(), None),
        };
        Ok((accounts, scope, page))
    }

    /// Load exact sidebar badges and the selected folder's total independently
    /// of the latency-sensitive message page. Core events can therefore update
    /// counters live without delaying list rendering.
    pub async fn load_mail_metadata(&self, scope: &str) -> Result<MailMetadata, String> {
        let accounts = self
            .core
            .list_accounts()
            .await
            .map_err(|error| error.to_string())?;
        let folders = self
            .core
            .list_folders(None)
            .await
            .map_err(|error| error.to_string())?;
        let labels = self
            .core
            .list_labels()
            .await
            .map_err(|error| error.to_string())?;
        self.load_mail_metadata_with_context(scope, &accounts, &folders, &labels)
            .await
    }

    async fn load_mail_metadata_with_context(
        &self,
        scope: &str,
        accounts: &[Account],
        folders: &[FolderInfo],
        labels: &[Label],
    ) -> Result<MailMetadata, String> {
        let badges = self
            .core
            .mailbox_badge_counts()
            .await
            .map_err(|error| error.to_string())?;
        let resolved = resolve_scope(scope, accounts, folders, labels);
        let scope_total = count_threads(&self.core, &resolved).await?;
        Ok(MailMetadata {
            scope: scope.to_owned(),
            scope_total,
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
        } = context;
        let labels = self
            .core
            .list_labels()
            .await
            .map_err(|error| error.to_string())?;
        let metadata = if include_counts {
            Some(
                self.load_mail_metadata_with_context(scope, accounts, folders, &labels)
                    .await?,
            )
        } else {
            None
        };
        let mailboxes = metadata
            .as_ref()
            .map(|metadata| metadata.mailboxes.clone())
            .unwrap_or_else(|| mailbox_entries(accounts, folders));
        let resolved = resolve_scope(scope, accounts, folders, &labels);

        let (threads, next_cursor) = if query.trim().is_empty() {
            let page = self
                .core
                .list_threads(
                    resolved.view,
                    resolved.split_id,
                    resolved.account_id,
                    resolved.label_id,
                    resolved.folder_id,
                    (cursor, limit),
                )
                .await
                .map_err(|error| error.to_string())?;
            (page.threads, page.next_cursor)
        } else {
            // The mail list is a timeline: search narrows it, then presents
            // the newest matching threads first. Relevance-ranked retrieval
            // remains available to non-UI core consumers.
            let results = self
                .core
                .search_chronological(query.trim().to_owned(), resolved.account_id, limit)
                .await
                .map_err(|error| error.to_string())?;
            (results, None)
        };

        let messages = threads
            .into_iter()
            .filter_map(|thread| summary_to_message(thread, accounts, resolved.title.as_str()))
            .collect();

        Ok(MailPage {
            messages,
            labels,
            mailboxes,
            next_cursor,
            account_count: accounts.len(),
            unified_mailboxes: metadata
                .map(|metadata| metadata.unified_mailboxes)
                .unwrap_or_default(),
        })
    }

    pub async fn load_message(&self, row: &MailMessage) -> Result<MailMessage, String> {
        let thread_id = row
            .thread_id
            .ok_or_else(|| "message is not backed by a core thread".to_owned())?;
        let message = self
            .core
            .get_latest_thread_body(thread_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(detail_to_message(row, &message))
    }

    pub async fn load_compose_source(&self, row: &MailMessage) -> Result<ComposeSource, String> {
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
            .last()
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

        Ok(ComposeSource {
            message_id: message.id,
            account_id: message.account_id,
            account_email,
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

    pub async fn set_mark_read_on_open(&self, enabled: bool) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.mark_read_on_open = enabled;
        self.core
            .set_settings(settings)
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
        settings.language = match language {
            "es" => "es",
            "en" => "en",
            _ => "system",
        }
        .to_owned();
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

    pub async fn set_close_to_tray(&self, enabled: bool) -> Result<(), String> {
        let mut settings = self.load_settings().await?;
        settings.close_to_tray = enabled;
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
        self.core
            .remove_account(account_id)
            .await
            .map_err(|error| error.to_string())
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

    pub async fn load_contact_page(
        &self,
        scope: String,
        query: String,
        cursor: Option<ContactRecordCursor>,
        limit: i64,
    ) -> Result<ContactRecordPage, String> {
        let favorites_only = scope == "Favorites";
        let account_id = scope
            .strip_prefix("Account:")
            .and_then(|value| value.parse().ok());
        self.core
            .list_contact_record_page(query, account_id, favorites_only, cursor, limit)
            .await
            .map_err(|error| error.to_string())
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

    pub async fn delete_contact(&self, id: i64) -> Result<(), String> {
        self.core
            .delete_contact(id)
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
                .filter(|label| !label.is_auto)
                .map(|label| label.position)
                .max()
                .unwrap_or(-1)
                .saturating_add(1),
        };
        self.core
            .save_label(id, name.to_owned(), color.to_owned(), position)
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
            Provider::Imap => return Err("use CalDAV for an IMAP account".into()),
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

const STANDARD_ACCOUNT_FOLDERS: [&str; 7] = [
    "Inbox", "Starred", "Sent", "Archive", "Spam", "Trash", "Drafts",
];

fn standard_account_view(label: &str) -> Option<View> {
    match label {
        "Inbox" => Some(View::Inbox),
        "Starred" => Some(View::Starred),
        "Sent" => Some(View::Sent),
        "Archive" => Some(View::Done),
        "Spam" => Some(View::Spam),
        "Trash" => Some(View::Trash),
        "Drafts" => Some(View::Drafts),
        _ => None,
    }
}

fn standard_folder_role(label: &str) -> Option<&'static str> {
    match label {
        "Inbox" => Some("inbox"),
        "Sent" => Some("sent"),
        "Archive" => Some("archive"),
        "Spam" => Some("spam"),
        "Trash" => Some("trash"),
        "Drafts" => Some("drafts"),
        _ => None,
    }
}

fn is_standard_folder(folder: &FolderInfo) -> bool {
    matches!(
        folder.role.as_deref(),
        Some("inbox" | "sent" | "archive" | "spam" | "trash" | "drafts")
    )
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
            label: account_label.clone(),
            scope: account_label.clone(),
            context: account_label.clone(),
            detail: account.email.clone(),
            avatar: avatar_initials(&account_label),
            is_account: true,
            count: String::new(),
        });

        for label in STANDARD_ACCOUNT_FOLDERS {
            let expected_role = standard_folder_role(label);
            let role_folder = expected_role.and_then(|role| {
                folders.iter().find(|folder| {
                    folder.account_id == account.id && folder.role.as_deref() == Some(role)
                })
            });
            entries.push(MailboxEntry {
                account_id: account.id,
                folder_id: role_folder.map_or(-1, |folder| folder.id),
                parent_folder_id: -1,
                depth: 0,
                has_children: false,
                is_standard: true,
                label: label.to_owned(),
                scope: format!("{account_label} / {label}"),
                context: account_label.clone(),
                detail: String::new(),
                avatar: String::new(),
                is_account: false,
                count: String::new(),
            });
        }

        let mut custom_folders: Vec<&FolderInfo> = folders
            .iter()
            .filter(|folder| folder.account_id == account.id && !is_standard_folder(folder))
            .collect();
        custom_folders.sort_by_key(|folder| folder.display_name.to_lowercase());
        let custom_ids = custom_folders
            .iter()
            .map(|folder| (folder.imap_name.as_str(), folder.id))
            .collect::<std::collections::HashMap<_, _>>();
        let parent_ids = custom_folders
            .iter()
            .map(|folder| {
                let parent = folder
                    .is_jmap
                    .then_some(" / ")
                    .or(folder.delimiter.as_deref())
                    .filter(|delimiter| !delimiter.is_empty())
                    .and_then(|delimiter| folder.imap_name.rsplit_once(delimiter))
                    .and_then(|(parent, _)| custom_ids.get(parent).copied())
                    .unwrap_or(-1);
                (folder.id, parent)
            })
            .collect::<std::collections::HashMap<_, _>>();
        for folder in custom_folders {
            let label = folder_label(folder);
            let parent_folder_id = parent_ids.get(&folder.id).copied().unwrap_or(-1);
            let mut depth = 0usize;
            let mut parent = parent_folder_id;
            let mut visited = std::collections::HashSet::new();
            while parent >= 0 && visited.insert(parent) {
                depth += 1;
                parent = parent_ids.get(&parent).copied().unwrap_or(-1);
            }
            entries.push(MailboxEntry {
                account_id: account.id,
                folder_id: folder.id,
                parent_folder_id,
                depth,
                has_children: parent_ids.values().any(|parent| *parent == folder.id),
                is_standard: false,
                label: label.clone(),
                scope: format!("Folder:{}", folder.id),
                context: account_label.clone(),
                detail: String::new(),
                avatar: String::new(),
                is_account: false,
                count: String::new(),
            });
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
        if entry.is_account {
            continue;
        }
        let Some(counts) = badges
            .iter()
            .find(|counts| counts.account_id == entry.account_id)
        else {
            continue;
        };
        entry.count = mailbox_badge(&entry.label, counts);
    }
    entries
}

async fn count_threads(core: &Core, resolved: &ScopeResolution) -> Result<usize, String> {
    core.count_threads_filtered(
        resolved.view,
        resolved.split_id,
        resolved.account_id,
        resolved.label_id,
        resolved.folder_id,
    )
    .await
    .map_err(|error| error.to_string())
}

fn unified_mailbox_entries(badges: &[MailboxBadgeCounts]) -> Vec<MailboxEntry> {
    let total = MailboxBadgeCounts {
        account_id: 0,
        inbox: badges.iter().map(|counts| counts.inbox).sum(),
        starred: badges.iter().map(|counts| counts.starred).sum(),
        drafts: badges.iter().map(|counts| counts.drafts).sum(),
    };
    let mailboxes = [
        ("Starred", "Unified Starred"),
        ("Sent", "Unified Sent"),
        ("Archive", "Unified Archive"),
        ("Spam", "Unified Spam"),
        ("Trash", "Unified Trash"),
        ("Drafts", "Unified Drafts"),
    ];
    let mut entries = Vec::with_capacity(mailboxes.len());
    for (label, scope) in mailboxes {
        entries.push(MailboxEntry {
            account_id: 0,
            folder_id: -1,
            parent_folder_id: -1,
            depth: 0,
            has_children: false,
            is_standard: true,
            label: label.to_owned(),
            scope: scope.to_owned(),
            context: "Unified".to_owned(),
            detail: String::new(),
            avatar: String::new(),
            is_account: false,
            count: mailbox_badge(label, &total),
        });
    }
    entries
}

fn mailbox_badge(label: &str, counts: &MailboxBadgeCounts) -> String {
    let count = match label {
        "Inbox" => counts.inbox,
        "Starred" => counts.starred,
        "Drafts" => counts.drafts,
        _ => return String::new(),
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

fn resolve_scope(
    scope: &str,
    accounts: &[Account],
    folders: &[FolderInfo],
    labels: &[Label],
) -> ScopeResolution {
    if scope == "Unified Inbox" {
        return ScopeResolution {
            account_id: None,
            folder_id: None,
            split_id: None,
            label_id: None,
            view: View::Inbox,
            title: "Inbox".to_owned(),
        };
    }
    if let Some(view) = match scope {
        "Unified Starred" => Some(View::Starred),
        "Unified Sent" => Some(View::Sent),
        "Unified Archive" => Some(View::Done),
        "Unified Spam" => Some(View::Spam),
        "Unified Trash" => Some(View::Trash),
        "Unified Drafts" => Some(View::Drafts),
        _ => None,
    } {
        return ScopeResolution {
            account_id: None,
            folder_id: None,
            split_id: None,
            label_id: None,
            view,
            title: scope.trim_start_matches("Unified ").to_owned(),
        };
    }

    if matches!(scope, "Important" | "Other") {
        return ScopeResolution {
            account_id: None,
            folder_id: None,
            split_id: Some(if scope == "Important" { -1 } else { -2 }),
            label_id: None,
            view: View::Inbox,
            title: scope.to_owned(),
        };
    }

    if let Some(label_id) = scope
        .strip_prefix("Label:")
        .and_then(|id| id.parse::<i64>().ok())
        && let Some(label) = labels.iter().find(|label| label.id == label_id)
    {
        return ScopeResolution {
            account_id: None,
            folder_id: None,
            split_id: None,
            label_id: Some(label_id),
            view: if label.is_auto {
                View::Inbox
            } else {
                View::All
            },
            title: label.name.clone(),
        };
    }

    if let Some(folder_id) = scope
        .strip_prefix("Folder:")
        .and_then(|id| id.parse::<i64>().ok())
        && let Some(folder) = folders.iter().find(|folder| folder.id == folder_id)
    {
        return ScopeResolution {
            account_id: Some(folder.account_id),
            folder_id: Some(folder.id),
            split_id: None,
            label_id: None,
            view: view_for_folder(folder),
            title: folder_label(folder),
        };
    }

    if let Some((account_name, folder_name)) = scope.split_once(" / ")
        && let Some(account) = accounts
            .iter()
            .find(|account| account_label(account) == account_name)
    {
        if let Some(view) = standard_account_view(folder_name) {
            return ScopeResolution {
                account_id: Some(account.id),
                folder_id: None,
                split_id: None,
                label_id: None,
                view,
                title: folder_name.to_owned(),
            };
        }
        if let Some(folder) = folders
            .iter()
            .find(|folder| folder.account_id == account.id && folder_label(folder) == folder_name)
        {
            return ScopeResolution {
                account_id: Some(account.id),
                folder_id: Some(folder.id),
                split_id: None,
                label_id: None,
                view: view_for_folder(folder),
                title: folder_name.to_owned(),
            };
        }
    }

    let account_id = accounts
        .iter()
        .find(|account| account_label(account) == scope)
        .map(|account| account.id);
    ScopeResolution {
        account_id,
        folder_id: None,
        split_id: None,
        label_id: None,
        view: View::Inbox,
        title: "Inbox".to_owned(),
    }
}

/// A warm-start scope is presentation data and can outlive an account rename
/// or removal. Accept only scopes represented by the current local database;
/// otherwise fall back to the unified inbox before querying its first page.
fn validated_startup_scope(
    preferred_scope: &str,
    accounts: &[Account],
    folders: &[FolderInfo],
    labels: &[Label],
) -> String {
    const UNIFIED_SCOPES: [&str; 7] = [
        "Unified Inbox",
        "Unified Starred",
        "Unified Sent",
        "Unified Archive",
        "Unified Spam",
        "Unified Trash",
        "Unified Drafts",
    ];
    if UNIFIED_SCOPES.contains(&preferred_scope)
        || matches!(preferred_scope, "Important" | "Other")
        || preferred_scope
            .strip_prefix("Label:")
            .and_then(|id| id.parse::<i64>().ok())
            .is_some_and(|id| labels.iter().any(|label| label.id == id))
        || mailbox_entries(accounts, folders)
            .iter()
            .any(|mailbox| mailbox.scope == preferred_scope)
    {
        preferred_scope.to_owned()
    } else {
        "Unified Inbox".to_owned()
    }
}

fn summary_to_message(
    thread: ThreadSummary,
    accounts: &[Account],
    folder: &str,
) -> Option<MailMessage> {
    let id = i32::try_from(thread.id).ok()?;
    let participant = thread.participants.first();
    let address = participant
        .map(|person| person.email.clone())
        .unwrap_or_default();
    let sender = participant
        .and_then(|person| person.name.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| address.clone());
    let account = accounts
        .iter()
        .find(|account| account.id == thread.account_id)
        .map(account_label)
        .unwrap_or_else(|| thread.account_email.clone());
    let domain = domain_from_address(&address).unwrap_or_default();

    Some(MailMessage {
        id,
        thread_id: Some(thread.id),
        account_id: thread.account_id,
        account,
        folder: folder.to_owned(),
        sender: sender.clone(),
        address,
        domain,
        initials: initials(&sender),
        subject: thread.subject,
        preview: thread.snippet,
        time: relative_time(thread.last_message_at),
        to: String::new(),
        label: if thread.unread_count > 0 {
            "UNREAD".to_owned()
        } else {
            "MAIL".to_owned()
        },
        unread: thread.unread_count > 0,
        starred: thread.is_starred,
        has_attachments: thread.has_attachments,
        attachments: Vec::new(),
        has_replied: thread.has_replied,
        labels: thread.labels,
        html: None,
        text: None,
        body_pending: true,
        sender_verification: String::new(),
    })
}

fn detail_to_message(row: &MailMessage, message: &MessageDetail) -> MailMessage {
    let sender = message
        .from
        .name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| message.from.email.clone());
    let html = Some(readable_message_html(
        message.html_body.as_deref(),
        message.text_body.as_deref(),
        &message.snippet,
    ));

    MailMessage {
        id: row.id,
        thread_id: row.thread_id,
        account_id: row.account_id,
        account: row.account.clone(),
        folder: row.folder.clone(),
        sender: sender.clone(),
        address: message.from.email.clone(),
        domain: domain_from_address(&message.from.email).unwrap_or_default(),
        initials: initials(&sender),
        subject: message.subject.clone(),
        preview: message.snippet.clone(),
        time: relative_time(message.date),
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
        attachments: message.attachments.clone(),
        has_replied: row.has_replied,
        labels: row.labels.clone(),
        html,
        text: message.text_body.clone(),
        body_pending: message.body_state != "cached",
        sender_verification: message.sender_verification.as_str().to_owned(),
    }
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

fn relative_time(timestamp_ms: i64) -> String {
    relative_time_at(timestamp_ms, Local::now())
}

fn relative_time_at(timestamp_ms: i64, now: DateTime<Local>) -> String {
    let Some(date) = DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .map(|timestamp| timestamp.with_timezone(&Local))
    else {
        return "Unknown date".to_owned();
    };
    let days = now
        .date_naive()
        .signed_duration_since(date.date_naive())
        .num_days()
        .max(0);
    match days {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        _ if date.year() == now.year() => date.format("%b %-d").to_string(),
        _ => date.format("%b %-d, %Y").to_string(),
    }
}

fn text_to_html(text: &str) -> String {
    format!(
        "<html><body style=\"font-family:Arial,sans-serif;padding:32px;line-height:1.6;color:#303348\"><p>{}</p></body></html>",
        escape_html(text)
    )
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

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
pub fn fixture_messages() -> Vec<MailMessage> {
    fixtures()
        .into_iter()
        .map(MailMessage::from_fixture)
        .collect()
}

#[cfg(test)]
pub fn fixtures() -> Vec<EmailFixture> {
    let mut messages = vec![
        EmailFixture {
            id: 1,
            account: "Northstar Work",
            folder: "Inbox",
            sender: "Maya Chen",
            address: "maya@northstar.design",
            initials: "MC",
            subject: "A calmer way to plan the week",
            preview: "The latest direction for the product launch…",
            time: "9:41 AM",
            to: "alex@flectar.example",
            label: "IMPORTANT",
            unread: true,
            html: r##"<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <style>
    body { margin: 0; background: #f5f6fb; color: #282b40; font-family: Arial, sans-serif; }
    .card { margin: 26px; padding: 30px; background: #ffffff; border-radius: 18px; }
    .eyebrow { color: #6a60dc; font-size: 12px; font-weight: bold; letter-spacing: 1px; }
    h1 { margin: 14px 0 12px; color: #25283a; font-size: 28px; }
    p { font-size: 15px; line-height: 1.6; }
    .callout { margin: 24px 0; padding: 18px; background: #f0efff; border-left: 4px solid #6a60dc; border-radius: 8px; }
    .button { display: inline-block; margin-top: 8px; padding: 12px 18px; background: #5b52d6; color: #ffffff; border-radius: 8px; font-weight: bold; }
    .muted { color: #898da0; font-size: 12px; }
  </style>
</head>
<body>
  <div class="card">
    <div class="eyebrow">NORTHSTAR DESIGN · WEEKLY NOTE</div>
    <h1>A calmer way to plan the week</h1>
    <p>Hi Alex,</p>
    <p>We turned the launch plan into three small moments: see what matters, make one decision, and leave the rest for later. The new flow feels much closer to how people actually work.</p>
    <div class="callout"><b>Our focus this week</b><br>Ship the quiet version first. Let the interface make space for the work.</div>
    <p>If this direction feels right, I’ll package the prototype for the team tomorrow.</p>
    <span class="button">Open the launch plan</span>
    <p class="muted">Maya · Northstar Design · This message was rendered natively.</p>
  </div>
</body>
</html>"##,
        },
        EmailFixture {
            id: 2,
            account: "Northstar Work",
            folder: "Inbox",
            sender: "Jon Bell",
            address: "jon@fieldnotes.fm",
            initials: "JB",
            subject: "Three links for the native UI rabbit hole",
            preview: "Slint, Blitz, and a little time to experiment…",
            time: "Yesterday",
            to: "alex@flectar.example",
            label: "PROJECTS",
            unread: true,
            html: r##"<!doctype html>
<html><head><style>
  body { margin:0; background:#fffdf8; color:#33302c; font-family: Georgia, serif; }
  .note { margin:30px; padding:28px; border:1px solid #e8decf; border-radius:10px; }
  h1 { font-size:25px; margin:0 0 18px; }
  p, li { font-size:15px; line-height:1.55; }
  a { color:#ba5f32; font-weight:bold; }
  .quote { padding:16px 20px; margin:22px 0; background:#fff2e7; border-radius:8px; }
</style></head><body><div class="note">
  <h1>The native UI rabbit hole</h1>
  <p>Hey Alex — this feels like the right kind of weird project.</p>
  <div class="quote">“A mail client is the perfect stress test for a renderer: messy HTML, dense lists, and a lot of tiny interactions.”</div>
  <ol><li><a href="https://slint.dev">Slint</a> for the application shell.</li><li><a href="https://github.com/DioxusLabs/blitz">Blitz</a> for HTML and CSS email bodies.</li><li>A tiny adapter so the two stay pleasantly decoupled.</li></ol>
  <p>Let’s keep the first cut intentionally small: fixture inbox, selectable messages, and a real rendered body.</p>
  <p>— Jon</p>
</div></body></html>"##,
        },
        EmailFixture {
            id: 3,
            account: "Flectar",
            folder: "Inbox",
            sender: "Flectar Mail Updates",
            address: "updates@flectar.example",
            initials: "CO",
            subject: "Your weekly mailbox digest",
            preview: "14 conversations, 3 follow-ups, 1 quiet afternoon…",
            time: "Mon",
            to: "alex@flectar.example",
            label: "DIGEST",
            unread: false,
            html: r##"<!doctype html>
<html><head><style>
  body { margin:0; background:#f4fbfa; color:#263c3b; font-family:Arial,sans-serif; }
  .wrap { margin:24px; padding:26px; background:#fff; border-radius:16px; }
  .badge { color:#138d82; font-size:12px; font-weight:bold; }
  h1 { font-size:26px; margin:10px 0 20px; }
  .grid { display:flex; gap:10px; margin:20px 0; }
  .stat { flex:1; padding:16px; background:#e9f8f5; border-radius:10px; }
  .number { font-size:25px; font-weight:bold; color:#138d82; }
  .caption { color:#708885; font-size:12px; }
  p { font-size:14px; line-height:1.6; }
</style></head><body><div class="wrap">
  <div class="badge">FLECTAR MAIL / WEEKLY DIGEST</div><h1>A little room to breathe</h1>
  <div class="grid"><div class="stat"><div class="number">14</div><div class="caption">conversations</div></div><div class="stat"><div class="number">3</div><div class="caption">follow-ups</div></div><div class="stat"><div class="number">1</div><div class="caption">quiet afternoon</div></div></div>
  <p>The mailbox is in good shape. Two threads are waiting on someone else, and the rest can wait until tomorrow.</p>
  <p>Keep the momentum gentle,<br><b>Flectar Mail</b></p>
</div></body></html>"##,
        },
        EmailFixture {
            id: 4,
            account: "Northstar Work",
            folder: "Sent",
            sender: "Ravi Patel",
            address: "ravi@orbit.tools",
            initials: "RP",
            subject: "Re: the renderer boundary",
            preview: "I think the split between chrome and content is right…",
            time: "Sun",
            to: "alex@flectar.example",
            label: "REPLY",
            unread: false,
            html: r##"<!doctype html>
<html><head><style>
  body { margin:0; background:#f8f8f8; color:#303238; font-family:Arial,sans-serif; }
  .message { margin:30px; padding:26px; background:#fff; border:1px solid #e1e1e1; border-radius:6px; }
  h2 { margin:0 0 16px; font-size:21px; }
  p { font-size:14px; line-height:1.6; }
  code { padding:3px 6px; background:#f0f0f0; border-radius:4px; font-family:monospace; }
  .signature { margin-top:24px; padding-top:16px; border-top:1px solid #e7e7e7; color:#777; font-size:12px; }
</style></head><body><div class="message">
  <h2>The renderer boundary</h2>
  <p>Exactly. Slint should know about folders, messages, and actions. Blitz should receive a sanitized HTML document and return pixels. The hand-off can stay as small as <code>render(html) -&gt; Image</code>.</p>
  <p>That keeps rich HTML isolated inside the native reading pane.</p>
  <p>Good first milestone.</p><div class="signature">Ravi Patel<br>Orbit Tools</div>
</div></body></html>"##,
        },
    ];

    messages.extend([
        EmailFixture {
            id: 5,
            account: "Personal",
            folder: "Inbox",
            sender: "Lena Ortiz",
            address: "lena@papertrail.art",
            initials: "LO",
            subject: "Photos from the coast",
            preview: "A few favorites from the weekend are attached…",
            time: "Sat",
            to: "alex@flectar.example",
            label: "PERSONAL",
            unread: true,
            html: messages[1].html,
        },
        EmailFixture {
            id: 6,
            account: "Northstar Work",
            folder: "Inbox",
            sender: "Calendar Bot",
            address: "calendar@northstar.design",
            initials: "CB",
            subject: "Tomorrow's launch review",
            preview: "The launch review starts at 10:00 AM tomorrow…",
            time: "Fri",
            to: "alex@flectar.example",
            label: "CALENDAR",
            unread: false,
            html: messages[0].html,
        },
        EmailFixture {
            id: 7,
            account: "Flectar",
            folder: "Inbox",
            sender: "Flectar Mail Updates",
            address: "updates@flectar.example",
            initials: "FM",
            subject: "Your Flectar account is ready",
            preview: "Your inbox is connected and ready for the next step…",
            time: "Thu",
            to: "alex@flectar.example",
            label: "ACCOUNT",
            unread: true,
            html: messages[2].html,
        },
        EmailFixture {
            id: 8,
            account: "Northstar Work",
            folder: "Archive",
            sender: "Maya Chen",
            address: "maya@northstar.design",
            initials: "MC",
            subject: "Re: A calmer way to plan the week",
            preview: "The revised plan is in the shared folder…",
            time: "Thu",
            to: "alex@flectar.example",
            label: "ARCHIVE",
            unread: false,
            html: messages[0].html,
        },
        EmailFixture {
            id: 9,
            account: "Personal",
            folder: "Sent",
            sender: "Lena Ortiz",
            address: "lena@papertrail.art",
            initials: "LO",
            subject: "Re: Photos from the coast",
            preview: "These are beautiful — thank you for sending them…",
            time: "Wed",
            to: "lena@papertrail.art",
            label: "SENT",
            unread: false,
            html: messages[1].html,
        },
        EmailFixture {
            id: 10,
            account: "Flectar",
            folder: "Archive",
            sender: "Ravi Patel",
            address: "ravi@orbit.tools",
            initials: "RP",
            subject: "Native renderer notes",
            preview: "The CPU boundary keeps the rest of the app small…",
            time: "Tue",
            to: "alex@flectar.example",
            label: "ARCHIVE",
            unread: false,
            html: messages[3].html,
        },
        EmailFixture {
            id: 11,
            account: "Northstar Work",
            folder: "Inbox",
            sender: "Jon Bell",
            address: "jon@fieldnotes.fm",
            initials: "JB",
            subject: "A small plan for next week",
            preview: "Three things we can finish without rushing them…",
            time: "Mon",
            to: "alex@flectar.example",
            label: "PROJECTS",
            unread: true,
            html: messages[1].html,
        },
        EmailFixture {
            id: 12,
            account: "Personal",
            folder: "Inbox",
            sender: "Mina Park",
            address: "mina@home.example",
            initials: "MP",
            subject: "Dinner next Thursday?",
            preview: "Would Thursday evening work for everyone?",
            time: "Sun",
            to: "alex@flectar.example",
            label: "PERSONAL",
            unread: true,
            html: messages[2].html,
        },
    ]);

    messages
}

#[cfg(test)]
mod tests {
    use super::{
        ComposeMessage, compose_args, mailbox_entries, markdown_to_html, markdown_to_plain_text,
        readable_message_html, relative_time_at, resolve_scope, validated_startup_scope,
    };
    use chrono::{Local, TimeZone};
    use flectar_mail_core::models::{
        Account, AuthKind, FolderInfo, Label, MailProtocol, Provider, View,
    };

    fn test_account() -> Account {
        Account {
            id: 1,
            email: "person@example.com".into(),
            display_name: Some("Person".into()),
            avatar_url: None,
            provider: Provider::Imap,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            sync_state: "idle".into(),
            sync_error: None,
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
    fn old_mail_uses_a_calendar_date_instead_of_elapsed_days() {
        let now = Local.with_ymd_and_hms(2026, 8, 23, 12, 0, 0).unwrap();
        let old = Local.with_ymd_and_hms(2021, 6, 15, 9, 0, 0).unwrap();

        assert_eq!(
            relative_time_at(old.timestamp_millis(), now),
            "Jun 15, 2021"
        );
    }

    #[test]
    fn recent_mail_keeps_friendly_labels() {
        let now = Local.with_ymd_and_hms(2026, 8, 23, 12, 0, 0).unwrap();
        let today = Local.with_ymd_and_hms(2026, 8, 23, 8, 0, 0).unwrap();
        let yesterday = Local.with_ymd_and_hms(2026, 8, 22, 23, 0, 0).unwrap();

        assert_eq!(relative_time_at(today.timestamp_millis(), now), "Today");
        assert_eq!(
            relative_time_at(yesterday.timestamp_millis(), now),
            "Yesterday"
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
                display_name: "☺ Projects".into(),
                is_jmap: false,
                imap_name: "&Jjo- Projects".into(),
                delimiter: Some("/".into()),
                role: None,
            },
            FolderInfo {
                id: 11,
                account_id: 1,
                display_name: "☺ Projects/2026".into(),
                is_jmap: false,
                imap_name: "&Jjo- Projects/2026".into(),
                delimiter: Some("/".into()),
                role: None,
            },
            FolderInfo {
                id: 12,
                account_id: 1,
                display_name: "Archive copy/2026".into(),
                is_jmap: false,
                imap_name: "Archive copy/2026".into(),
                delimiter: Some("/".into()),
                role: None,
            },
            FolderInfo {
                id: 13,
                account_id: 1,
                display_name: "Inbox".into(),
                is_jmap: false,
                imap_name: "Inbox".into(),
                delimiter: Some("/".into()),
                role: None,
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
        assert_eq!(child.scope, "Folder:11");
        assert_eq!(duplicate_leaf.scope, "Folder:12");
        assert_eq!(custom_inbox.scope, "Folder:13");
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
                is_auto: false,
            },
            Label {
                id: 32,
                name: "Newsletters".into(),
                color: "#a479e2".into(),
                keyword: "newsletters".into(),
                position: 1,
                is_auto: true,
            },
        ];

        let important = resolve_scope("Important", &[], &[], &labels);
        assert_eq!(important.view, View::Inbox);
        assert_eq!(important.split_id, Some(-1));

        let manual = resolve_scope("Label:31", &[], &[], &labels);
        assert_eq!(manual.view, View::All);
        assert_eq!(manual.label_id, Some(31));
        assert_eq!(manual.title, "Projects");

        let automatic = resolve_scope("Label:32", &[], &[], &labels);
        assert_eq!(automatic.view, View::Inbox);
        assert_eq!(automatic.label_id, Some(32));
        assert_eq!(automatic.title, "Newsletters");
        assert_eq!(
            validated_startup_scope("Label:32", &[], &[], &labels),
            "Label:32"
        );
        assert_eq!(
            validated_startup_scope("Label:999", &[], &[], &labels),
            "Unified Inbox"
        );
    }
}
