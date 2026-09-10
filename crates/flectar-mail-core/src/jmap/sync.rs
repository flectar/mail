//! Durable JMAP Mail synchronization and submission actor.

use crate::accounts::credentials::{self, Slot};
use crate::db::repo;
use crate::db::repo::messages::NewMessage;
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use crate::jmap::client::{self, ConnectedClient};
use crate::models::{AccountConfig, Address, MailProtocol, now_ms, roles};
use crate::sync::engine::{
    PriorityFetchCmd, SyncCmd, SyncCtx, configured_sync_interval, set_state, set_state_error,
};
use futures::StreamExt;
use jmap_client::core::error::MethodErrorType;
use jmap_client::core::response::{EmailGetResponse, MailboxGetResponse};
use jmap_client::core::set::{SetErrorType, SetObject};
use jmap_client::email::{Email, Property as EmailProperty};
use jmap_client::mailbox::Role;
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet};
use tokio::sync::{mpsc, watch};

const PAGE: usize = 500;
const MAX_QUERY_RESTARTS: usize = 3;
const MAX_CHANGE_PAGES: usize = 10_000;
const MAX_JMAP_MESSAGE_BYTES: usize = 128 * 1024 * 1024;

pub(crate) fn spawn(
    ctx: SyncCtx,
    config: AccountConfig,
    rx: mpsc::Receiver<SyncCmd>,
    body_rx: mpsc::Receiver<PriorityFetchCmd>,
    settings_rx: watch::Receiver<crate::models::AccountSettings>,
) -> Vec<tokio::task::JoinHandle<()>> {
    debug_assert_eq!(config.mail_protocol, MailProtocol::Jmap);
    vec![
        tokio::spawn(actor(ctx.clone(), config.clone(), rx, settings_rx)),
        tokio::spawn(body_actor(ctx, config, body_rx)),
    ]
}

async fn connect(ctx: &SyncCtx, config: &AccountConfig) -> Result<ConnectedClient> {
    let secret =
        credentials::load_async(ctx.credentials.clone(), config.id, Slot::Password).await?;
    client::connect(config, &secret).await
}

async fn actor(
    ctx: SyncCtx,
    mut config: AccountConfig,
    mut rx: mpsc::Receiver<SyncCmd>,
    mut settings: watch::Receiver<crate::models::AccountSettings>,
) {
    let mut waiters = Vec::new();
    let mut retry = 1u64;
    loop {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                SyncCmd::Shutdown => return,
                SyncCmd::SyncNow {
                    complete: Some(done),
                } => waiters.push(done),
                _ => {}
            }
        }
        if settings.has_changed().unwrap_or(false) {
            config.settings = settings.borrow_and_update().clone();
        }
        let outcome = async {
            let connected = connect(&ctx, &config).await?;
            persist_session(&ctx, config.id, &connected).await?;
            set_state(&ctx, config.id, "syncing").await;
            sync_mailboxes(&ctx, config.id, &connected).await?;
            sync_emails(&ctx, &config, &connected).await?;
            execute_actions(&ctx, &config, &connected).await?;
            Ok::<ConnectedClient, CoreError>(connected)
        }
        .await;
        let connected = match outcome {
            Ok(connected) => {
                retry = 1;
                set_state(&ctx, config.id, "idle").await;
                ctx.bus.emit(CoreEvent::NetworkState { online: true });
                finish_waiters(&mut waiters, Ok(()));
                Some(connected)
            }
            Err(error @ (CoreError::Auth(_) | CoreError::NeedsReauth)) => {
                set_state(&ctx, config.id, "needs_reauth").await;
                finish_waiters(&mut waiters, Err(error.to_string()));
                None
            }
            Err(error) => {
                let message = error.to_string();
                set_state_error(&ctx, config.id, "offline", &message).await;
                ctx.bus.emit(CoreEvent::NetworkState { online: false });
                finish_waiters(&mut waiters, Err(message));
                retry = (retry * 2).min(300);
                None
            }
        };
        let normal = configured_sync_interval(&ctx.db).await;
        let delay = if retry == 1 {
            normal
        } else {
            std::time::Duration::from_secs(retry)
        };
        if let Some(connected) = connected {
            // RFC 8620 EventSource gives Stalwart an immediate new-mail path;
            // the selected periodic interval remains the correctness backstop.
            match connected
                .client
                .event_source(
                    Some([
                        jmap_client::DataType::Email,
                        jmap_client::DataType::Mailbox,
                        jmap_client::DataType::EmailSubmission,
                    ]),
                    true,
                    Some(60),
                    None,
                )
                .await
            {
                Ok(mut stream) => {
                    tokio::select! {
                        command = rx.recv() => match command {
                            None | Some(SyncCmd::Shutdown) => return,
                            Some(SyncCmd::SyncNow { complete: Some(done) }) => waiters.push(done),
                            _ => {}
                        },
                        _ = settings.changed() => {},
                        _ = tokio::time::sleep(delay) => {},
                        notification = stream.next() => {
                            if let Some(Err(error)) = notification {
                                tracing::debug!(account_id=config.id, error=%error, "JMAP push stream ended; polling fallback will continue");
                            }
                        },
                    }
                    continue;
                }
                Err(error) => {
                    tracing::debug!(account_id=config.id, error=%error, "JMAP push unavailable; using polling")
                }
            }
        }
        match tokio::time::timeout(delay, rx.recv()).await {
            Ok(None) | Ok(Some(SyncCmd::Shutdown)) => return,
            Ok(Some(SyncCmd::SyncNow {
                complete: Some(done),
            })) => waiters.push(done),
            _ => {}
        }
    }
}

fn finish_waiters(
    waiters: &mut Vec<tokio::sync::oneshot::Sender<std::result::Result<(), String>>>,
    result: std::result::Result<(), String>,
) {
    for waiter in waiters.drain(..) {
        let _ = waiter.send(result.clone());
    }
}

async fn persist_session(ctx: &SyncCtx, id: i64, connected: &ConnectedClient) -> Result<()> {
    let url = connected.base_url.clone();
    let remote = connected.account_id.clone();
    ctx.db
        .write(move |conn| {
            conn.execute(
                "UPDATE accounts SET jmap_url=?2, jmap_account_id=?3 WHERE id=?1",
                params![id, url, remote],
            )?;
            Ok(())
        })
        .await
}

fn role(value: Role) -> Option<&'static str> {
    match value {
        Role::Inbox => Some(roles::INBOX),
        Role::Archive => Some(roles::ARCHIVE),
        Role::Drafts => Some(roles::DRAFTS),
        Role::Sent => Some(roles::SENT),
        Role::Trash => Some(roles::TRASH),
        Role::Junk => Some(roles::SPAM),
        _ => None,
    }
}

