//! Durable JMAP Mail synchronization and submission actor.

use crate::accounts::credentials::{self, Slot};
use crate::db::repo;
use crate::db::repo::messages::NewMessage;
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use crate::jmap::client::{self, ConnectedClient};
use crate::models::{AccountConfig, Address, MailProtocol, SenderDeliveryStatus, now_ms, roles};
use crate::sync::engine::{
    PriorityFetchCmd, SyncCmd, SyncCtx, configured_sync_interval, set_state, set_state_error,
};
use futures::{FutureExt, Stream, StreamExt};
use jmap_client::core::error::MethodErrorType;
use jmap_client::core::query::QueryResponse;
use jmap_client::core::response::{
    EmailGetResponse, EmailSetResponse, EmailSubmissionSetResponse, IdentityGetResponse,
    MailboxGetResponse,
};
use jmap_client::core::set::{SetErrorType, SetObject};
use jmap_client::email::import::EmailImportResponse;
use jmap_client::email::{BodyProperty, Email, Header, HeaderValue, Property as EmailProperty};
use jmap_client::event_source::PushNotification;
use jmap_client::mailbox::{Mailbox, Property as MailboxProperty, Role};
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use tokio::sync::{mpsc, oneshot, watch};

const PAGE: usize = 500;
const MAX_QUERY_RESTARTS: usize = 3;
const MAX_CHANGE_PAGES: usize = 10_000;
const MAX_JMAP_MESSAGE_BYTES: usize = 128 * 1024 * 1024;
/// Stalwart reads a message in full to list its attachments, so one listing
/// request covers at most this many message bytes, or a single larger
/// message. A request the server doesn't answer in time lowers it.
const ATTACHMENT_LISTING_BYTES: i64 = 32 * 1024 * 1024;
const ATTACHMENT_LISTING_EMAILS: usize = 50;

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

const IDENTITY_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

async fn sync_sender_identities(
    ctx: &SyncCtx,
    config: &AccountConfig,
    c: &ConnectedClient,
) -> Result<()> {
    let identities = crate::sender_identities::jmap_sender_identities(config, c).await?;
    let account_id = config.id;
    ctx.db
        .write(move |conn| repo::sender_identities::replace(conn, account_id, &identities))
        .await
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
    let mut identities_synced_at: Option<std::time::Instant> = None;
    let mut listing_bytes = ATTACHMENT_LISTING_BYTES;
    // Kept from one sync to the next, with its connection. A failed sync or a
    // changed Session makes the next one connect again.
    let mut session: Option<ConnectedClient> = None;
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
        let reused = session.take();
        let outcome = async {
            let connected = match reused {
                Some(connected) if !connected.session_changed() => connected,
                _ => {
                    let connected = connect(&ctx, &config).await?;
                    persist_session(&ctx, config.id, &connected).await?;
                    connected
                }
            };
            if identities_synced_at.is_none_or(|at| at.elapsed() >= IDENTITY_REFRESH_INTERVAL) {
                // Identities change rarely and only gate the From picker, so
                // a failure here must not block mail synchronization.
                match sync_sender_identities(&ctx, &config, &connected).await {
                    Ok(()) => identities_synced_at = Some(std::time::Instant::now()),
                    Err(error) => tracing::warn!(
                        account_id = config.id,
                        %error,
                        "JMAP identity refresh failed; keeping cached identities"
                    ),
                }
            }
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
        let deadline = tokio::time::sleep(delay);
        tokio::pin!(deadline);
        let mut push = None;
        if let Some(connected) = &connected {
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
                Ok(stream) => push = Some(stream),
                Err(error) => {
                    tracing::debug!(account_id=config.id, error=%error, "JMAP push unavailable; using polling")
                }
            }
        }
        let mut listing = connected.is_some();
        loop {
            let wake = next_wake(
                &mut rx,
                &mut settings,
                &mut waiters,
                push.as_mut(),
                deadline.as_mut(),
                config.id,
            );
            let wake = match (&connected, listing) {
                // Attachment listing fills idle time one request at a time and
                // gives way as soon as anything else needs the actor.
                (Some(connected), true) => match wake.now_or_never() {
                    Some(wake) => wake,
                    None => {
                        listing = match list_attachments(
                            &ctx,
                            &config,
                            connected,
                            &mut listing_bytes,
                        )
                        .await
                        {
                            Ok(listed) => listed,
                            Err(error) => {
                                tracing::warn!(account_id = config.id, %error, "JMAP attachment listing failed; retrying after the next sync");
                                false
                            }
                        };
                        continue;
                    }
                },
                _ => wake.await,
            };
            match wake {
                Wake::Shutdown => return,
                Wake::Sync => break,
            }
        }
        // The push stream borrows the session it was opened on.
        drop(push);
        session = connected;
    }
}

enum Wake {
    Shutdown,
    Sync,
}