async fn sync_mailboxes(ctx: &SyncCtx, local_account: i64, c: &ConnectedClient) -> Result<()> {
    let mut request = c.client.build();
    request.get_mailbox().account_id(&c.account_id);
    let mut response: MailboxGetResponse = request
        .send_get_mailbox()
        .await
        .map_err(client::map_error)?;
    let state = response.take_state();
    let mailboxes = response.take_list();
    let hierarchy = mailboxes
        .iter()
        .map(|mailbox| {
            Ok((
                mailbox
                    .id()
                    .ok_or_else(|| {
                        CoreError::Jmap("Mailbox/get returned an object without id".into())
                    })?
                    .to_owned(),
                (
                    mailbox
                        .name()
                        .ok_or_else(|| {
                            CoreError::Jmap("Mailbox/get returned an object without name".into())
                        })?
                        .to_owned(),
                    mailbox.parent_id().map(str::to_owned),
                ),
            ))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    for (id, (_, parent)) in &hierarchy {
        if parent
            .as_ref()
            .is_some_and(|parent| !hierarchy.contains_key(parent))
        {
            return Err(CoreError::Jmap(format!(
                "Mailbox {id} references a missing parent"
            )));
        }
    }
    let mut standard_roles = HashMap::new();
    for mailbox in &mailboxes {
        if let Some(local_role) = role(mailbox.role()) {
            let mailbox_id = mailbox.id().ok_or_else(|| {
                CoreError::Jmap("Mailbox/get returned an object without id".into())
            })?;
            if let Some(previous) = standard_roles.insert(local_role, mailbox_id) {
                return Err(CoreError::Jmap(format!(
                    "Mailboxes {previous} and {mailbox_id} both advertise the {local_role} role"
                )));
            }
        }
    }
    let seen = hierarchy.keys().cloned().collect::<HashSet<_>>();
    ctx.db
        .write(move |conn| {
            let tx = conn.transaction()?;
            for mailbox in mailboxes {
                let id = mailbox.id().ok_or_else(|| {
                    CoreError::Jmap("Mailbox/get returned an object without id".into())
                })?;
                let display_name = mailbox_path(id, &hierarchy)?;
                repo::folders::upsert_jmap(
                    &tx,
                    local_account,
                    id,
                    &display_name,
                    role(mailbox.role()),
                )?;
            }
            let mut stmt = tx.prepare(
                "SELECT id,jmap_id FROM folders WHERE account_id=?1 AND jmap_id IS NOT NULL",
            )?;
            let current = stmt
                .query_map(params![local_account], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            for (id, remote) in current {
                if !seen.contains(&remote) {
                    tx.execute("UPDATE folders SET jmap_id=NULL WHERE id=?1", params![id])?;
                }
            }
            tx.execute(
                "INSERT INTO jmap_sync_state(account_id,mailbox_state) VALUES(?1,?2)
                 ON CONFLICT(account_id) DO UPDATE SET mailbox_state=excluded.mailbox_state",
                params![local_account, state],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
}

fn mailbox_path(id: &str, hierarchy: &HashMap<String, (String, Option<String>)>) -> Result<String> {
    let mut parts = Vec::new();
    let mut current = Some(id);
    let mut visited = HashSet::new();
    while let Some(id) = current {
        if !visited.insert(id.to_owned()) {
            return Err(CoreError::Jmap(format!(
                "Mailbox hierarchy contains a cycle at {id}"
            )));
        }
        let Some((name, parent)) = hierarchy.get(id) else {
            break;
        };
        parts.push(name.clone());
        current = parent.as_deref();
    }
    parts.reverse();
    if parts.is_empty() {
        Ok("Mailbox".into())
    } else {
        Ok(parts.join(" / "))
    }
}

async fn saved_email_state(ctx: &SyncCtx, id: i64) -> Result<Option<String>> {
    ctx.db
        .read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT email_state FROM jmap_sync_state WHERE account_id=?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()?
                .flatten())
        })
        .await
}

async fn query_ids(config: &AccountConfig, c: &ConnectedClient) -> Result<Vec<String>> {
    let cutoff = config
        .settings
        .mail_history
        .cutoff_ms_at(now_ms())
        .map(|v| v / 1000);
    'restart: for _ in 0..MAX_QUERY_RESTARTS {
        let mut ids = Vec::new();
        let mut query_state: Option<String> = None;
        let mut expected_total: Option<usize> = None;
        loop {
            let position = i32::try_from(ids.len())
                .map_err(|_| CoreError::Jmap("Email/query result set is too large".into()))?;
            let mut request = c.client.build();
            let query = request
                .query_email()
                .account_id(&c.account_id)
                .position(position)
                .limit(PAGE)
                .calculate_total(true)
                .sort([jmap_client::email::query::Comparator::received_at().descending()]);
            if let Some(cutoff) = cutoff {
                query.filter(jmap_client::email::query::Filter::after(cutoff));
            }
            let mut response = request
                .send_query_email()
                .await
                .map_err(client::map_error)?;
            let response_state = response.query_state().to_owned();
            let total = response
                .total()
                .ok_or_else(|| CoreError::Jmap("Email/query omitted its requested total".into()))?;
            if response.position() != ids.len() as i32
                || query_state
                    .as_ref()
                    .is_some_and(|state| state != &response_state)
                || expected_total.is_some_and(|expected| expected != total)
            {
                continue 'restart;
            }
            query_state.get_or_insert(response_state);
            expected_total.get_or_insert(total);
            let page = response.take_ids();
            if page.is_empty() && ids.len() < total {
                continue 'restart;
            }
            ids.extend(page);
            if ids.len() == total {
                let unique = ids.iter().collect::<HashSet<_>>();
                if unique.len() != ids.len() {
                    continue 'restart;
                }
                return Ok(ids);
            }
            if ids.len() > total {
                continue 'restart;
            }
        }
    }
    Err(CoreError::Jmap(
        "mailbox changed repeatedly while paging Email/query; retrying later".into(),
    ))
}

async fn changes(
    c: &ConnectedClient,
    mut state: String,
) -> Result<(Vec<String>, Vec<String>, String)> {
    let mut ids = Vec::new();
    let mut destroyed = Vec::new();
    for _ in 0..MAX_CHANGE_PAGES {
        let previous_state = state.clone();
        let mut response = match c.client.email_changes(state.clone(), Some(PAGE)).await {
            Ok(response) => response,
            Err(jmap_client::Error::Method(method))
                if method.p_type == MethodErrorType::CannotCalculateChanges =>
            {
                return Err(CoreError::Jmap("cannotCalculateChanges".into()));
            }
            Err(error) => return Err(client::map_error(error)),
        };
        ids.extend(response.take_created());
        ids.extend(response.take_updated());
        destroyed.extend(response.take_destroyed());
        let more = response.has_more_changes();
        state = response.new_state().to_owned();
        if !more {
            ids.sort();
            ids.dedup();
            destroyed.sort();
            destroyed.dedup();
            return Ok((ids, destroyed, state));
        }
        if state == previous_state {
            return Err(CoreError::Jmap(
                "Email/changes did not advance its state while reporting more changes".into(),
            ));
        }
    }
    Err(CoreError::Jmap(
        "Email/changes exceeded the pagination safety limit".into(),
    ))
}

async fn current_email_state(c: &ConnectedClient) -> Result<String> {
    let mut request = c.client.build();
    request
        .get_email()
        .account_id(&c.account_id)
        .ids(Vec::<String>::new())
        .properties([EmailProperty::Id]);
    let mut response: EmailGetResponse =
        request.send_get_email().await.map_err(client::map_error)?;
    Ok(response.take_state())
}

fn header_properties() -> Vec<EmailProperty> {
    vec![
        EmailProperty::Id,
        EmailProperty::BlobId,
        EmailProperty::ThreadId,
        EmailProperty::MailboxIds,
        EmailProperty::Keywords,
        EmailProperty::Size,
        EmailProperty::ReceivedAt,
        EmailProperty::MessageId,
        EmailProperty::InReplyTo,
        EmailProperty::References,
        EmailProperty::From,
        EmailProperty::To,
        EmailProperty::Cc,
        EmailProperty::Bcc,
        EmailProperty::Subject,
        EmailProperty::SentAt,
        EmailProperty::HasAttachment,
        EmailProperty::Attachments,
        EmailProperty::Preview,
    ]
}

async fn sync_emails(ctx: &SyncCtx, config: &AccountConfig, c: &ConnectedClient) -> Result<()> {
    let previous = saved_email_state(ctx, config.id).await?;
    let (ids, mut destroyed, checkpoint_state, full) = match previous {
        Some(state) => match changes(c, state).await {
            Ok((ids, destroyed, state)) => (ids, destroyed, state, false),
            Err(CoreError::Jmap(value)) if value == "cannotCalculateChanges" => {
                let state = current_email_state(c).await?;
                (query_ids(config, c).await?, Vec::new(), state, true)
            }
            Err(error) => return Err(error),
        },
        None => {
            let state = current_email_state(c).await?;
            (query_ids(config, c).await?, Vec::new(), state, true)
        }
    };
    let max = c
        .client
        .session()
        .core_capabilities()
        .map(|v| v.max_objects_in_get().clamp(1, 1000))
        .unwrap_or(256);
    let mut emails = Vec::new();
    for chunk in ids.chunks(max) {
        let mut request = c.client.build();
        request
            .get_email()
            .account_id(&c.account_id)
            .ids(chunk.iter().cloned())
            .properties(header_properties());
        let mut response: EmailGetResponse =
            request.send_get_email().await.map_err(client::map_error)?;
        destroyed.extend(response.take_not_found());
        emails.extend(response.take_list());
    }
    destroyed.sort();
    destroyed.dedup();
    persist_emails(ctx, config, emails, destroyed, checkpoint_state, full).await
}

fn addresses(values: Option<&[jmap_client::email::EmailAddress]>) -> Vec<Address> {
    values
        .unwrap_or_default()
        .iter()
        .map(|value| Address {
            name: value.name().map(str::to_owned),
            email: value.email().to_owned(),
        })
        .collect()
}

fn primary(folders: &[repo::folders::Folder]) -> Option<&repo::folders::Folder> {
    folders.iter().min_by_key(|f| match f.role.as_deref() {
        Some(roles::INBOX) => 0,
        Some(roles::DRAFTS) => 1,
        Some(roles::SENT) => 2,
        Some(roles::ARCHIVE) => 3,
        Some(roles::TRASH) => 5,
        Some(roles::SPAM) => 6,
        _ => 4,
    })
}

fn first(values: Option<&[String]>) -> Option<String> {
    values.and_then(|values| values.first()).cloned()
}

async fn persist_emails(
    ctx: &SyncCtx,
    config: &AccountConfig,
    emails: Vec<Email>,
    destroyed: Vec<String>,
    state: String,
    full: bool,
) -> Result<()> {
    let account_id = config.id;
    let cutoff = config.settings.mail_history.cutoff_ms_at(now_ms());
    let remote_ids = emails
        .iter()
        .filter_map(|email| email.id().map(str::to_owned))
        .collect::<HashSet<_>>();
    let (thread_ids, stale_paths) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let mut changed_threads = HashSet::new();
            let mut stale_paths = Vec::new();

            for remote_id in destroyed {
                if let Some(row) = repo::messages::by_jmap_id(&tx, account_id, &remote_id)?
                    && !repo::actions::has_pending_remote_creation(&tx, row.id)?
                {
                    if let Some(thread_id) = row.thread_id {
                        changed_threads.insert(thread_id);
                    }
                    if let Some(path) = row.raw_path {
                        stale_paths.push(path);
                    }
                    repo::messages::delete(&tx, row.id)?;
                }
            }

            for email in emails {
                let remote_id = email
                    .id()
                    .map(str::to_owned)
                    .ok_or_else(|| CoreError::Jmap("Email/get returned an object without id".into()))?;
                let mut folders = Vec::new();
                for mailbox_id in email.mailbox_ids() {
                    let folder = repo::folders::by_jmap_id(&tx, account_id, mailbox_id)?
                        .ok_or_else(|| {
                            CoreError::Jmap(format!(
                                "Email {remote_id} references mailbox {mailbox_id} before it was synchronized"
                            ))
                        })?;
                    folders.push(folder);
                }
                let primary_folder = primary(&folders).ok_or_else(|| {
                    CoreError::Jmap(format!(
                        "Email {remote_id} does not belong to a synchronized mailbox"
                    ))
                })?;
                let folder_ids = folders.iter().map(|folder| folder.id).collect::<Vec<_>>();
                let message_id = first(email.message_id());
                let subject = email.subject().unwrap_or_default().to_owned();
                let date = email
                    .received_at()
                    .ok_or_else(|| {
                        CoreError::Jmap(format!(
                            "Email/get omitted receivedAt for Email {remote_id}"
                        ))
                    })?
                    .saturating_mul(1000);
                let size = i64::try_from(email.size()).map_err(|_| {
                    CoreError::Jmap(format!("Email {remote_id} size exceeds local limits"))
                })?;
                let existing = match repo::messages::by_jmap_id(&tx, account_id, &remote_id)? {
                    Some(row) => Some(row),
                    None => match message_id.as_deref() {
                        Some(message_id) => repo::messages::jmap_adoption_candidate(
                            &tx,
                            account_id,
                            message_id,
                            &subject,
                            date,
                            size,
                        )?,
                        None => None,
                    },
                };
                let remote_thread = email
                    .thread_id()
                    .ok_or_else(|| {
                        CoreError::Jmap(format!(
                            "Email/get omitted threadId for Email {remote_id}"
                        ))
                    })?
                    .to_owned();
                let thread_id = match repo::threads::by_jmap_id(&tx, account_id, &remote_thread)? {
                    Some(id) => id,
                    None => match existing.as_ref().and_then(|row| row.thread_id) {
                        Some(id) => {
                            tx.execute(
                                "UPDATE threads SET jmap_id=?2 WHERE id=?1 AND jmap_id IS NULL",
                                params![id, remote_thread],
                            )?;
                            id
                        }
                        None => repo::threads::create_jmap(
                            &tx,
                            account_id,
                            &remote_thread,
                            &crate::mime::normalize_subject(&subject),
                        )?,
                    },
                };
                let keywords = email
                    .keywords()
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                let is_read = keywords.iter().any(|value| value == "$seen");
                let is_starred = keywords.iter().any(|value| value == "$flagged");
                let is_draft = keywords.iter().any(|value| value == "$draft")
                    || primary_folder.role.as_deref() == Some(roles::DRAFTS);
                let is_outgoing = primary_folder.role.as_deref() == Some(roles::SENT);
                let references = email.references().unwrap_or_default().to_vec();
                let from = addresses(email.from()).into_iter().next();
                let blob_id = Some(
                    email
                        .blob_id()
                        .ok_or_else(|| {
                            CoreError::Jmap(format!(
                                "Email/get omitted blobId for Email {remote_id}"
                            ))
                        })?
                        .to_owned(),
                );
                if let Some(existing) = existing.as_ref()
                    && repo::actions::has_pending_remote_creation(&tx, existing.id)?
                {
                    // A queued autosave/send owns the entire local draft. The
                    // remote object may be the previous autosave; applying it
                    // here would overwrite the user's newest edits immediately
                    // before the queued action serializes them.
                    repo::messages::set_jmap_remote(
                        &tx,
                        existing.id,
                        &remote_id,
                        existing.jmap_blob_id.as_deref(),
                    )?;
                    if let Some(thread_id) = existing.thread_id {
                        changed_threads.insert(thread_id);
                    }
                    continue;
                }
                let mut pending_keywords = false;
                let mut pending_mailboxes = false;
                let local_id;

                if let Some(existing) = existing {
                    (pending_keywords, pending_mailboxes) =
                        repo::actions::jmap_reconciliation_guards(&tx, existing.id)?;
                    let old_thread_id = existing.thread_id;
                    if existing.jmap_blob_id != blob_id && let Some(path) = existing.raw_path {
                        stale_paths.push(path);
                    }
                    tx.execute(
                        "UPDATE messages SET thread_id=?2, folder_id=CASE WHEN ?20 THEN folder_id ELSE ?3 END,
                           message_id=?4, subject=?5, from_name=?6, from_addr=?7, to_json=?8,
                           cc_json=?9, bcc_json=?10, date=?11, internal_date=?11,
                           is_read=CASE WHEN ?21 THEN is_read ELSE ?12 END,
                           is_starred=CASE WHEN ?21 THEN is_starred ELSE ?13 END,
                           is_draft=?14, is_outgoing=?15, has_attachments=?16, size=?17,
                           snippet=?18, jmap_blob_id=?19,
                           body_state=CASE WHEN jmap_blob_id IS NOT ?19 THEN 'none' ELSE body_state END,
                           raw_path=CASE WHEN jmap_blob_id IS NOT ?19 THEN NULL ELSE raw_path END
                         WHERE id=?1",
                        params![
                            existing.id,
                            thread_id,
                            primary_folder.id,
                            message_id,
                            subject,
                            from.as_ref().and_then(|value| value.name.clone()),
                            from.as_ref().map(|value| value.email.clone()),
                            serde_json::to_string(&addresses(email.to()))?,
                            serde_json::to_string(&addresses(email.cc()))?,
                            serde_json::to_string(&addresses(email.bcc()))?,
                            date,
                            is_read as i64,
                            is_starred as i64,
                            is_draft as i64,
                            is_outgoing as i64,
                            email.has_attachment() as i64,
                            size,
                            email.preview().unwrap_or_default(),
                            blob_id,
                            pending_mailboxes as i64,
                            pending_keywords as i64,
                        ],
                    )?;
                    local_id = existing.id;
                    repo::messages::set_jmap_remote(
                        &tx,
                        local_id,
                        &remote_id,
                        blob_id.as_deref(),
                    )?;
                    if let Some(old_thread_id) = old_thread_id
                        && old_thread_id != thread_id
                    {
                        changed_threads.insert(old_thread_id);
                    }
                    tx.execute("DELETE FROM message_refs WHERE message_id=?1", params![local_id])?;
                    for reference in &references {
                        tx.execute(
                            "INSERT OR IGNORE INTO message_refs(message_id,ref_message_id) VALUES(?1,?2)",
                            params![local_id, reference],
                        )?;
                    }
                } else {
                    let new = NewMessage {
                        account_id,
                        folder_id: primary_folder.id,
                        uid: None,
                        message_id,
                        gm_msgid: None,
                        gm_thrid: None,
                        subject,
                        from,
                        to: addresses(email.to()),
                        cc: addresses(email.cc()),
                        bcc: addresses(email.bcc()),
                        date,
                        internal_date: Some(date),
                        is_read,
                        is_starred,
                        is_draft,
                        is_outgoing,
                        is_automated: false,
                        has_attachments: email.has_attachment(),
                        size: Some(size),
                        snippet: email.preview().unwrap_or_default().to_owned(),
                        references,
                        list_unsubscribe: None,
                        list_unsubscribe_post: None,
                        sender_addr: None,
                        sender_verification: crate::models::SenderVerification::None,
                    };
                    local_id = repo::messages::insert(&tx, &new, thread_id)?;
                    repo::messages::set_jmap_remote(
                        &tx,
                        local_id,
                        &remote_id,
                        blob_id.as_deref(),
                    )?;
                }
                // Populate Files without downloading the message or changing $seen.
                // Preserve MIME-derived rows once a raw body is cached; their part
                // identifiers belong to the MIME parser, not the JMAP server.
                let body_cached: bool=tx.query_row("SELECT body_state='cached' FROM messages WHERE id=?1",[local_id],|r|r.get(0))?;
                if !body_cached { persist_attachment_parts(&tx, local_id, email.attachments().unwrap_or_default())?; }
                if !pending_mailboxes {
                    repo::gmail::set_message_folders(&tx, local_id, &folder_ids)?;
                }
                if !pending_keywords {
                    repo::labels::reconcile_keywords(&tx, local_id, &keywords)?;
                }
                repo::search::index_message(&tx, local_id)?;
                changed_threads.insert(thread_id);
            }

            if full {
                let mut stmt = tx.prepare(
                    "SELECT id,thread_id,raw_path,jmap_id,date FROM messages
                     WHERE account_id=?1 AND jmap_id IS NOT NULL",
                )?;
                let rows = stmt
                    .query_map(params![account_id], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(stmt);
                for (id, thread_id, path, remote_id, date) in rows {
                    if !remote_ids.contains(&remote_id)
                        && cutoff.is_none_or(|cutoff| date >= cutoff)
                        && !repo::actions::has_pending_remote_creation(&tx, id)?
                    {
                        if let Some(thread_id) = thread_id {
                            changed_threads.insert(thread_id);
                        }
                        if let Some(path) = path {
                            stale_paths.push(path);
                        }
                        repo::messages::delete(&tx, id)?;
                    }
                }
            }
            for thread_id in &changed_threads {
                repo::threads::recompute(&tx, *thread_id)?;
            }
            tx.execute(
                "INSERT INTO jmap_sync_state(account_id,email_state,last_full_sync)
                 VALUES(?1,?2,CASE WHEN ?3 THEN ?4 ELSE NULL END)
                 ON CONFLICT(account_id) DO UPDATE SET email_state=excluded.email_state,
                   last_full_sync=CASE WHEN ?3 THEN ?4 ELSE jmap_sync_state.last_full_sync END",
                params![account_id, state, full as i64, now_ms()],
            )?;
            tx.commit()?;
            Ok((changed_threads.into_iter().collect::<Vec<_>>(), stale_paths))
        })
        .await?;
    for path in stale_paths {
        let _ = tokio::fs::remove_file(path).await;
    }
    if !thread_ids.is_empty() {
        ctx.bus.emit(CoreEvent::MailUpdated { thread_ids });
    }
    Ok(())
}

fn persist_attachment_parts(
    db: &rusqlite::Connection,
    message: i64,
    parts: &[jmap_client::email::EmailBodyPart],
) -> Result<()> {
    let mut retained = Vec::new();
    for part in parts {
        let Some(blob) = part.blob_id() else {
            continue;
        };
        let Some(part_id) = part.part_id() else {
            continue;
        };
        let existing: Option<i64> = db
            .query_row(
                "SELECT id FROM attachments WHERE message_id=?1 AND part_id=?2 AND jmap_blob_id=?3",
                params![message, part_id, blob],
                |r| r.get(0),
            )
            .optional()?;
        let id = if let Some(id) = existing {
            id
        } else {
            db.execute("INSERT INTO attachments(message_id,part_id,filename,mime_type,size,content_id,is_inline,jmap_blob_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![message,part_id,part.name(),part.content_type(),part.size() as i64,part.content_id(),part.content_disposition()==Some("inline"),blob])?;
            db.last_insert_rowid()
        };
        retained.push(id);
    }
    let old = {
        let mut q = db.prepare("SELECT id FROM attachments WHERE message_id=?1")?;
        q.query_map([message], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for id in old {
        if !retained.contains(&id) {
            db.execute("DELETE FROM attachments WHERE id=?1", [id])?;
        }
    }
    Ok(())
}

async fn execute_actions(ctx: &SyncCtx, config: &AccountConfig, c: &ConnectedClient) -> Result<()> {
    let account_id = config.id;
    let actions = ctx
        .db
        .read(move |conn| repo::actions::due(conn, account_id, now_ms(), 20))
        .await?;
    for action in actions {
        let action_id = action.id;
        if !ctx
            .db
            .write(move |conn| repo::actions::try_claim(conn, action_id))
            .await?
        {
            continue;
        }
        let outcome = execute_action(ctx, config, c, &action).await;
        match outcome {
            Ok(()) => {
                ctx.db
                    .write(move |conn| repo::actions::set_state(conn, action_id, "done", None))
                    .await?;
                ctx.bus.emit(CoreEvent::ActionState {
                    action_id,
                    state: "done".into(),
                    error: None,
                });
            }
            Err(error @ (CoreError::Auth(_) | CoreError::NeedsReauth)) => {
                let message = error.to_string();
                ctx.db
                    .write(move |conn| {
                        repo::actions::bump_attempt(conn, action_id, now_ms() + 60_000, &message)
                    })
                    .await?;
                return Err(error);
            }
            Err(error @ CoreError::SendUncertain(_)) => {
                let message = error.to_string();
                let saved = message.clone();
                ctx.db
                    .write(move |conn| {
                        repo::actions::set_state(conn, action_id, "failed", Some(&saved))
                    })
                    .await?;
                ctx.bus.emit(CoreEvent::ActionState {
                    action_id,
                    state: "failed".into(),
                    error: Some(message),
                });
            }
            Err(error) => {
                let message = error.to_string();
                let attempts = action.attempts + 1;
                if attempts >= 8 {
                    let saved = message.clone();
                    ctx.db
                        .write(move |conn| {
                            repo::actions::set_state(conn, action_id, "failed", Some(&saved))
                        })
                        .await?;
                    ctx.bus.emit(CoreEvent::ActionState {
                        action_id,
                        state: "failed".into(),
                        error: Some(message),
                    });
                } else {
                    let retry_at = now_ms() + (1_i64 << attempts.min(8)) * 1000 + action_id % 997;
                    let saved = message.clone();
                    ctx.db
                        .write(move |conn| {
                            repo::actions::bump_attempt(conn, action_id, retry_at, &saved)
                        })
                        .await?;
                    tracing::warn!(account_id, action_id, kind=%action.kind, error=%message, "JMAP action retry scheduled");
                }
            }
        }
    }
    Ok(())
}

enum RemoteMessage {
    Gone,
    Unlinked,
    Linked(String),
}

async fn remote_message(ctx: &SyncCtx, message_id: Option<i64>) -> Result<RemoteMessage> {
    let Some(message_id) = message_id else {
        return Ok(RemoteMessage::Gone);
    };
    ctx.db
        .read(move |conn| {
            Ok(match repo::messages::get_row(conn, message_id)? {
                None => RemoteMessage::Gone,
                Some(row) => match row.jmap_id {
                    Some(id) => RemoteMessage::Linked(id),
                    None => RemoteMessage::Unlinked,
                },
            })
        })
        .await
}

fn linked_or_retry(remote: &RemoteMessage) -> Result<Option<&str>> {
    match remote {
        RemoteMessage::Gone => Ok(None),
        RemoteMessage::Unlinked => Err(CoreError::Jmap(
            "message is waiting for its JMAP remote id".into(),
        )),
        RemoteMessage::Linked(id) => Ok(Some(id)),
    }
}

async fn execute_action(
    ctx: &SyncCtx,
    config: &AccountConfig,
    c: &ConnectedClient,
    action: &repo::actions::PendingAction,
) -> Result<()> {
    let remote_id = remote_message(ctx, action.message_id).await?;
    match action.kind.as_str() {
        "mark_read" | "mark_unread" | "star" | "unstar" => {
            let Some(remote_id) = linked_or_retry(&remote_id)? else {
                return Ok(());
            };
            let (keyword, set) = match action.kind.as_str() {
                "mark_read" => ("$seen", true),
                "mark_unread" => ("$seen", false),
                "star" => ("$flagged", true),
                _ => ("$flagged", false),
            };
            apply_message_mutation(
                ctx,
                action.message_id,
                c.client.email_set_keyword(remote_id, keyword, set).await,
            )
            .await
        }
        "add_label" | "remove_label" => {
            let Some(remote_id) = linked_or_retry(&remote_id)? else {
                return Ok(());
            };
            let Some(keyword) = action.payload["keyword"].as_str() else {
                return Err(CoreError::Jmap("label action has no JMAP keyword".into()));
            };
            apply_message_mutation(
                ctx,
                action.message_id,
                c.client
                    .email_set_keyword(remote_id, keyword, action.kind == "add_label")
                    .await,
            )
            .await
        }
        "archive" | "unarchive" | "trash" | "spam" | "not_spam" | "move" => {
            let Some(remote_id) = linked_or_retry(&remote_id)? else {
                return Ok(());
            };
            let Some(message_id) = action.message_id else {
                return Ok(());
            };
            let mailbox_ids = ctx
                .db
                .read(move |conn| {
                    let mut stmt = conn.prepare(
                        "SELECT f.jmap_id FROM message_folders mf
                         JOIN folders f ON f.id=mf.folder_id
                         WHERE mf.message_id=?1 AND f.jmap_id IS NOT NULL",
                    )?;
                    Ok(stmt
                        .query_map(params![message_id], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?)
                })
                .await?;
            if mailbox_ids.is_empty() {
                return Err(CoreError::Jmap("move has no JMAP target mailbox".into()));
            }
            apply_message_mutation(
                ctx,
                action.message_id,
                c.client.email_set_mailboxes(remote_id, mailbox_ids).await,
            )
            .await
        }
        "save_draft" => save_draft_action(ctx, config, c, action).await,
        "delete_draft" => {
            let remote_id = action.payload["jmapEmailId"]
                .as_str()
                .ok_or_else(|| CoreError::Jmap("draft deletion has no JMAP Email id".into()))?;
            destroy_email_if_present(c, remote_id).await
        }
        "send" => send_action(ctx, config, c, action).await,
        "snooze" | "unsnooze" => Ok(()),
        other => Err(CoreError::Jmap(format!(
            "unsupported queued JMAP action: {other}"
        ))),
    }
}

async fn apply_message_mutation<T>(
    ctx: &SyncCtx,
    message_id: Option<i64>,
    outcome: std::result::Result<T, jmap_client::Error>,
) -> Result<()> {
    match outcome {
        Ok(_) => Ok(()),
        Err(jmap_client::Error::Set(error)) if error.type_ == SetErrorType::NotFound => {
            let Some(message_id) = message_id else {
                return Ok(());
            };
            let (thread_id, raw_path) = ctx
                .db
                .write(move |conn| {
                    let tx = conn.transaction()?;
                    let row = repo::messages::get_row(&tx, message_id)?;
                    let thread_id = row.as_ref().and_then(|row| row.thread_id);
                    let raw_path = row.and_then(|row| row.raw_path);
                    repo::messages::delete(&tx, message_id)?;
                    if let Some(thread_id) = thread_id {
                        repo::threads::recompute(&tx, thread_id)?;
                    }
                    tx.commit()?;
                    Ok((thread_id, raw_path))
                })
                .await?;
            if let Some(path) = raw_path {
                let _ = tokio::fs::remove_file(path).await;
            }
            if let Some(thread_id) = thread_id {
                ctx.bus.emit(CoreEvent::MailUpdated {
                    thread_ids: vec![thread_id],
                });
            }
            Ok(())
        }
        Err(error) => Err(client::map_error(error)),
    }
}

async fn destroy_email_if_present(c: &ConnectedClient, remote_id: &str) -> Result<()> {
    match c.client.email_destroy(remote_id).await {
        Ok(()) => Ok(()),
        Err(jmap_client::Error::Set(error)) if error.type_ == SetErrorType::NotFound => Ok(()),
        Err(error) => Err(client::map_error(error)),
    }
}

async fn query_draft_by_message_id(
    c: &ConnectedClient,
    draft_mailbox: &str,
    message_id: &str,
) -> Result<Option<String>> {
    let filter = jmap_client::core::query::Filter::and([
        jmap_client::email::query::Filter::in_mailbox(draft_mailbox),
        jmap_client::email::query::Filter::header("Message-ID", Some(format!("<{message_id}>"))),
    ]);
    let matches =
        c.client
            .email_query(
                Some(filter),
                None::<
                    Vec<
                        jmap_client::core::query::Comparator<jmap_client::email::query::Comparator>,
                    >,
                >,
            )
            .await
            .map_err(client::map_error)?;
    match matches.ids() {
        [] => Ok(None),
        [id] => Ok(Some(id.clone())),
        _ => Err(CoreError::Jmap(
            "multiple remote drafts share this Message-ID; refusing to create another copy".into(),
        )),
    }
}

async fn persist_remote_draft(
    ctx: &SyncCtx,
    draft_id: i64,
    remote: String,
    blob: Option<String>,
    thread: Option<String>,
) -> Result<()> {
    let thread_id = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let account_id = tx.query_row(
                "SELECT account_id FROM messages WHERE id=?1",
                params![draft_id],
                |row| row.get::<_, i64>(0),
            )?;
            let draft_folder_id = repo::folders::by_jmap_role(&tx, account_id, roles::DRAFTS)?
                .map(|folder| folder.id)
                .ok_or_else(|| {
                    CoreError::Jmap("local Drafts mailbox projection is missing".into())
                })?;
            repo::messages::set_jmap_remote(&tx, draft_id, &remote, blob.as_deref())?;
            tx.execute(
                "UPDATE messages SET folder_id=?2 WHERE id=?1 AND is_draft=1",
                params![draft_id, draft_folder_id],
            )?;
            repo::gmail::set_message_folders(&tx, draft_id, &[draft_folder_id])?;
            if let Some(thread) = thread {
                tx.execute(
                    "UPDATE threads SET jmap_id=COALESCE(jmap_id,?2)
                     WHERE id=(SELECT thread_id FROM messages WHERE id=?1)",
                    params![draft_id, thread],
                )?;
            }
            let thread_id = repo::messages::get_row(&tx, draft_id)?.and_then(|row| row.thread_id);
            if let Some(thread_id) = thread_id {
                repo::threads::recompute(&tx, thread_id)?;
            }
            tx.commit()?;
            Ok(thread_id)
        })
        .await?;
    if let Some(thread_id) = thread_id {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: vec![thread_id],
        });
    }
    Ok(())
}

async fn build_draft_message(
    ctx: &SyncCtx,
    config: &AccountConfig,
    c: &ConnectedClient,
    draft_id: i64,
) -> Result<(String, Vec<u8>)> {
    let (detail, bcc, mut references, in_reply_to, stored_message_id, attachments) = ctx
        .db
        .read(move |conn| {
            let detail = repo::messages::detail(conn, draft_id)?;
            let (bcc_json, stored_message_id): (String, Option<String>) = conn.query_row(
                "SELECT bcc_json,message_id FROM messages WHERE id=?1 AND is_draft=1",
                params![draft_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let parent_id = conn
                .query_row(
                    "SELECT in_reply_to_message_id FROM drafts_meta WHERE message_id=?1",
                    params![draft_id],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .optional()?
                .flatten();
            let mut references = Vec::new();
            let mut in_reply_to = None;
            if let Some(parent_id) = parent_id {
                let mut stmt =
                    conn.prepare("SELECT ref_message_id FROM message_refs WHERE message_id=?1")?;
                references = stmt
                    .query_map(params![parent_id], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if let Some(parent) = repo::messages::get_row(conn, parent_id)?
                    && let Some(message_id) = parent.message_id
                {
                    references.push(message_id.clone());
                    in_reply_to = Some(message_id);
                }
            }
            let mut stmt =
                conn.prepare("SELECT file_path,filename FROM draft_attachments WHERE draft_id=?1")?;
            let attachments = stmt
                .query_map(params![draft_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let bcc = serde_json::from_str::<Vec<Address>>(&bcc_json)?;
            Ok((
                detail,
                bcc,
                references,
                in_reply_to,
                stored_message_id,
                attachments,
            ))
        })
        .await?;
    references.sort();
    references.dedup();

    let server_max_upload = c
        .client
        .session()
        .core_capabilities()
        .map(|capabilities| capabilities.max_size_upload())
        .unwrap_or(MAX_JMAP_MESSAGE_BYTES);
    let max_upload = server_max_upload.min(MAX_JMAP_MESSAGE_BYTES);
    if max_upload == 0 {
        return Err(CoreError::Jmap(
            "server does not permit JMAP message uploads".into(),
        ));
    }
    let mut estimated_size = detail
        .text_body
        .as_ref()
        .map_or(0_u64, |body| body.len() as u64)
        .saturating_add(
            detail
                .html_body
                .as_ref()
                .map_or(0_u64, |body| body.len() as u64),
        )
        .saturating_add(64 * 1024);
    let staging_root = tokio::fs::canonicalize(ctx.paths.draft_attachments_dir())
        .await
        .ok();
    let mut outgoing_attachments = Vec::new();
    for (path, filename) in attachments {
        let canonical = tokio::fs::canonicalize(&path)
            .await
            .map_err(|error| CoreError::Other(format!("attachment {filename}: {error}")))?;
        if !staging_root
            .as_ref()
            .is_some_and(|root| canonical.starts_with(root))
        {
            return Err(CoreError::Other(format!(
                "attachment {filename}: refusing file outside the staging area"
            )));
        }
        let attachment_size = tokio::fs::metadata(&canonical).await?.len();
        estimated_size = estimated_size
            .saturating_add((attachment_size.saturating_add(2) / 3).saturating_mul(4))
            .saturating_add(2048);
        if estimated_size > max_upload as u64 {
            return Err(CoreError::Jmap(format!(
                "message exceeds the server or local upload limit of {} MiB",
                max_upload.div_ceil(1024 * 1024)
            )));
        }
        outgoing_attachments.push(crate::mime::OutgoingAttachment {
            mime_type: crate::queue::mime_guess_from_name(&filename),
            filename,
            bytes: crate::file_io::read(
                canonical,
                usize::try_from(attachment_size).unwrap_or(MAX_JMAP_MESSAGE_BYTES),
                "draft attachment",
            )
            .await?,
        });
    }
    let domain = config
        .email
        .rsplit_once('@')
        .map(|(_, value)| value)
        .unwrap_or("localhost");
    let outgoing = crate::mime::OutgoingMessage {
        from: Address {
            name: config.display_name.clone(),
            email: config.email.clone(),
        },
        to: &detail.to,
        cc: &detail.cc,
        bcc: &bcc,
        subject: &detail.subject,
        body_text: detail.text_body.as_deref().unwrap_or_default(),
        body_html: detail.html_body.as_deref(),
        in_reply_to: in_reply_to.as_deref(),
        references: &references,
        message_id: stored_message_id.as_deref(),
        message_id_domain: domain,
        attachments: outgoing_attachments,
    };
    let (message_id, raw) = crate::mime::build_message(&outgoing)?;
    let raw = crate::mail_security::protect_draft(
        &ctx.db,
        config.id,
        draft_id,
        raw,
        outgoing
            .to
            .iter()
            .chain(outgoing.cc)
            .chain(outgoing.bcc)
            .cloned()
            .collect(),
    )
    .await?;
    if raw.len() > max_upload {
        return Err(CoreError::Jmap(format!(
            "message exceeds the server or local upload limit of {} MiB",
            max_upload.div_ceil(1024 * 1024)
        )));
    }
    let message_id = message_id.trim_matches(['<', '>']).to_owned();
    let stable_id = message_id.clone();
    ctx.db
        .write(move |conn| {
            conn.execute(
                "UPDATE messages SET message_id=?2 WHERE id=?1 AND is_draft=1",
                params![draft_id, stable_id],
            )?;
            Ok(())
        })
        .await?;
    Ok((message_id, raw))
}

async fn save_draft_action(
    ctx: &SyncCtx,
    config: &AccountConfig,
    c: &ConnectedClient,
    action: &repo::actions::PendingAction,
) -> Result<()> {
    if !action_is_inflight(ctx, action.id).await? {
        return Ok(());
    }
    let draft_id = action.payload["draftId"]
        .as_i64()
        .or(action.message_id)
        .ok_or_else(|| CoreError::Jmap("draft save has no draftId".into()))?;
    let (message_id, raw) = build_draft_message(ctx, config, c, draft_id).await?;
    let draft_mailbox =
        ensure_role_mailbox(ctx, config.id, c, roles::DRAFTS, "Drafts", Role::Drafts).await?;
    if !action_is_inflight(ctx, action.id).await? {
        return Ok(());
    }
    let existing_remote = ctx
        .db
        .read(move |conn| Ok(repo::messages::get_row(conn, draft_id)?.and_then(|row| row.jmap_id)))
        .await?;
    let discovered = query_draft_by_message_id(c, &draft_mailbox, &message_id).await?;
    if let Some(discovered) = discovered
        && existing_remote.as_deref() != Some(discovered.as_str())
    {
        if !action_is_inflight(ctx, action.id).await? {
            destroy_email_if_present(c, &discovered).await?;
            return Ok(());
        }
        persist_remote_draft(ctx, draft_id, discovered, None, None).await?;
        return Ok(());
    }

    let mut uploaded = c
        .client
        .upload(Some(&c.account_id), raw, Some("message/rfc822"))
        .await
        .map_err(client::map_error)?;
    let blob_id = uploaded.take_blob_id();
    if !action_is_inflight(ctx, action.id).await? {
        return Ok(());
    }
    if let Some(existing) = existing_remote {
        destroy_email_if_present(c, &existing).await?;
    }
    if !action_is_inflight(ctx, action.id).await? {
        return Ok(());
    }
    let mut request = c.client.build();
    let import = request
        .import_email()
        .account_id(&c.account_id)
        .email(blob_id)
        .mailbox_ids([draft_mailbox])
        .keywords(["$draft", "$seen"])
        .received_at(now_ms() / 1000);
    let create_id = import.create_id();
    let imported = request
        .send_import_email()
        .await
        .map_err(client::map_error)?
        .created(&create_id)
        .map_err(client::map_error)?;
    let remote = imported
        .id()
        .ok_or_else(|| CoreError::Jmap("Email/import returned no id".into()))?
        .to_owned();
    if !action_is_inflight(ctx, action.id).await? {
        destroy_email_if_present(c, &remote).await?;
        return Ok(());
    }
    let persisted = persist_remote_draft(
        ctx,
        draft_id,
        remote.clone(),
        imported.blob_id().map(str::to_owned),
        imported.thread_id().map(str::to_owned),
    )
    .await;
    if let Err(error) = persisted {
        // The local draft may have been deleted in the narrow interval after
        // the cancellation check. Do not strand the Email we just created.
        destroy_email_if_present(c, &remote).await?;
        return Err(error);
    }
    Ok(())
}

async fn action_is_inflight(ctx: &SyncCtx, action_id: i64) -> Result<bool> {
    ctx.db
        .read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT state='inflight' FROM pending_actions WHERE id=?1",
                    params![action_id],
                    |row| row.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false))
        })
        .await
}

async fn send_action(
    ctx: &SyncCtx,
    config: &AccountConfig,
    c: &ConnectedClient,
    action: &repo::actions::PendingAction,
) -> Result<()> {
    if !c.supports_submission {
        return Err(CoreError::Jmap(
            "server does not advertise JMAP EmailSubmission".into(),
        ));
    }
    let draft_id = action.payload["draftId"]
        .as_i64()
        .ok_or_else(|| CoreError::Jmap("send action has no draftId".into()))?;
    let submission_accepted = action.payload["jmapSubmissionAccepted"]
        .as_bool()
        .unwrap_or(false);
    let remote_email = if let Some(remote) = action.payload["jmapPreparedEmailId"].as_str() {
        remote.to_owned()
    } else if submission_accepted {
        // Compatibility with an action persisted after confirmation by an
        // older build, before the local draft was finalized.
        ctx.db
            .read(move |conn| {
                Ok(repo::messages::get_row(conn, draft_id)?.and_then(|row| row.jmap_id))
            })
            .await?
            .ok_or_else(|| {
                CoreError::SendUncertain(
                    "submission was accepted but its prepared Email id is unavailable; check Sent"
                        .into(),
                )
            })?
    } else {
        // First make the current local content a remote draft, then durably
        // pin the exact Email id before submission. A retry after a crash must
        // query that same EmailSubmission, never replace a possibly-sent Email.
        save_draft_action(ctx, config, c, action).await?;
        let remote = ctx
            .db
            .read(move |conn| {
                Ok(repo::messages::get_row(conn, draft_id)?.and_then(|row| row.jmap_id))
            })
            .await?
            .ok_or_else(|| CoreError::Jmap("prepared JMAP draft has no remote Email id".into()))?;
        mark_send_prepared(ctx, action, &remote).await?;
        remote
    };
    let sent_mailbox =
        ensure_role_mailbox(ctx, config.id, c, roles::SENT, "Sent", Role::Sent).await?;

    // A lost response must not cause a duplicate submission on retry.
    let already_submitted = submission_accepted
        || !c
            .client
            .email_submission_query(
                Some(jmap_client::email_submission::query::Filter::email_ids([
                    remote_email.clone(),
                ])),
                None::<
                    Vec<
                        jmap_client::core::query::Comparator<
                            jmap_client::email_submission::query::Comparator,
                        >,
                    >,
                >,
            )
            .await
            .map_err(client::map_error)?
            .ids()
            .is_empty();
    if !already_submitted {
        let mut identity_request = c.client.build();
        identity_request.get_identity().account_id(&c.account_id);
        let identities = identity_request
            .send_get_identity()
            .await
            .map_err(client::map_error)?
            .take_list();
        let identity_id = identities
            .iter()
            .find(|identity| {
                identity
                    .email()
                    .is_some_and(|email| identity_matches(email, &config.email))
            })
            .and_then(|identity| identity.id())
            .ok_or_else(|| {
                CoreError::Jmap(format!(
                    "server has no JMAP sending identity for {}",
                    config.email
                ))
            })?
            .to_owned();
        let (request, create_id) =
            submission_request(c, &remote_email, identity_id, sent_mailbox.clone());
        let mut response = request
            .send_set_email_submission()
            .await
            .map_err(map_submission_error)?;
        response.created(&create_id).map_err(map_submission_error)?;
    }
    if !submission_accepted {
        mark_submission_accepted(ctx, action).await?;
    }
    ensure_submitted_email_state(c, &remote_email, &sent_mailbox).await?;

    let sent_folder_id = ctx
        .db
        .read({
            let account_id = config.id;
            move |conn| {
                Ok(repo::folders::by_jmap_role(conn, account_id, roles::SENT)?
                    .map(|folder| folder.id))
            }
        })
        .await?
        .ok_or_else(|| CoreError::Jmap("local Sent mailbox projection is missing".into()))?;
    let (thread_id, staged_paths) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE messages SET is_draft=0,is_outgoing=1,is_read=1,
                  folder_id=?2,date=?3 WHERE id=?1",
                params![draft_id, sent_folder_id, now_ms()],
            )?;
            repo::gmail::set_message_folders(&tx, draft_id, &[sent_folder_id])?;
            tx.execute(
                "DELETE FROM drafts_meta WHERE message_id=?1",
                params![draft_id],
            )?;
            let staged_paths = repo::messages::take_draft_attachment_paths(&tx, draft_id)?;
            let thread_id = repo::messages::get_row(&tx, draft_id)?.and_then(|row| row.thread_id);
            if let Some(thread_id) = thread_id {
                repo::threads::recompute(&tx, thread_id)?;
            }
            repo::search::index_message(&tx, draft_id)?;
            tx.commit()?;
            Ok((thread_id, staged_paths))
        })
        .await?;
    for path in staged_paths {
        crate::remove_staged_attachment(&ctx.paths.draft_attachments_dir(), &path).await;
    }
    if let Some(thread_id) = thread_id {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: vec![thread_id],
        });
    }
    Ok(())
}

async fn ensure_role_mailbox(
    ctx: &SyncCtx,
    account_id: i64,
    c: &ConnectedClient,
    local_role: &'static str,
    name: &'static str,
    jmap_role: Role,
) -> Result<String> {
    if let Some(remote) = ctx
        .db
        .read(move |conn| {
            Ok(repo::folders::by_jmap_role(conn, account_id, local_role)?
                .and_then(|folder| folder.jmap_id))
        })
        .await?
    {
        return Ok(remote);
    }
    let mailbox = c
        .client
        .mailbox_create(name, None::<String>, jmap_role)
        .await
        .map_err(client::map_error)?;
    let remote = mailbox
        .id()
        .ok_or_else(|| CoreError::Jmap(format!("Mailbox/set returned no id for {name}")))?
        .to_owned();
    let saved_remote = remote.clone();
    ctx.db
        .write(move |conn| {
            repo::folders::upsert_jmap(conn, account_id, &saved_remote, name, Some(local_role))?;
            Ok(())
        })
        .await?;
    Ok(remote)
}

fn submission_request<'a>(
    c: &'a ConnectedClient,
    remote_email: &str,
    identity_id: String,
    sent_mailbox: String,
) -> (jmap_client::core::request::Request<'a>, String) {
    let mut request = c.client.build();
    let set = request.set_email_submission().account_id(&c.account_id);
    let create_id = set
        .create()
        .email_id(remote_email)
        .identity_id(identity_id)
        .create_id()
        .unwrap_or_else(|| "c0".into());
    set.arguments()
        .on_success_update_email(&create_id)
        .mailbox_ids([sent_mailbox])
        .keyword("$draft", false)
        .keyword("$seen", true);
    (request, create_id)
}