/// Waits for the next reason to sync: a command, a settings change, the
/// sync interval, or a push notification.
async fn next_wake<S>(
    rx: &mut mpsc::Receiver<SyncCmd>,
    settings: &mut watch::Receiver<crate::models::AccountSettings>,
    waiters: &mut Vec<oneshot::Sender<std::result::Result<(), String>>>,
    push: Option<&mut S>,
    deadline: Pin<&mut tokio::time::Sleep>,
    account_id: i64,
) -> Wake
where
    S: Stream<Item = jmap_client::Result<PushNotification>> + Unpin,
{
    let notification = async {
        match push {
            Some(stream) => stream.next().await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        command = rx.recv() => match command {
            None | Some(SyncCmd::Shutdown) => Wake::Shutdown,
            Some(SyncCmd::SyncNow { complete: Some(done) }) => {
                waiters.push(done);
                Wake::Sync
            }
            Some(_) => Wake::Sync,
        },
        _ = settings.changed() => Wake::Sync,
        _ = deadline => Wake::Sync,
        notification = notification => {
            if let Some(Err(error)) = notification {
                tracing::debug!(account_id, error=%error, "JMAP push stream ended; polling fallback will continue");
            }
            Wake::Sync
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
    let can_create_top_level_mailbox = connected.may_create_top_level_mailbox;
    ctx.db
        .write(move |conn| {
            conn.execute(
                "UPDATE accounts
                 SET jmap_url=?2, jmap_account_id=?3, can_create_top_level_mailbox=?4
                 WHERE id=?1",
                params![id, url, remote, can_create_top_level_mailbox],
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
    let (state, mailboxes) = fetch_mailboxes(c).await?;
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
            let mut local_ids = HashMap::with_capacity(mailboxes.len());
            for mailbox in mailboxes {
                let id = mailbox.id().ok_or_else(|| {
                    CoreError::Jmap("Mailbox/get returned an object without id".into())
                })?;
                let display_name = mailbox_path(id, &hierarchy)?;
                let local_id = repo::folders::upsert_jmap(
                    &tx,
                    local_account,
                    id,
                    &display_name,
                    role(mailbox.role()),
                )?;
                let rights = mailbox.my_rights();
                repo::folders::set_permissions(
                    &tx,
                    local_id,
                    repo::folders::FolderPermissions {
                        can_create_children: rights.is_some_and(|rights| rights.may_create_child()),
                        can_rename: rights.is_some_and(|rights| rights.may_rename()),
                        can_delete: rights.is_some_and(|rights| rights.may_delete()),
                    },
                )?;
                local_ids.insert(id.to_owned(), local_id);
            }
            for (remote_id, (_, parent_remote_id)) in &hierarchy {
                let local_id = local_ids.get(remote_id).copied().ok_or_else(|| {
                    CoreError::Jmap(format!("Mailbox {remote_id} was not persisted"))
                })?;
                let parent_id = parent_remote_id
                    .as_ref()
                    .and_then(|parent| local_ids.get(parent))
                    .copied();
                repo::folders::set_parent(&tx, local_id, parent_id)?;
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

/// Every mailbox of the account and the Mailbox state they share.
///
/// Asked for without ids, Stalwart returns no more mailboxes than its get
/// limit and says nothing of the rest. An answer that fills the limit is
/// checked against Mailbox/query and fetched again in pages.
async fn fetch_mailboxes(c: &ConnectedClient) -> Result<(String, Vec<Mailbox>)> {
    let limit = max_objects_in_get(c);
    let (state, mailboxes) = get_mailboxes(c, None).await?;
    if mailboxes.len() < limit {
        return Ok((state, mailboxes));
    }
    'restart: for _ in 0..MAX_QUERY_RESTARTS {
        let ids = query_all("Mailbox/query", |position| async move {
            let mut request = c.request();
            request
                .query_mailbox()
                .account_id(&c.account_id)
                .position(position)
                .limit(PAGE)
                .calculate_total(true);
            c.send_single::<QueryResponse>(request).await
        })
        .await?;
        let mut state: Option<String> = None;
        let mut mailboxes = Vec::with_capacity(ids.len());
        for page in ids.chunks(limit) {
            let (page_state, list) = get_mailboxes(c, Some(page)).await?;
            if state.get_or_insert_with(|| page_state.clone()) != &page_state {
                continue 'restart;
            }
            mailboxes.extend(list);
        }
        if let Some(state) = state {
            return Ok((state, mailboxes));
        }
    }
    Err(CoreError::Jmap(
        "mailboxes changed repeatedly while listing them; retrying later".into(),
    ))
}

async fn get_mailboxes(
    c: &ConnectedClient,
    ids: Option<&[String]>,
) -> Result<(String, Vec<Mailbox>)> {
    let mut request = c.request();
    // Counters (totalEmails, unreadThreads, ...) are computed locally; asking
    // for them makes the server count every mailbox of a large account.
    let get = request.get_mailbox().account_id(&c.account_id).properties([
        MailboxProperty::Id,
        MailboxProperty::Name,
        MailboxProperty::ParentId,
        MailboxProperty::Role,
        MailboxProperty::MyRights,
    ]);
    if let Some(ids) = ids {
        get.ids(ids.iter().cloned());
    }
    let mut response: MailboxGetResponse =
        c.send_single(request).await.map_err(client::map_error)?;
    Ok((response.take_state(), response.take_list()))
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

/// The Emails a full sync goes through: the account's mail history, reaching
/// back to the oldest message an earlier server or protocol of the account
/// left here, so it can be matched with its copy on this server.
async fn full_sync_ids(
    ctx: &SyncCtx,
    config: &AccountConfig,
    c: &ConnectedClient,
) -> Result<Vec<String>> {
    let Some(cutoff) = config.settings.mail_history.cutoff_ms_at(now_ms()) else {
        return query_ids(c, None).await;
    };
    let account_id = config.id;
    let oldest_leftover = ctx
        .db
        .read(move |conn| {
            Ok(repo::messages::jmap_leftovers(conn, account_id)?
                .into_iter()
                .map(|leftover| leftover.received_at)
                .min())
        })
        .await?;
    // A day's margin covers servers recording slightly different times.
    let received_after = oldest_leftover.map_or(cutoff, |oldest| {
        cutoff.min(oldest.saturating_sub(24 * 60 * 60 * 1000))
    });
    query_ids(c, Some(received_after)).await
}

/// The Email state an unfinished full sync started from.
async fn unfinished_full_sync(ctx: &SyncCtx, id: i64) -> Result<Option<String>> {
    ctx.db
        .read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT full_sync_state FROM jmap_sync_state WHERE account_id=?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()?
                .flatten())
        })
        .await
}

/// Starts a full sync from the server's current Email state. Recording the
/// state first lets an interrupted run resume: checkpointing that older state
/// at the end is safe because Email/changes then repeats whatever changed
/// meanwhile.
async fn begin_full_sync(ctx: &SyncCtx, id: i64, c: &ConnectedClient) -> Result<String> {
    let state = current_email_state(c).await?;
    let started = state.clone();
    ctx.db
        .write(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO jmap_sync_state(account_id,full_sync_state) VALUES(?1,?2)
                 ON CONFLICT(account_id) DO UPDATE SET full_sync_state=excluded.full_sync_state",
                params![id, started],
            )?;
            tx.execute(
                "DELETE FROM jmap_full_sync_stored WHERE account_id=?1",
                params![id],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(state)
}

async fn full_sync_stored(ctx: &SyncCtx, id: i64) -> Result<HashSet<String>> {
    ctx.db
        .read(move |conn| {
            let mut stmt =
                conn.prepare("SELECT email_id FROM jmap_full_sync_stored WHERE account_id=?1")?;
            Ok(stmt
                .query_map(params![id], |r| r.get(0))?
                .collect::<rusqlite::Result<HashSet<String>>>()?)
        })
        .await
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

/// Every Email received after `received_after` (ms), newest first.
async fn query_ids(c: &ConnectedClient, received_after: Option<i64>) -> Result<Vec<String>> {
    let cutoff = received_after.map(|v| v / 1000);
    query_all("Email/query", |position| async move {
        let mut request = c.request();
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
        c.send_single::<QueryResponse>(request).await
    })
    .await
}

/// Pages through a query until it has every id, starting over whenever the
/// results change underneath it.
async fn query_all<F, Fut>(name: &str, mut page: F) -> Result<Vec<String>>
where
    F: FnMut(i32) -> Fut,
    Fut: Future<Output = jmap_client::Result<QueryResponse>>,
{
    'restart: for _ in 0..MAX_QUERY_RESTARTS {
        let mut ids = Vec::new();
        let mut query_state: Option<String> = None;
        let mut expected_total: Option<usize> = None;
        loop {
            let position = i32::try_from(ids.len())
                .map_err(|_| CoreError::Jmap(format!("{name} result set is too large")))?;
            let mut response = page(position).await.map_err(client::map_error)?;
            let response_state = response.query_state().to_owned();
            let total = response
                .total()
                .ok_or_else(|| CoreError::Jmap(format!("{name} omitted its requested total")))?;
            if response.position() != position
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
    Err(CoreError::Jmap(format!(
        "{name} results changed repeatedly while paging; retrying later"
    )))
}

async fn changes(
    c: &ConnectedClient,
    mut state: String,
) -> Result<(Vec<String>, Vec<String>, String)> {
    let mut ids = Vec::new();
    let mut destroyed = Vec::new();
    for _ in 0..MAX_CHANGE_PAGES {
        let previous_state = state.clone();
        let mut response = match c.email_changes(state.clone(), PAGE).await {
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
    let mut request = c.request();
    request
        .get_email()
        .account_id(&c.account_id)
        .ids(Vec::<String>::new())
        .properties([EmailProperty::Id]);
    let mut response: EmailGetResponse = c.send_single(request).await.map_err(client::map_error)?;
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
        // Not `attachments`: see `list_attachments`.
        EmailProperty::HasAttachment,
        EmailProperty::Preview,
        EmailProperty::Header(Header::as_text("Auto-Submitted", false)),
        EmailProperty::Header(Header::as_text("List-Id", false)),
        EmailProperty::Header(Header::as_text("Precedence", false)),
    ]
}

fn header_text<'a>(email: &'a Email, name: &str) -> Option<&'a str> {
    match email.header(&Header::as_text(name, false)) {
        Some(HeaderValue::AsText(value)) => Some(value.trim()),
        _ => None,
    }
}

fn automated_email(email: &Email) -> bool {
    header_text(email, "Auto-Submitted")
        .is_some_and(|value| !value.is_empty() && !value.eq_ignore_ascii_case("no"))
        || header_text(email, "List-Id").is_some_and(|value| !value.is_empty())
        || header_text(email, "Precedence").is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "bulk" | "list" | "junk"
            )
        })
}

async fn sync_emails(ctx: &SyncCtx, config: &AccountConfig, c: &ConnectedClient) -> Result<()> {
    let account_id = config.id;
    let (ids, mut destroyed, checkpoint_state, full) =
        match unfinished_full_sync(ctx, account_id).await? {
            Some(state) => (
                full_sync_ids(ctx, config, c).await?,
                Vec::new(),
                state,
                true,
            ),
            None => match saved_email_state(ctx, account_id).await? {
                Some(state) => match changes(c, state).await {
                    Ok((ids, destroyed, state)) => (ids, destroyed, state, false),
                    Err(CoreError::Jmap(value)) if value == "cannotCalculateChanges" => {
                        let state = begin_full_sync(ctx, account_id, c).await?;
                        (
                            full_sync_ids(ctx, config, c).await?,
                            Vec::new(),
                            state,
                            true,
                        )
                    }
                    Err(error) => return Err(error),
                },
                None => {
                    let state = begin_full_sync(ctx, account_id, c).await?;
                    (
                        full_sync_ids(ctx, config, c).await?,
                        Vec::new(),
                        state,
                        true,
                    )
                }
            },
        };
    // A full sync downloads the account's mail history. Older mail it lists
    // only replaces what an earlier server left here.
    let insert_after = if full {
        config.settings.mail_history.cutoff_ms_at(now_ms())
    } else {
        None
    };
    // A full sync that stopped midway skips what it already stored.
    let stored = if full {
        full_sync_stored(ctx, account_id).await?
    } else {
        HashSet::new()
    };
    let (skipped, ids): (Vec<_>, Vec<_>) = ids.into_iter().partition(|id| stored.contains(id));
    // Each Email/get page is committed on its own while the next one is in
    // flight: a first sync fills the mailbox newest first instead of showing
    // nothing until the whole history is downloaded. The email state is only
    // checkpointed once every page is stored.
    let mut remote_ids = skipped.into_iter().collect::<HashSet<_>>();
    remote_ids.reserve(ids.len());
    let mut pages = HeaderPages {
        ids: &ids,
        position: 0,
        size: max_objects_in_get(c),
    };
    let mut fetched = pages.next(c).await?;
    while let Some((emails, not_found)) = fetched.take() {
        destroyed.extend(not_found);
        remote_ids.extend(
            emails
                .iter()
                .filter_map(|email| email.id().map(str::to_owned)),
        );
        let (next, persisted) = tokio::join!(
            pages.next(c),
            persist_emails(ctx, config, emails, full, insert_after)
        );
        persisted?;
        fetched = next?;
    }
    destroyed.sort();
    destroyed.dedup();
    finish_email_sync(ctx, config, remote_ids, destroyed, checkpoint_state, full).await
}

/// The header pages of one sync run, newest first.
struct HeaderPages<'a> {
    ids: &'a [String],
    position: usize,
    size: usize,
}

impl HeaderPages<'_> {
    /// The next page of headers and the ids the server no longer has. A page
    /// the server doesn't answer in time is asked for again in halves, and
    /// later pages keep the smaller size, so a slow server still makes
    /// progress.
    async fn next(&mut self, c: &ConnectedClient) -> Result<Option<(Vec<Email>, Vec<String>)>> {
        while self.position < self.ids.len() {
            let end = self.ids.len().min(self.position + self.size);
            match fetch_headers(c, &self.ids[self.position..end]).await {
                Ok(page) => {
                    self.position = end;
                    return Ok(Some(page));
                }
                Err(error) if client::is_timeout(&error) && end - self.position > 1 => {
                    self.size = (end - self.position) / 2;
                    tracing::debug!(
                        size = self.size,
                        "JMAP header page timed out; asking for fewer"
                    );
                }
                Err(error) => return Err(client::map_error(error)),
            }
        }
        Ok(None)
    }
}

async fn fetch_headers(
    c: &ConnectedClient,
    ids: &[String],
) -> jmap_client::Result<(Vec<Email>, Vec<String>)> {
    let mut request = c.request();
    request
        .get_email()
        .account_id(&c.account_id)
        .ids(ids.iter().cloned())
        .properties(header_properties());
    let mut response: EmailGetResponse = c.send_single(request).await?;
    Ok((response.take_list(), response.take_not_found()))
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

/// Stores a page of headers. A full sync also records each message as
/// stored, so an interrupted run need not fetch it again. An Email received
/// before `insert_after` only updates a message already here.
async fn persist_emails(
    ctx: &SyncCtx,
    config: &AccountConfig,
    emails: Vec<Email>,
    full_sync: bool,
    insert_after: Option<i64>,
) -> Result<()> {
    let account_id = config.id;
    let (thread_ids, stale_paths) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let settings = repo::settings::get(&tx)?;
            let (outgoing_learning_since, incoming_learning_since) =
                repo::contacts::learning_boundaries(&tx, account_id, now_ms())?;
            let account_emails = repo::sender_identities::own_addresses(&tx, account_id)?;
            let mut changed_threads = HashSet::new();
            let mut stale_paths = Vec::new();

            for email in emails {
                let remote_id = email
                    .id()
                    .map(str::to_owned)
                    .ok_or_else(|| CoreError::Jmap("Email/get returned an object without id".into()))?;
                if full_sync {
                    tx.execute(
                        "INSERT OR IGNORE INTO jmap_full_sync_stored(account_id,email_id) VALUES(?1,?2)",
                        params![account_id, remote_id],
                    )?;
                }
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
                if existing.is_none() && insert_after.is_some_and(|after| date < after) {
                    continue;
                }
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
                let is_automated = automated_email(&email);
                let references = email.references().unwrap_or_default().to_vec();
                let from = addresses(email.from()).into_iter().next();
                let to = addresses(email.to());
                let cc = addresses(email.cc());
                let bcc = addresses(email.bcc());
                // Servers may keep the body's line breaks in `preview`, which
                // the single-line mail list would stack over the row above.
                let snippet = crate::mime::make_snippet(email.preview().unwrap_or_default());
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
                let inserted = existing.is_none();
                let is_junk = folders.iter().any(|folder| {
                    matches!(folder.role.as_deref(), Some(roles::SPAM) | Some(roles::TRASH))
                });
                if inserted
                    && is_outgoing
                    && settings.collect_outgoing_contacts
                    && date >= outgoing_learning_since
                {
                    let mut harvested_addresses = HashSet::new();
                    for address in to.iter().chain(cc.iter()).chain(bcc.iter()) {
                        let email = address.email.to_ascii_lowercase();
                        if !account_emails.contains(&email) && harvested_addresses.insert(email) {
                            repo::contacts::harvest(&tx, account_id, address, true, date)?;
                        }
                    }
                } else if inserted
                    && !is_outgoing
                    && settings.collect_incoming_contacts
                    && date >= incoming_learning_since
                    && !is_junk
                    && !is_automated
                    && let Some(sender) = &from
                    && !crate::mime::robot_sender(&sender.email)
                {
                    repo::contacts::harvest(&tx, account_id, sender, false, date)?;
                }
                let mut pending_keywords = false;
                let mut pending_mailboxes = false;
                let local_id;

                if let Some(existing) = existing {
                    (pending_keywords, pending_mailboxes) =
                        repo::actions::jmap_reconciliation_guards(&tx, existing.id)?;
                    let old_thread_id = existing.thread_id;
                    if existing.jmap_blob_id != blob_id {
                        // These rows describe the previous content; the
                        // attachment listing describes the new blob.
                        tx.execute(
                            "DELETE FROM attachments WHERE message_id=?1",
                            params![existing.id],
                        )?;
                        if let Some(path) = existing.raw_path {
                            stale_paths.push(path);
                        }
                    }
                    // JMAP messages carry no IMAP UID; an adopted one drops
                    // the UID its earlier copy had in another folder.
                    tx.execute(
                        "UPDATE messages SET uid=NULL, thread_id=?2, folder_id=CASE WHEN ?21 THEN folder_id ELSE ?3 END,
                           message_id=?4, subject=?5, from_name=?6, from_addr=?7, to_json=?8,
                           cc_json=?9, bcc_json=?10, date=?11, internal_date=?11,
                           is_read=CASE WHEN ?22 THEN is_read ELSE ?12 END,
                           is_starred=CASE WHEN ?22 THEN is_starred ELSE ?13 END,
                           is_draft=?14, is_outgoing=?15, is_automated=?16,
                           has_attachments=?17, size=?18, snippet=?19, jmap_blob_id=?20,
                           body_state=CASE WHEN jmap_blob_id IS NOT ?20 THEN 'none' ELSE body_state END,
                           raw_path=CASE WHEN jmap_blob_id IS NOT ?20 THEN NULL ELSE raw_path END
                         WHERE id=?1",
                        params![
                            existing.id,
                            thread_id,
                            primary_folder.id,
                            message_id,
                            subject,
                            from.as_ref().and_then(|value| value.name.clone()),
                            from.as_ref().map(|value| value.email.clone()),
                            serde_json::to_string(&to)?,
                            serde_json::to_string(&cc)?,
                            serde_json::to_string(&bcc)?,
                            date,
                            is_read as i64,
                            is_starred as i64,
                            is_draft as i64,
                            is_outgoing as i64,
                            is_automated as i64,
                            email.has_attachment() as i64,
                            size,
                            snippet,
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
                        to,
                        cc,
                        bcc,
                        date,
                        internal_date: Some(date),
                        is_read,
                        is_starred,
                        is_draft,
                        is_outgoing,
                        is_automated,
                        has_attachments: email.has_attachment(),
                        size: Some(size),
                        snippet,
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
                if !pending_mailboxes {
                    repo::gmail::set_message_folders(&tx, local_id, &folder_ids)?;
                }
                if !pending_keywords {
                    repo::labels::reconcile_keywords(&tx, local_id, &keywords)?;
                }
                repo::search::index_message(&tx, local_id)?;
                changed_threads.insert(thread_id);
            }

            for thread_id in &changed_threads {
                repo::threads::recompute(&tx, *thread_id)?;
            }
            tx.commit()?;
            Ok((changed_threads.into_iter().collect::<Vec<_>>(), stale_paths))
        })
        .await?;
    publish_changes(ctx, thread_ids, stale_paths).await;
    Ok(())
}

/// Applies remote deletions and full-sync reconciliation before checkpointing.
/// Waiting for every header page keeps surviving threads and their snoozes alive.
async fn finish_email_sync(
    ctx: &SyncCtx,
    config: &AccountConfig,
    remote_ids: HashSet<String>,
    destroyed: Vec<String>,
    state: String,
    full: bool,
) -> Result<()> {
    let account_id = config.id;
    let cutoff = config.settings.mail_history.cutoff_ms_at(now_ms());
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
                // Mail an earlier server or protocol of the account left here
                // had this full sync to be matched with its copy on this
                // server. What wasn't matched is not on the account's server.
                for leftover in repo::messages::jmap_leftovers(&tx, account_id)? {
                    tx.execute(
                        "UPDATE pending_actions SET state='cancelled', finished_at=?2,
                           last_error='the message is not on the account''s server'
                         WHERE message_id=?1 AND state IN ('pending','inflight')",
                        params![leftover.id, now_ms()],
                    )?;
                    changed_threads.extend(leftover.thread_id);
                    stale_paths.extend(leftover.raw_path);
                    repo::messages::delete(&tx, leftover.id)?;
                }
                // Everything an earlier server or protocol left has now been
                // matched with its copy here or removed.
                tx.execute(
                    "UPDATE messages SET relink_pending=0 WHERE account_id=?1 AND relink_pending=1",
                    params![account_id],
                )?;
                changed_threads.extend(repo::folders::drop_jmap_leftovers(&tx, account_id)?);
            }
            for thread_id in &changed_threads {
                repo::threads::recompute(&tx, *thread_id)?;
            }
            tx.execute(
                "INSERT INTO jmap_sync_state(account_id,email_state,last_full_sync)
                 VALUES(?1,?2,CASE WHEN ?3 THEN ?4 ELSE NULL END)
                 ON CONFLICT(account_id) DO UPDATE SET email_state=excluded.email_state,
                   last_full_sync=CASE WHEN ?3 THEN ?4 ELSE jmap_sync_state.last_full_sync END,
                   full_sync_state=NULL",
                params![account_id, state, full as i64, now_ms()],
            )?;
            tx.execute(
                "DELETE FROM jmap_full_sync_stored WHERE account_id=?1",
                params![account_id],
            )?;
            tx.commit()?;
            Ok((changed_threads.into_iter().collect::<Vec<_>>(), stale_paths))
        })
        .await?;
    publish_changes(ctx, thread_ids, stale_paths).await;
    Ok(())
}

async fn publish_changes(ctx: &SyncCtx, thread_ids: Vec<i64>, stale_paths: Vec<String>) {
    for path in stale_paths {
        let _ = tokio::fs::remove_file(path).await;
    }
    if !thread_ids.is_empty() {
        ctx.bus.emit(CoreEvent::MailUpdated { thread_ids });
    }
}

/// Lists the attachments of the newest messages still waiting for it, as many
/// as one request covers, so Files can show them without downloading the
/// messages or changing $seen. Returns whether a request was made; the next
/// call finds out whether more messages wait.
///
/// Header sync leaves `attachments` out because Stalwart can only answer it by
/// reading each message in full. Here the reads are bounded per request by
/// `budget`, which halves whenever the server doesn't answer in time, and
/// never hold back mail sync.
async fn list_attachments(
    ctx: &SyncCtx,
    config: &AccountConfig,
    c: &ConnectedClient,
    budget: &mut i64,
) -> Result<bool> {
    let account_id = config.id;
    let limit = max_objects_in_get(c).min(ATTACHMENT_LISTING_EMAILS);
    let queue = ctx
        .db
        .read(move |conn| {
            repo::messages::jmap_attachment_listing_queue(conn, account_id, limit as i64)
        })
        .await?;
    let mut count = queue.len();
    loop {
        let mut bytes = 0_i64;
        let mut batch = Vec::new();
        for message in queue.iter().take(count) {
            let size = message.size.max(0);
            if !batch.is_empty() && bytes.saturating_add(size) > *budget {
                break;
            }
            bytes = bytes.saturating_add(size);
            batch.push(message.clone());
        }
        if batch.is_empty() {
            return Ok(false);
        }
        match request_attachments(c, &batch).await {
            Ok(listed) => {
                store_attachments(ctx, batch, listed).await?;
                return Ok(true);
            }
            Err(error) if client::is_timeout(&error) && batch.len() > 1 => {
                // Halving the count too guarantees progress when sizes are
                // unknown.
                count = batch.len() / 2;
                *budget = (bytes / 2).max(1);
                tracing::debug!(
                    account_id,
                    budget = *budget,
                    "JMAP attachment listing timed out; asking for fewer"
                );
            }
            Err(error) if client::is_timeout(&error) => {
                // The server can't read this one message in time. Rather than
                // retrying it forever, Files leaves it out until the message
                // is opened, which lists its attachments from the download.
                tracing::warn!(
                    account_id,
                    size = bytes,
                    "JMAP attachment listing of one message timed out; skipping it"
                );
                store_attachments(ctx, batch, Vec::new()).await?;
                return Ok(true);
            }
            Err(error) => return Err(client::map_error(error)),
        }
    }
}

async fn request_attachments(
    c: &ConnectedClient,
    batch: &[repo::messages::JmapAttachmentListing],
) -> jmap_client::Result<Vec<Email>> {
    let mut request = c.request();
    request
        .get_email()
        .account_id(&c.account_id)
        .ids(batch.iter().map(|message| message.remote_id.clone()))
        .properties([
            EmailProperty::Id,
            EmailProperty::BlobId,
            EmailProperty::Attachments,
        ])
        .arguments()
        .body_properties([
            BodyProperty::PartId,
            BodyProperty::BlobId,
            BodyProperty::Size,
            BodyProperty::Name,
            BodyProperty::Type,
            BodyProperty::Disposition,
            BodyProperty::Cid,
        ]);
    let mut response: EmailGetResponse = c.send_single(request).await?;
    Ok(response.take_list())
}

/// Stores what the server listed for `batch` and marks every message in it as
/// listed for its blob. Messages the server didn't list as they are here are
/// left to header sync, which queues them again if their blob changes.
async fn store_attachments(
    ctx: &SyncCtx,
    batch: Vec<repo::messages::JmapAttachmentListing>,
    listed: Vec<Email>,
) -> Result<()> {
    let thread_ids = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let mut changed_threads = HashSet::new();
            for message in &batch {
                let current = tx
                    .query_row(
                        "SELECT thread_id, body_state FROM messages
                         WHERE id=?1 AND jmap_id=?2 AND jmap_blob_id=?3",
                        params![message.message_id, message.remote_id, message.blob_id],
                        |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, String>(1)?)),
                    )
                    .optional()?;
                // A message that changed meanwhile comes back through the
                // queue with its new blob.
                let Some((thread_id, body_state)) = current else {
                    continue;
                };
                // Missing here or holding another blob on the server: header
                // sync reconciles the message, which queues it again if needed.
                // MIME-derived rows of a cached body stay authoritative.
                if let Some(email) = listed.iter().find(|email| {
                    email.id() == Some(message.remote_id.as_str())
                        && email.blob_id() == Some(message.blob_id.as_str())
                }) && body_state != "cached"
                {
                    persist_attachment_parts(
                        &tx,
                        message.message_id,
                        email.attachments().unwrap_or_default(),
                    )?;
                    changed_threads.extend(thread_id);
                }
                tx.execute(
                    "UPDATE messages SET jmap_attachments_blob_id=?2 WHERE id=?1",
                    params![message.message_id, message.blob_id],
                )?;
            }
            tx.commit()?;
            Ok(changed_threads.into_iter().collect::<Vec<_>>())
        })
        .await?;
    publish_changes(ctx, thread_ids, Vec::new()).await;
    Ok(())
}