async fn ensure_submitted_email_state(
    c: &ConnectedClient,
    remote_email: &str,
    sent_mailbox: &str,
) -> Result<()> {
    let mut request = c.client.build();
    request
        .set_email()
        .account_id(&c.account_id)
        .update(remote_email)
        .mailbox_ids([sent_mailbox])
        .keyword("$draft", false)
        .keyword("$seen", true);
    let mut response = request.send_set_email().await.map_err(client::map_error)?;
    match response.updated(remote_email) {
        Ok(_) => Ok(()),
        Err(jmap_client::Error::Set(error)) if error.type_ == SetErrorType::NotFound => Ok(()),
        Err(error) => Err(client::map_error(error)),
    }
}

fn identity_matches(identity: &str, from: &str) -> bool {
    if identity.eq_ignore_ascii_case(from) {
        return true;
    }
    let Some((identity_local, identity_domain)) = identity.rsplit_once('@') else {
        return false;
    };
    let Some((_, from_domain)) = from.rsplit_once('@') else {
        return false;
    };
    identity_local == "*" && identity_domain.eq_ignore_ascii_case(from_domain)
}

fn map_submission_error(error: jmap_client::Error) -> CoreError {
    let uncertain = match &error {
        jmap_client::Error::Transport(_) | jmap_client::Error::Parse(_) => true,
        jmap_client::Error::Internal(_) => true,
        jmap_client::Error::Problem(problem) => {
            problem.status().is_some_and(|status| status >= 500)
        }
        _ => false,
    };
    if uncertain {
        CoreError::SendUncertain(format!(
            "the server may have accepted the message, but confirmation was lost ({error}); check Sent before retrying"
        ))
    } else {
        client::map_error(error)
    }
}

async fn mark_send_prepared(
    ctx: &SyncCtx,
    action: &repo::actions::PendingAction,
    remote_email: &str,
) -> Result<()> {
    let action_id = action.id;
    let remote_email = remote_email.to_owned();
    ctx.db
        .write(move |conn| {
            let mut current = repo::actions::get(conn, action_id)?
                .ok_or_else(|| CoreError::NotFound(format!("action {action_id}")))?;
            current.payload["jmapPreparedEmailId"] = serde_json::Value::String(remote_email);
            repo::actions::set_payload(conn, action_id, &current.payload)
        })
        .await
}

async fn mark_submission_accepted(
    ctx: &SyncCtx,
    action: &repo::actions::PendingAction,
) -> Result<()> {
    let action_id = action.id;
    ctx.db
        .write(move |conn| {
            let mut current = repo::actions::get(conn, action_id)?
                .ok_or_else(|| CoreError::NotFound(format!("action {action_id}")))?;
            current.payload["jmapSubmissionAccepted"] = serde_json::Value::Bool(true);
            repo::actions::set_payload(conn, action_id, &current.payload)
        })
        .await
        .map_err(|error| {
            CoreError::SendUncertain(format!(
                "submission succeeded but its local confirmation could not be saved ({error}); check Sent before retrying"
            ))
        })
}