fn max_objects_in_get(c: &ConnectedClient) -> usize {
    c.client
        .session()
        .core_capabilities()
        .map(|v| v.max_objects_in_get().clamp(1, 1000))
        .unwrap_or(256)
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
        "empty_trash" => {
            let folder_id = action.payload["folderId"]
                .as_i64()
                .ok_or_else(|| CoreError::Jmap("Trash purge has no mailbox".into()))?;
            let folder = ctx
                .db
                .read(move |conn| repo::folders::get(conn, folder_id))
                .await?
                .ok_or_else(|| CoreError::NotFound("Trash mailbox".into()))?;
            if folder.account_id != config.id || folder.role.as_deref() != Some(roles::TRASH) {
                return Err(CoreError::Jmap(
                    "Trash purge mailbox belongs to another account".into(),
                ));
            }
            let mailbox_id = folder
                .jmap_id
                .ok_or_else(|| CoreError::Jmap("Trash mailbox has no remote id".into()))?;
            let snapshot_ids = crate::sync::snapshot_trash_ids(ctx, config.id).await?;
            let ids = query_mailbox_ids(c, &mailbox_id).await?;
            let batch_size = c
                .client
                .session()
                .core_capabilities()
                .map_or(1, |caps| caps.max_objects_in_set().clamp(1, 500));
            for batch in ids.chunks(batch_size) {
                let mut request = c.request();
                request
                    .set_email()
                    .account_id(&c.account_id)
                    .destroy(batch.iter().cloned());
                let mut response: EmailSetResponse =
                    c.send_single(request).await.map_err(client::map_error)?;
                for id in batch {
                    match response.destroyed(id) {
                        Ok(()) => {}
                        Err(jmap_client::Error::Set(error))
                            if error.type_ == SetErrorType::NotFound => {}
                        Err(error) => return Err(client::map_error(error)),
                    }
                }
            }
            crate::sync::finish_empty_trash(ctx, config.id, snapshot_ids).await
        }
        "delete_permanently" => {
            if let Some(message_id) = action.message_id
                && crate::sync::is_local_only_draft(ctx, message_id).await?
            {
                return crate::sync::finish_permanent_delete(ctx, Some(message_id)).await;
            }
            if let Some(remote_id) = linked_or_retry(&remote_id)? {
                destroy_email_if_present(c, remote_id).await?;
            }
            crate::sync::finish_permanent_delete(ctx, action.message_id).await
        }
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
                c.email_set_keyword(remote_id, keyword, set).await,
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
                c.email_set_keyword(remote_id, keyword, action.kind == "add_label")
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
                c.email_set_mailboxes(remote_id, mailbox_ids).await,
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

async fn query_mailbox_ids(c: &ConnectedClient, mailbox_id: &str) -> Result<Vec<String>> {
    query_all("Trash query", |position| async move {
        let mut request = c.request();
        request
            .query_email()
            .account_id(&c.account_id)
            .position(position)
            .limit(PAGE)
            .calculate_total(true)
            .sort([jmap_client::email::query::Comparator::received_at().descending()])
            .filter(jmap_client::email::query::Filter::in_mailbox(mailbox_id));
        c.send_single::<QueryResponse>(request).await
    })
    .await
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
    match c.email_destroy(remote_id).await {
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
    let matches = c.email_query(filter).await.map_err(client::map_error)?;
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
    let (detail, reply_to, bcc, mut references, in_reply_to, stored_message_id, attachments) = ctx
        .db
        .read(move |conn| {
            let detail = repo::messages::detail(conn, draft_id)?;
            let reply_to =
                repo::sender_identities::get_verified(conn, detail.account_id, &detail.from.email)?
                    .and_then(|identity| identity.reply_to());
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
                reply_to,
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
    let domain = detail
        .from
        .email
        .rsplit_once('@')
        .map(|(_, value)| value)
        .unwrap_or("localhost");
    let outgoing = crate::mime::OutgoingMessage {
        from: detail.from.clone(),
        reply_to,
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
    let security_recipients = outgoing
        .to
        .iter()
        .chain(outgoing.cc)
        .chain(outgoing.bcc)
        .cloned()
        .collect();
    let (message_id, raw) = crate::mime::build_message_owned(outgoing)?;
    let raw =
        crate::mail_security::protect_draft(&ctx.db, config.id, draft_id, raw, security_recipients)
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
        .upload(raw, "message/rfc822")
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
    let mut request = c.request();
    let import = request
        .import_email()
        .account_id(&c.account_id)
        .email(blob_id)
        .mailbox_ids([draft_mailbox])
        .keywords(["$draft", "$seen"])
        .received_at(now_ms() / 1000);
    let create_id = import.create_id();
    let imported = c
        .send_single::<EmailImportResponse>(request)
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
            .email_submission_query(jmap_client::email_submission::query::Filter::email_ids([
                remote_email.clone(),
            ]))
            .await
            .map_err(client::map_error)?
            .ids()
            .is_empty();
    if !already_submitted {
        let sender_email = ctx
            .db
            .read(move |conn| Ok(repo::messages::detail(conn, draft_id)?.from.email))
            .await?;
        let mut identity_request = c.request();
        identity_request.get_identity().account_id(&c.account_id);
        let identities = c
            .send_single::<IdentityGetResponse>(identity_request)
            .await
            .map_err(client::map_error)?
            .take_list();
        // RFC 8621 §6: an exact identity is more specific than `*@domain`.
        let identity_id = identities
            .iter()
            .filter(|identity| {
                identity
                    .email()
                    .is_some_and(|email| identity_matches(email, &sender_email))
            })
            .min_by_key(|identity| {
                identity
                    .email()
                    .is_some_and(|email| email.starts_with("*@"))
            })
            .and_then(|identity| identity.id())
            .ok_or_else(|| {
                CoreError::Jmap(format!(
                    "server has no JMAP sending identity for {}",
                    sender_email
                ))
            })?
            .to_owned();
        let (request, create_id) =
            submission_request(c, &remote_email, identity_id, sent_mailbox.clone());
        let mut response = c
            .send_single::<EmailSubmissionSetResponse>(request)
            .await
            .map_err(map_submission_error)?;
        let created = response.created(&create_id);
        let sender_refused = matches!(
            &created,
            Err(jmap_client::Error::Set(error))
                if matches!(
                    error.error(),
                    jmap_client::core::set::SetErrorType::ForbiddenFrom
                        | jmap_client::core::set::SetErrorType::ForbiddenMailFrom
                )
        );
        let delivery = if created.is_ok() {
            SenderDeliveryStatus::Confirmed
        } else if sender_refused {
            SenderDeliveryStatus::Rejected
        } else {
            SenderDeliveryStatus::Unknown
        };
        if delivery != SenderDeliveryStatus::Unknown {
            let account_id = config.id;
            let email = sender_email.clone();
            let server_reply = created.as_ref().err().map(ToString::to_string);
            // Best effort: never turn an accepted submission into a retry.
            if let Err(error) = ctx
                .db
                .write(move |conn| {
                    repo::sender_identities::record_delivery(
                        conn,
                        account_id,
                        &email,
                        delivery,
                        server_reply.as_deref(),
                    )
                })
                .await
            {
                tracing::warn!(account_id, %error, "could not record sender delivery outcome");
            }
        }
        if sender_refused {
            return Err(CoreError::Jmap(format!(
                "the server refused to send as {sender_email}; add the address as a sending identity on the server, then refresh sender identities"
            )));
        }
        created.map_err(map_submission_error)?;
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
    let account_id = config.id;
    let sent_at = now_ms();
    let (thread_id, staged_paths) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE messages SET is_draft=0,is_outgoing=1,is_read=1,
                  folder_id=?2,date=?3 WHERE id=?1",
                params![draft_id, sent_folder_id, sent_at],
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
            repo::contacts::record_sent_recipients(&tx, account_id, draft_id, sent_at)?;
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
    let mut request = c.request();
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
    let mut request = c.request();
    request
        .set_email()
        .account_id(&c.account_id)
        .update(remote_email)
        .mailbox_ids([sent_mailbox])
        .keyword("$draft", false)
        .keyword("$seen", true);
    let mut response: EmailSetResponse = c.send_single(request).await.map_err(client::map_error)?;
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
    let mut session = None;
    while let Some(command) = rx.recv().await {
        match command {
            PriorityFetchCmd::Body(message_id) => {
                if let Err(error) = fetch_body(&ctx, &config, &mut session, message_id).await {
                    session = None;
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
                let result = fetch_attachment(&ctx, &config, &mut session, attachment_id)
                    .await
                    .map_err(|error| {
                        session = None;
                        error.to_string()
                    });
                let _ = complete.send(result);
            }
        }
    }
}

/// The body worker's session: connected when first needed, and again after a
/// failure or once the server reports a changed Session.
async fn body_session<'a>(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &'a mut Option<ConnectedClient>,
) -> Result<&'a ConnectedClient> {
    if session
        .as_ref()
        .is_none_or(ConnectedClient::session_changed)
    {
        *session = Some(connect(ctx, config).await?);
    }
    Ok(session.as_ref().expect("the session was connected above"))
}

async fn fetch_body(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Option<ConnectedClient>,
    message_id: i64,
) -> Result<()> {
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
    let connected = body_session(ctx, config, session).await?;
    let raw = download_message_bounded(connected, &expected_blob).await?;
    let parsed = crate::mime::parse_message(&raw)?;
    let calendar_events = crate::sync::ingest::calendar_events(&parsed);
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
            crate::sync::ingest::store_body(&tx, message_id, &path_string, &parsed)?;
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
    crate::sync::ingest::store_calendar_events(ctx, config.id, message_id, calendar_events).await?;
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
    let response = c.get(url).send().await.map_err(|error| {
        CoreError::Network(format!(
            "JMAP message download failed: {}",
            client::transport_failure(&error)
        ))
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
            CoreError::Network(format!(
                "JMAP message download failed: {}",
                client::transport_failure(&error)
            ))
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
    session: &mut Option<ConnectedClient>,
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
        let connected = body_session(ctx, config, session).await?;
        return download_message_bounded(connected, &blob).await;
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
            fetch_body(ctx, config, session, message_id).await?;
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
#[path = "sync_tests.rs"]
mod sync_tests;

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
    fn attachment_listing_preserves_identity_and_invalidates_changed_blobs() {
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