async fn body_actor(ctx: SyncCtx, config: AccountConfig, mut rx: mpsc::Receiver<PriorityFetchCmd>) {
    while let Some(command) = rx.recv().await {
        match command {
            PriorityFetchCmd::Body(message_id) => {
                if let Err(error) = fetch_body(&ctx, &config, message_id).await {
                    tracing::warn!(account_id=config.id, message_id, error=%error, "JMAP body fetch failed");
                    let _ = ctx
                        .db
                        .write(move |conn| repo::messages::set_body_state(conn, message_id, "none"))
                        .await;
                }
            }
            PriorityFetchCmd::Attachment {
                attachment_id,
                complete,
            } => {
                let result = fetch_attachment(&ctx, &config, attachment_id)
                    .await
                    .map_err(|error| error.to_string());
                let _ = complete.send(result);
            }
        }
    }
}

async fn fetch_body(ctx: &SyncCtx, config: &AccountConfig, message_id: i64) -> Result<()> {
    let row = ctx
        .db
        .read(move |conn| repo::messages::get_row(conn, message_id))
        .await?
        .ok_or_else(|| CoreError::NotFound(format!("message {message_id}")))?;
    if row.body_state == "cached" {
        return Ok(());
    }
    let remote_id = row
        .jmap_id
        .ok_or_else(|| CoreError::NotFound("JMAP message id".into()))?;
    let expected_blob = row
        .jmap_blob_id
        .ok_or_else(|| CoreError::NotFound("JMAP blob id".into()))?;
    if row.size.is_some_and(|size| {
        size < 0 || usize::try_from(size).map_or(true, |size| size > MAX_JMAP_MESSAGE_BYTES)
    }) {
        return Err(CoreError::Jmap(format!(
            "message exceeds the {} MiB local safety limit",
            MAX_JMAP_MESSAGE_BYTES / (1024 * 1024)
        )));
    }
    let connected = connect(ctx, config).await?;
    let raw = download_message_bounded(&connected, &expected_blob).await?;
    let parsed = crate::mime::parse_message(&raw)?;
    let calendar_events = parsed
        .calendar_parts
        .iter()
        .flat_map(|ics| crate::calendar::parse_ics(ics))
        .collect::<Vec<_>>();
    let dir = ctx.paths.mail_dir(config.id);
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{message_id}.jmap.eml"));
    crate::file_io::write_atomic(&path, &raw, "JMAP message cache").await?;
    let path_string = path.to_string_lossy().into_owned();
    let guarded_remote = remote_id.clone();
    let guarded_blob = expected_blob.clone();
    let (persisted, thread_id) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let current = repo::messages::get_row(&tx, message_id)?;
            let valid = current.as_ref().is_some_and(|current| {
                current.jmap_id.as_deref() == Some(&guarded_remote)
                    && current.jmap_blob_id.as_deref() == Some(&guarded_blob)
            });
            if !valid {
                return Ok((false, None));
            }
            repo::messages::store_body(
                &tx,
                message_id,
                parsed.text.as_deref(),
                parsed.html.as_deref(),
                Some(&path_string),
                parsed
                    .attachments
                    .iter()
                    .any(|attachment| !attachment.is_inline),
                Some(&parsed.snippet),
            )?;
            repo::messages::set_sender_verification(
                &tx,
                message_id,
                parsed.headers.sender_verification,
            )?;
            let attachments = parsed
                .attachments
                .iter()
                .map(|attachment| repo::messages::NewAttachment {
                    message_id,
                    part_id: Some(&attachment.part_id),
                    filename: attachment.filename.as_deref(),
                    mime_type: attachment.mime_type.as_deref(),
                    size: Some(attachment.size),
                    content_id: attachment.content_id.as_deref(),
                    is_inline: attachment.is_inline,
                })
                .collect::<Vec<_>>();
            repo::messages::replace_attachments(&tx, message_id, &attachments)?;
            repo::search::index_message(&tx, message_id)?;
            let thread_id = current.and_then(|row| row.thread_id);
            if let Some(thread_id) = thread_id {
                repo::threads::recompute(&tx, thread_id)?;
            }
            tx.commit()?;
            Ok((true, thread_id))
        })
        .await?;
    if !persisted {
        let _ = tokio::fs::remove_file(path).await;
        return Ok(());
    }
    if !calendar_events.is_empty() {
        let account_id = config.id;
        ctx.calendar_db
            .write(move |conn| {
                let tx = conn.transaction()?;
                for event in &calendar_events {
                    repo::calendar::upsert(&tx, account_id, message_id, event)?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        ctx.bus.emit(CoreEvent::CalendarUpdated { account_id });
    }
    if let Some(thread_id) = thread_id {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: vec![thread_id],
        });
    }
    Ok(())
}

async fn download_message_bounded(c: &ConnectedClient, blob_id: &str) -> Result<Vec<u8>> {
    use jmap_client::blob::URLParameter;
    use jmap_client::core::session::URLPart;
    use reqwest::header::CONTENT_LENGTH;

    let mut url = String::with_capacity(
        c.client.session().download_url().len() + c.account_id.len() + blob_id.len(),
    );
    for part in c.client.download_url() {
        match part {
            URLPart::Value(value) => url.push_str(value),
            URLPart::Parameter(URLParameter::AccountId) => url.push_str(&c.account_id),
            URLPart::Parameter(URLParameter::BlobId) => url.push_str(blob_id),
            URLPart::Parameter(URLParameter::Name) => url.push_str("message.eml"),
            URLPart::Parameter(URLParameter::Type) => url.push_str("message/rfc822"),
        }
    }
    let response =
        c.download_http.get(url).send().await.map_err(|error| {
            CoreError::Network(format!("JMAP message download failed: {error}"))
        })?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Err(CoreError::Auth(
            "JMAP rejected credentials while downloading the message".into(),
        ));
    }
    if !status.is_success() {
        return Err(CoreError::Jmap(format!(
            "JMAP message download returned HTTP {status}"
        )));
    }
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|size| size > MAX_JMAP_MESSAGE_BYTES)
    {
        return Err(CoreError::Jmap(format!(
            "message exceeds the {} MiB local safety limit",
            MAX_JMAP_MESSAGE_BYTES / (1024 * 1024)
        )));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            CoreError::Network(format!("JMAP message download failed: {error}"))
        })?;
        if bytes.len().saturating_add(chunk.len()) > MAX_JMAP_MESSAGE_BYTES {
            return Err(CoreError::Jmap(format!(
                "message exceeds the {} MiB local safety limit",
                MAX_JMAP_MESSAGE_BYTES / (1024 * 1024)
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn fetch_attachment(
    ctx: &SyncCtx,
    config: &AccountConfig,
    attachment_id: i64,
) -> Result<Vec<u8>> {
    let (message_id, part_id, raw_path, blob_id) = ctx
        .db
        .read(move |conn| {
            conn.query_row(
                "SELECT a.message_id,a.part_id,m.raw_path,a.jmap_blob_id FROM attachments a
                 JOIN messages m ON m.id=a.message_id WHERE a.id=?1",
                params![attachment_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .map_err(Into::into)
        })
        .await?;
    if let Some(blob) = blob_id {
        let connected = connect(ctx, config).await?;
        return download_message_bounded(&connected, &blob).await;
    }
    let part_id = part_id.ok_or_else(|| CoreError::NotFound("attachment MIME part".into()))?;
    let cached = match raw_path {
        Some(path) => crate::file_io::read(path, MAX_JMAP_MESSAGE_BYTES, "cached JMAP message")
            .await
            .ok(),
        None => None,
    };
    let raw = match cached {
        Some(raw) => raw,
        None => {
            fetch_body(ctx, config, message_id).await?;
            let path = ctx
                .db
                .read(move |conn| {
                    repo::messages::get_row(conn, message_id)?
                        .and_then(|row| row.raw_path)
                        .ok_or_else(|| CoreError::NotFound("cached JMAP MIME message".into()))
                })
                .await?;
            crate::file_io::read(path, MAX_JMAP_MESSAGE_BYTES, "cached JMAP message").await?
        }
    };
    crate::mime::extract_attachment(&raw, &part_id).map(|(bytes, _)| bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_paths_preserve_hierarchy_and_reject_cycles() {
        let hierarchy = HashMap::from([
            ("root".into(), ("Projects".into(), None)),
            ("child".into(), ("Release".into(), Some("root".into()))),
            ("cycle-a".into(), ("A".into(), Some("cycle-b".into()))),
            ("cycle-b".into(), ("B".into(), Some("cycle-a".into()))),
        ]);
        assert_eq!(
            mailbox_path("child", &hierarchy).unwrap(),
            "Projects / Release"
        );
        assert!(mailbox_path("cycle-a", &hierarchy).is_err());
        assert_eq!(mailbox_path("missing", &hierarchy).unwrap(), "Mailbox");
    }

    #[test]
    fn sending_identity_must_match_exactly_or_by_wildcard_domain() {
        assert!(identity_matches("Me@Example.test", "me@example.test"));
        assert!(identity_matches("*@example.test", "alias@EXAMPLE.TEST"));
        assert!(!identity_matches("other@example.test", "me@example.test"));
        assert!(!identity_matches("*@other.test", "me@example.test"));
    }

    #[test]
    fn ambiguous_submission_failures_are_never_automatically_retried() {
        assert!(matches!(
            map_submission_error(jmap_client::Error::Internal("lost response".into())),
            CoreError::SendUncertain(_)
        ));
        assert!(matches!(
            map_submission_error(jmap_client::Error::Server("rejected".into())),
            CoreError::Jmap(_)
        ));
    }

    #[tokio::test]
    async fn submission_updates_are_keyed_by_creation_reference() {
        let (origin, server) = crate::jmap::client::tests::session_server(true).await;
        let connected = crate::jmap::client::connect_with(
            "me@example.test",
            "me@example.test",
            "app-password",
            &origin,
            None,
        )
        .await
        .unwrap();
        server.await.unwrap();
        let (request, create_id) =
            submission_request(&connected, "email-1", "identity-1".into(), "sent-1".into());
        let json = serde_json::to_value(request).unwrap();
        let updates = json["methodCalls"][0][1]["onSuccessUpdateEmail"]
            .as_object()
            .unwrap();
        assert!(updates.contains_key(&format!("#{create_id}")));
        assert!(!updates.contains_key("email-1"));
    }
}

#[cfg(test)]
mod attachment_metadata_tests {
    use super::*;
    #[test]
    fn header_attachment_metadata_preserves_identity_and_invalidates_changed_blobs() {
        let db = crate::db::testutil::conn();
        crate::db::testutil::seed_account(&db);
        db.execute("INSERT INTO messages(id,account_id,folder_id,subject,from_addr,date) VALUES(99,1,1,'File metadata','sender@test.dev',0)",[]).unwrap();
        let parts:Vec<jmap_client::email::EmailBodyPart>=serde_json::from_value(serde_json::json!([{"partId":"2","blobId":"blob1","name":"notes.txt","type":"text/plain","size":12,"disposition":"attachment"}])).unwrap();
        persist_attachment_parts(&db, 99, &parts).unwrap();
        let id: i64 = db
            .query_row("SELECT id FROM attachments WHERE message_id=99", [], |r| {
                r.get(0)
            })
            .unwrap();
        db.execute(
            "UPDATE attachments SET file_path='/private/cached' WHERE id=?1",
            [id],
        )
        .unwrap();
        persist_attachment_parts(&db, 99, &parts).unwrap();
        let same: (i64, String) = db
            .query_row(
                "SELECT id,file_path FROM attachments WHERE message_id=99",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(same, (id, "/private/cached".into()));
        persist_attachment_parts(&db, 99, &[]).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM attachment_files_fts WHERE rowid=?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}
