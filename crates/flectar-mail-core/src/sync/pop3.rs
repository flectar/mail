//! POP3 accounts. One worker per account downloads new mail into the local
//! Inbox and completes every other action on this device.
//!
//! A session holds the server's lock on the mailbox, so sessions stay short
//! and other mail apps get their turn. A session ends with QUIT only after
//! everything it retrieved is on stable storage, because QUIT lets a server
//! that expires retrieved mail (EXPIRE 0) delete it, and removes the mail the
//! account's setting no longer keeps on the server. Anything that goes wrong
//! before then drops the connection instead, which leaves the mailbox as it
//! was.

use super::engine::{
    PriorityFetchCmd, RestoreRequest, SyncCmd, SyncCtx, configured_sync_interval, set_state,
    set_state_error,
};
use super::ingest::{self, Delivery, Importer};
use crate::accounts::credentials::{self, Slot};
use crate::db::repo::{
    self,
    pop3::{KnownUidl, UidlState},
};
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use crate::mime::ParsedBody;
use crate::models::{
    AccountConfig, AccountSettings, ConnectionSecurity, KeepOnServer, now_ms, roles,
};
use crate::pop3::{self, SignInRefusal};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

/// The longest one session downloads before it lets go of the mailbox.
const SESSION_BUDGET: Duration = Duration::from_secs(90);
/// The most messages one session downloads.
const SESSION_MESSAGES: usize = 250;
/// The most messages one session removes from the server, and the longest
/// it spends on that after downloading.
const SESSION_REMOVALS: usize = 500;
const REMOVAL_BUDGET: Duration = Duration::from_secs(30);
/// How soon the next session continues work that reached a budget.
const CONTINUE_AFTER: Duration = Duration::from_secs(15);
/// Header sections read at once to date messages against the mail history.
const DATE_BATCH: usize = 32;
const MAX_HEADER_BYTES: usize = 256 * 1024;
/// A sign-in refused without a reason is tried once more this much later
/// before the password is in doubt.
const UNEXPLAINED_RETRY: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// A message the server refused to send is tried again after this, doubling
/// with each refusal up to a day.
const REFUSED_RETRY_MS: i64 = 15 * 60 * 1000;
const MAX_REFUSED_RETRY_MS: i64 = 24 * 60 * 60 * 1000;
/// Finished actions are kept this long for undo history.
const ACTION_HISTORY_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const MISSING_ORIGINAL: &str = "the original of this message is missing from this device, and the mail server no longer has it";

/// Everything a POP3 account worker listens to.
pub(crate) struct Inputs {
    pub commands: mpsc::Receiver<SyncCmd>,
    pub reads: mpsc::Receiver<PriorityFetchCmd>,
    pub restores: mpsc::Receiver<RestoreRequest>,
    pub settings: watch::Receiver<AccountSettings>,
}

pub(crate) async fn run(ctx: SyncCtx, mut config: AccountConfig, mut inputs: Inputs) {
    let account_id = config.id;
    let mut waiters: Vec<oneshot::Sender<std::result::Result<(), String>>> = Vec::new();
    // None while the server refuses the password; only a manual check or a
    // password change tries again.
    let mut next_sync = Some(Instant::now());
    let mut backoff = Duration::from_secs(1);
    let mut unexplained_refusal = false;
    loop {
        if let Err(error) = complete_actions(&ctx, &config).await {
            tracing::warn!(account_id, %error, "completing local POP3 actions failed");
        }
        if next_sync.is_some_and(|at| at <= Instant::now()) {
            next_sync = match sync(&ctx, &config).await {
                Ok(synced) => {
                    backoff = Duration::from_secs(1);
                    unexplained_refusal = false;
                    set_state(&ctx, account_id, "idle").await;
                    ctx.bus.emit(CoreEvent::NetworkState { online: true });
                    let next = match synced {
                        Synced::Complete => {
                            finish(&mut waiters, Ok(()));
                            Instant::now() + configured_sync_interval(&ctx.db).await
                        }
                        Synced::Partial => {
                            finish(&mut waiters, Ok(()));
                            Instant::now() + CONTINUE_AFTER
                        }
                        Synced::NotBefore(at) => {
                            finish(&mut waiters, Err(wait_message(at)));
                            instant_at(at)
                        }
                    };
                    Some(next)
                }
                Err(Failure::Unexplained(message)) if !unexplained_refusal => {
                    unexplained_refusal = true;
                    set_state_error(&ctx, account_id, "offline", &message).await;
                    finish(&mut waiters, Err(message));
                    Some(Instant::now() + UNEXPLAINED_RETRY)
                }
                Err(Failure::Credentials(message) | Failure::Unexplained(message)) => {
                    tracing::warn!(account_id, %message, "POP3 sign-in refused");
                    set_state(&ctx, account_id, "needs_reauth").await;
                    finish(&mut waiters, Err(message));
                    None
                }
                Err(Failure::Busy(message)) => {
                    set_state_error(&ctx, account_id, "offline", &message).await;
                    finish(&mut waiters, Err(message));
                    Some(Instant::now() + configured_sync_interval(&ctx.db).await)
                }
                Err(Failure::Other(error)) => {
                    tracing::warn!(account_id, %error, "POP3 sync failed");
                    let message = error.to_string();
                    set_state_error(&ctx, account_id, "offline", &message).await;
                    ctx.bus.emit(CoreEvent::NetworkState { online: false });
                    finish(&mut waiters, Err(message));
                    let retry = Instant::now() + backoff;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    Some(retry)
                }
            };
            next_sync = respect_login_delay(&ctx, account_id, next_sync).await;
        }

        let wake = async {
            match next_sync {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            command = inputs.commands.recv() => match command {
                None | Some(SyncCmd::Shutdown) => return,
                Some(SyncCmd::SyncNow { complete }) => {
                    waiters.extend(complete);
                    next_sync = Some(Instant::now());
                }
                // Bodies are stored with each message.
                Some(SyncCmd::RunActions | SyncCmd::FetchBody { .. }) => {}
            },
            read = inputs.reads.recv() => match read {
                None => return,
                Some(PriorityFetchCmd::Body(_)) => {}
                Some(PriorityFetchCmd::Attachment { complete, .. }) => {
                    let _ = complete.send(Err(
                        "attachments of POP3 mail are read from the stored message".into(),
                    ));
                }
            },
            request = inputs.restores.recv() => match request {
                None => return,
                Some(request) => {
                    let result = restore(&ctx, &config, request.message_id)
                        .await
                        .map_err(|error| error.to_string());
                    let _ = request.complete.send(result);
                }
            },
            changed = inputs.settings.changed() => {
                if changed.is_err() {
                    return;
                }
                config.settings = inputs.settings.borrow_and_update().clone();
                if next_sync.is_some() {
                    next_sync = Some(Instant::now());
                }
            },
            () = wake => {}
        }
    }
}

fn finish(
    waiters: &mut Vec<oneshot::Sender<std::result::Result<(), String>>>,
    result: std::result::Result<(), String>,
) {
    for waiter in waiters.drain(..) {
        let _ = waiter.send(result.clone());
    }
}

fn instant_at(at_ms: i64) -> Instant {
    Instant::now() + Duration::from_millis(u64::try_from(at_ms - now_ms()).unwrap_or(0))
}

fn wait_message(at_ms: i64) -> String {
    let minutes = u64::try_from(at_ms - now_ms())
        .unwrap_or(0)
        .div_ceil(60_000)
        .max(1);
    format!("the mail server allows checking for mail again in {minutes} min")
}

/// Never plan a sign-in before the server's minimum time between sign-ins
/// (RFC 2449 LOGIN-DELAY) has passed.
async fn respect_login_delay(
    ctx: &SyncCtx,
    account_id: i64,
    next_sync: Option<Instant>,
) -> Option<Instant> {
    let next_sync = next_sync?;
    let allowed = ctx
        .db
        .read(move |conn| repo::pop3::sync_state(conn, account_id))
        .await
        .ok()
        .and_then(|state| state.next_sign_in_at());
    Some(match allowed {
        Some(at) => next_sync.max(instant_at(at)),
        None => next_sync,
    })
}

/// Finish queued actions. For mail on this device nothing waits on a server.
async fn complete_actions(ctx: &SyncCtx, config: &AccountConfig) -> Result<()> {
    while crate::queue::execute_due_on_device(ctx, config)
        .await?
        .due_remaining
    {}
    ctx.db
        .write(|conn| repo::actions::gc(conn, now_ms() - ACTION_HISTORY_MS))
        .await
}

/// What a failed sign-in or session means for the account.
#[derive(Debug)]
enum Failure {
    /// The server refused the password, or it can't be sent.
    Credentials(String),
    /// The server refused without saying why, which may be the password.
    Unexplained(String),
    /// The server works but can't be used right now.
    Busy(String),
    Other(CoreError),
}

impl From<CoreError> for Failure {
    fn from(error: CoreError) -> Self {
        Self::Other(error)
    }
}

impl Failure {
    /// The error for callers that can't retry a sign-in later, such as
    /// account setup.
    fn into_error(self) -> CoreError {
        match self {
            Self::Credentials(message) | Self::Unexplained(message) => CoreError::Auth(message),
            Self::Busy(message) => CoreError::Pop3(message),
            Self::Other(error) => error,
        }
    }
}

fn failure(config: &AccountConfig, error: pop3::Error) -> Failure {
    let host = &config.incoming_host;
    let port = config.incoming_port;
    match error {
        pop3::Error::SignIn { refusal, .. } => {
            let message = format!("POP3 sign-in to {host}: {error}");
            match refusal {
                SignInRefusal::Credentials => Failure::Credentials(message),
                SignInRefusal::Unspecified => Failure::Unexplained(message),
                SignInRefusal::InUse => Failure::Busy(format!(
                    "another app is using the mailbox on {host}; checking again later"
                )),
                SignInRefusal::LoginDelay => Failure::Busy(format!(
                    "{host} limits how often mail can be checked; checking again later"
                )),
                SignInRefusal::Temporary => Failure::Other(CoreError::Pop3(message)),
                SignInRefusal::Permanent => Failure::Other(CoreError::Pop3(format!(
                    "{message}. Contact your mail provider."
                ))),
            }
        }
        pop3::Error::UnsendableCredentials => Failure::Credentials(error.to_string()),
        pop3::Error::NoPasswordSignIn => Failure::Other(CoreError::Pop3(
            match config.settings.connection.incoming_security {
                ConnectionSecurity::None => format!(
                    "{host} accepts passwords only over an encrypted connection. Choose STARTTLS or SSL/TLS."
                ),
                ConnectionSecurity::Tls | ConnectionSecurity::Starttls => {
                    format!("{host} accepts no password sign-in")
                }
            },
        )),
        pop3::Error::Tls(message) => Failure::Other(CoreError::Tls(format!(
            "POP3 {host}:{port}: {message}. Check SSL/TLS versus STARTTLS and use the hostname on the server certificate."
        ))),
        pop3::Error::Refused {
            command: "STLS", ..
        } => Failure::Other(CoreError::Tls(format!(
            "POP3 {host}:{port}: {error}. Check the server port and security mode."
        ))),
        error => Failure::Other(CoreError::Pop3(format!("{host}:{port}: {error}"))),
    }
}

fn no_unique_ids(config: &AccountConfig) -> Failure {
    Failure::Other(CoreError::Pop3(format!(
        "{} doesn't support UIDL, which downloading each message only once depends on",
        config.incoming_host
    )))
}

async fn load_password(
    ctx: &SyncCtx,
    config: &AccountConfig,
) -> std::result::Result<String, Failure> {
    credentials::load_async(ctx.credentials.clone(), config.id, Slot::Password)
        .await
        .map_err(|error| match error {
            CoreError::Auth(message) => Failure::Credentials(message),
            CoreError::NeedsReauth => Failure::Credentials(error.to_string()),
            error => Failure::Other(error),
        })
}

async fn connect(
    config: &AccountConfig,
    password: &str,
) -> std::result::Result<pop3::Session, Failure> {
    let connection = &config.settings.connection;
    let server = pop3::Server {
        host: &config.incoming_host,
        port: config.incoming_port,
        security: connection.incoming_security,
        trusted_certificate_pem: &connection.trusted_certificate_pem,
    };
    pop3::Session::connect(&server, &config.username, password)
        .await
        .map_err(|error| failure(config, error))
}

/// What a sign-in during account setup learned about the server.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Probe {
    pub signed_in_at: i64,
    pub login_delay: Option<Duration>,
    pub expire: Option<pop3::Expire>,
}

/// Sign in and make sure the server can tell its messages apart, which
/// downloading each message once depends on.
pub(crate) async fn probe(config: &AccountConfig, password: &str) -> Result<Probe> {
    let mut session = connect(config, password)
        .await
        .map_err(Failure::into_error)?;
    let signed_in_at = now_ms();
    let capabilities = session.capabilities().clone();
    if !capabilities.announced {
        match session.unique_ids().await {
            Ok(_) => {}
            Err(pop3::Error::Refused { .. }) => return Err(no_unique_ids(config).into_error()),
            Err(error) => return Err(failure(config, error).into_error()),
        }
    } else if !capabilities.uidl {
        return Err(no_unique_ids(config).into_error());
    }
    // Nothing was retrieved, so QUIT changes nothing on the server.
    if let Err(error) = session.quit().await {
        tracing::debug!(%error, "POP3 QUIT after the setup check failed");
    }
    Ok(Probe {
        signed_in_at,
        login_delay: capabilities.login_delay,
        expire: capabilities.expire,
    })
}

/// How a sync ended.
#[derive(Debug)]
enum Synced {
    /// Every listed message is accounted for.
    Complete,
    /// The session's budget ran out with messages left to download or
    /// remove.
    Partial,
    /// The server accepts no sign-in before this time.
    NotBefore(i64),
}

async fn sync(ctx: &SyncCtx, config: &AccountConfig) -> std::result::Result<Synced, Failure> {
    let account_id = config.id;
    let state = ctx
        .db
        .read(move |conn| repo::pop3::sync_state(conn, account_id))
        .await?;
    if let Some(at) = state.next_sign_in_at()
        && at > now_ms()
    {
        return Ok(Synced::NotBefore(at));
    }
    let password = load_password(ctx, config).await?;
    let mut session = connect(config, &password).await?;
    set_state(ctx, account_id, "syncing").await;
    let signed_in_at = now_ms();
    let capabilities = session.capabilities().clone();
    ctx.db
        .write(move |conn| {
            repo::pop3::record_sign_in(
                conn,
                account_id,
                signed_in_at,
                capabilities.login_delay,
                capabilities.expire,
            )
        })
        .await?;

    let listing = list(&mut session, config).await?;
    let known = ctx
        .db
        .read(move |conn| repo::pop3::known_uidls(conn, account_id))
        .await?;
    let cutoff = config.settings.mail_history.cutoff_ms_at(signed_in_at);
    let planned = plan(&listing, &known, cutoff, signed_in_at);
    let listed: HashSet<String> = listing.iter().map(|message| message.uidl.clone()).collect();
    ctx.db
        .write(move |conn| {
            let listed = listed.iter().map(String::as_str).collect();
            repo::pop3::reconcile_listing(conn, account_id, &known, &listed, signed_in_at)
        })
        .await?;

    // Mail found by the first complete download is history, not news.
    let live = state.first_synced_at.is_some();
    let downloaded = download(ctx, config, &mut session, planned, cutoff, live).await?;
    if downloaded == Downloaded::All {
        ctx.db
            .write(move |conn| repo::pop3::record_first_sync(conn, account_id, now_ms()))
            .await?;
    }
    let removed = match downloaded {
        Downloaded::All | Downloaded::Partly => remove(ctx, config, &mut session, &listing).await?,
        Downloaded::Ended => false,
    };
    // Everything retrieved is stored, so the server may now act on the
    // session and remove what it was asked to. After a cut-short response
    // the client sends nothing more.
    if let Err(error) = session.quit().await {
        tracing::debug!(account_id, %error, "POP3 QUIT failed");
    }
    Ok(if downloaded == Downloaded::All && removed {
        Synced::Complete
    } else {
        Synced::Partial
    })
}

/// A message in the server's listing.
struct Listed {
    number: u32,
    uidl: String,
    size: u64,
}

async fn list(
    session: &mut pop3::Session,
    config: &AccountConfig,
) -> std::result::Result<Vec<Listed>, Failure> {
    let capabilities = session.capabilities();
    if capabilities.announced && !capabilities.uidl {
        return Err(no_unique_ids(config));
    }
    let ids = match session.unique_ids().await {
        Ok(ids) => ids,
        Err(pop3::Error::Refused { .. }) => return Err(no_unique_ids(config)),
        Err(error) => return Err(failure(config, error)),
    };
    let sizes: HashMap<u32, u64> = session
        .sizes()
        .await
        .map_err(|error| failure(config, error))?
        .into_iter()
        .collect();
    Ok(ids
        .into_iter()
        .map(|(number, uidl)| Listed {
            number,
            size: sizes.get(&number).copied().unwrap_or(0),
            uidl,
        })
        .collect())
}

/// A listed message to download.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    number: u32,
    uidl: String,
    size: u64,
    /// The message's date, when already known.
    sent_at: Option<i64>,
    attempts: i64,
}

/// The listed messages to download, newest first. A server numbers its
/// messages oldest first. It must not repeat a UIDL; if one does, only the
/// newest message under it is downloaded.
fn plan(
    listing: &[Listed],
    known: &HashMap<String, KnownUidl>,
    cutoff: Option<i64>,
    now: i64,
) -> Vec<Candidate> {
    let mut newest_first: Vec<&Listed> = listing.iter().collect();
    newest_first.sort_by_key(|message| std::cmp::Reverse(message.number));
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for message in newest_first {
        if !seen.insert(message.uidl.as_str()) {
            tracing::warn!(
                number = message.number,
                "POP3 server listed a UIDL twice; keeping its newest message"
            );
            continue;
        }
        let (sent_at, attempts) = match known.get(&message.uidl) {
            None => (None, 0),
            Some(entry) => {
                let due = match entry.state {
                    UidlState::Stored | UidlState::Matched => false,
                    // Reconsidered once the mail history reaches back to it.
                    UidlState::Skipped => {
                        cutoff.is_none_or(|cutoff| entry.sent_at.is_none_or(|at| at >= cutoff))
                    }
                    UidlState::Failed => entry.retry_at.is_some_and(|at| at <= now),
                };
                if !due {
                    continue;
                }
                (entry.sent_at, entry.attempts)
            }
        };
        candidates.push(Candidate {
            number: message.number,
            uidl: message.uidl.clone(),
            size: message.size,
            sent_at,
            attempts,
        });
    }
    candidates
}

/// How far a session's download got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Downloaded {
    /// Every planned message was dealt with.
    All,
    /// The session's budget ran out first.
    Partly,
    /// The session can't continue.
    Ended,
}

/// Download planned messages until the session's budget runs out.
async fn download(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut pop3::Session,
    candidates: Vec<Candidate>,
    cutoff: Option<i64>,
    live: bool,
) -> std::result::Result<Downloaded, Failure> {
    let account_id = config.id;
    let deadline = Instant::now() + SESSION_BUDGET;
    // Without TOP a message's date is only known once it is retrieved, and
    // a retrieved message must be kept, so the mail history can't apply.
    let capabilities = session.capabilities();
    let cutoff = cutoff.filter(|_| capabilities.top || !capabilities.announced);
    let mut downloaded = 0;
    for batch in candidates.chunks(DATE_BATCH) {
        let dates = match cutoff {
            Some(_) if Instant::now() < deadline => read_dates(session, config, batch).await?,
            _ => vec![None; batch.len()],
        };
        for (candidate, date) in batch.iter().zip(dates) {
            if Instant::now() >= deadline || downloaded >= SESSION_MESSAGES {
                return Ok(Downloaded::Partly);
            }
            if let Some(cutoff) = cutoff
                && let Some(sent_at) = candidate.sent_at.or(date)
                && sent_at < cutoff
            {
                let uidl = candidate.uidl.clone();
                let size = candidate.size;
                ctx.db
                    .write(move |conn| {
                        repo::pop3::record_skipped(conn, account_id, &uidl, size, sent_at)
                    })
                    .await?;
                continue;
            }
            match retrieve(ctx, config, session, candidate, live).await? {
                Retrieved::Stored => downloaded += 1,
                Retrieved::Deferred => {}
                Retrieved::SessionEnded => return Ok(Downloaded::Ended),
            }
        }
    }
    Ok(Downloaded::All)
}

/// The dates of the batch's messages that don't have one yet, from their
/// header sections.
async fn read_dates(
    session: &mut pop3::Session,
    config: &AccountConfig,
    batch: &[Candidate],
) -> std::result::Result<Vec<Option<i64>>, Failure> {
    let wanted: Vec<u32> = batch
        .iter()
        .filter(|candidate| candidate.sent_at.is_none())
        .map(|candidate| candidate.number)
        .collect();
    if wanted.is_empty() {
        return Ok(vec![None; batch.len()]);
    }
    let sections = session
        .headers(&wanted, MAX_HEADER_BYTES)
        .await
        .map_err(|error| failure(config, error))?;
    let mut dates: HashMap<u32, Option<i64>> = wanted
        .into_iter()
        .zip(sections)
        .map(|(number, section)| {
            let date = section
                .and_then(|bytes| crate::mime::parse_header_block(&bytes).ok())
                .and_then(|headers| headers.date_ms);
            (number, date)
        })
        .collect();
    Ok(batch
        .iter()
        .map(|candidate| dates.remove(&candidate.number).flatten())
        .collect())
}

enum Retrieved {
    Stored,
    /// Recorded as failed, to be tried again later or never.
    Deferred,
    /// The session can't continue; it ends without QUIT.
    SessionEnded,
}

/// Retrieve one message and store it. A message the server refuses, or one
/// over the size limit, is recorded and skipped. Any other failure ends the
/// session before anything retrieved could be lost.
async fn retrieve(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut pop3::Session,
    candidate: &Candidate,
    live: bool,
) -> std::result::Result<Retrieved, Failure> {
    let limit = crate::MAX_CACHED_MESSAGE_BYTES;
    let too_large = format!(
        "the message is larger than the {} MiB limit",
        limit / (1024 * 1024)
    );
    if candidate.size > limit as u64 {
        record_failure(ctx, config.id, candidate, &too_large, None).await?;
        return Ok(Retrieved::Deferred);
    }
    let raw = match session.retrieve(candidate.number, limit).await {
        Ok(raw) => raw,
        Err(error @ pop3::Error::Refused { .. }) => {
            let delay = REFUSED_RETRY_MS
                .saturating_mul(1 << candidate.attempts.clamp(0, 10))
                .min(MAX_REFUSED_RETRY_MS);
            let retry_at = now_ms().saturating_add(delay);
            record_failure(
                ctx,
                config.id,
                candidate,
                &error.to_string(),
                Some(retry_at),
            )
            .await?;
            return Ok(Retrieved::Deferred);
        }
        Err(pop3::Error::TooLarge(_)) => {
            record_failure(ctx, config.id, candidate, &too_large, None).await?;
            return Ok(Retrieved::SessionEnded);
        }
        Err(error) => return Err(failure(config, error)),
    };
    store(ctx, config.id, candidate, raw, live)
        .await
        .map_err(|error| {
            Failure::Other(CoreError::Other(format!(
                "couldn't save downloaded mail: {error}"
            )))
        })?;
    Ok(Retrieved::Stored)
}

async fn record_failure(
    ctx: &SyncCtx,
    account_id: i64,
    candidate: &Candidate,
    error: &str,
    retry_at: Option<i64>,
) -> Result<()> {
    let uidl = candidate.uidl.clone();
    let size = candidate.size;
    let error = error.to_owned();
    ctx.db
        .write(move |conn| {
            repo::pop3::record_failed(conn, account_id, &uidl, size, &error, retry_at)
        })
        .await
}

/// Mark the mail the account's setting no longer keeps on the server. The
/// server removes it at QUIT and keeps it if the session ends any other way.
/// Returns whether everything due was marked.
async fn remove(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut pop3::Session,
    listing: &[Listed],
) -> std::result::Result<bool, Failure> {
    let keep = config.settings.keep_on_server;
    if keep == KeepOnServer::Always {
        return Ok(true);
    }
    let account_id = config.id;
    let now = now_ms();
    let due = ctx
        .db
        .read(move |conn| repo::pop3::removable(conn, account_id, keep, now))
        .await?;
    // A UIDL the server repeats doesn't tell its messages apart, and one
    // listed with another size than it was downloaded with may name another
    // message now. Neither is removed.
    let mut listed: HashMap<&str, Option<&Listed>> = HashMap::new();
    for message in listing {
        listed
            .entry(message.uidl.as_str())
            .and_modify(|entry| *entry = None)
            .or_insert(Some(message));
    }
    let deadline = Instant::now() + REMOVAL_BUDGET;
    let mut marked = 0;
    for message in due {
        let Some(Some(server)) = listed.get(message.uidl.as_str()) else {
            continue;
        };
        if server.size != message.size
            || (!message.deleted && !original_exists(message.original.as_deref()).await)
        {
            continue;
        }
        if marked == SESSION_REMOVALS || Instant::now() >= deadline {
            return Ok(false);
        }
        match session.delete(server.number).await {
            Ok(()) => marked += 1,
            Err(error @ pop3::Error::Refused { .. }) => {
                tracing::debug!(account_id, %error, "the POP3 server kept a message it was asked to remove");
            }
            Err(error) => return Err(failure(config, error)),
        }
    }
    Ok(true)
}

/// Whether a message's original is on this device, so that the server's
/// copy isn't the only one.
async fn original_exists(path: Option<&str>) -> bool {
    match path {
        Some(path) => tokio::fs::metadata(path)
            .await
            .is_ok_and(|metadata| metadata.is_file()),
        None => false,
    }
}

/// How the file names of downloaded POP3 originals end.
pub(crate) const ORIGINAL_SUFFIX: &str = ".pop3.eml";

/// A file name for a message's original. Names don't derive from the UIDL,
/// which servers may reuse once a message is gone.
fn new_original_path(ctx: &SyncCtx, account_id: i64) -> std::path::PathBuf {
    ctx.paths
        .mail_dir(account_id)
        .join(format!("{:032x}{ORIGINAL_SUFFIX}", rand::random::<u128>()))
}

/// Keep a retrieved message: its original on stable storage first, then one
/// durable transaction that makes it part of the account.
async fn store(
    ctx: &SyncCtx,
    account_id: i64,
    candidate: &Candidate,
    raw: Vec<u8>,
    live: bool,
) -> Result<()> {
    let (parsed, raw) = tokio::task::spawn_blocking(move || (parse(&raw), raw))
        .await
        .map_err(|error| CoreError::Other(error.to_string()))?;
    let path = new_original_path(ctx, account_id);
    crate::file_io::write_durable(&path, &raw, "POP3 message").await?;
    drop(raw);
    let calendar_events = ingest::calendar_events(&parsed);
    let path_string = path.to_string_lossy().into_owned();
    let uidl = candidate.uidl.clone();
    let size = candidate.size;
    let committed = ctx
        .db
        .write_durable(move |tx| {
            let now = now_ms();
            let sent_at = parsed.headers.date_ms;
            // A message the account already has stays where it is, such as
            // the Sent copy of mail the user sent to themselves.
            if let Some(message_id) = parsed.headers.message_id.as_deref()
                && let Some(existing) = repo::messages::by_message_id(tx, account_id, message_id)?
            {
                repo::pop3::record_matched(tx, account_id, &uidl, size, sent_at, existing.id, now)?;
                return Ok(None);
            }
            let inbox = repo::folders::by_role(tx, account_id, roles::INBOX)?
                .ok_or_else(|| CoreError::NotFound("POP3 Inbox".into()))?;
            let mut importer = Importer::new(tx, account_id)?;
            let imported = importer.insert(
                tx,
                &parsed.headers,
                &Delivery {
                    folder_id: inbox.id,
                    folder_role: Some(roles::INBOX),
                    uid: None,
                    received_ms: None,
                    seen: false,
                    flagged: false,
                    draft: false,
                    has_attachments: parsed
                        .attachments
                        .iter()
                        .any(|attachment| !attachment.is_inline),
                    size: i64::try_from(size).ok(),
                },
            )?;
            ingest::store_body(tx, imported.message_id, &path_string, &parsed)?;
            repo::threads::recompute(tx, imported.thread_id)?;
            importer.learn_contacts(tx, &parsed.headers, &imported)?;
            importer.announce(tx, &imported, live)?;
            let message_id = imported.message_id;
            let (thread_ids, fresh) = importer.finish(tx)?;
            repo::pop3::record_stored(tx, account_id, &uidl, size, sent_at, message_id, now)?;
            Ok(Some((message_id, thread_ids, fresh)))
        })
        .await;
    let (message_id, thread_ids, fresh) = match committed {
        Ok(Some(inserted)) => inserted,
        Ok(None) => {
            let _ = tokio::fs::remove_file(&path).await;
            return Ok(());
        }
        Err(error) => {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(error);
        }
    };
    if let Err(error) =
        ingest::store_calendar_events(ctx, account_id, message_id, calendar_events).await
    {
        tracing::warn!(account_id, message_id, %error, "could not record a POP3 message's invitations");
    }
    ctx.bus.emit(CoreEvent::MailUpdated { thread_ids });
    if !fresh.is_empty() {
        ctx.bus.emit(CoreEvent::MailNew {
            account_id,
            thread_ids: fresh,
        });
    }
    Ok(())
}

/// Every retrieved message is kept: one that can't be parsed is stored
/// whole and shown as text.
fn parse(raw: &[u8]) -> ParsedBody {
    crate::mime::parse_message(raw).unwrap_or_else(|error| {
        tracing::warn!(%error, "storing an unparseable POP3 message as text");
        let text = String::from_utf8_lossy(raw).into_owned();
        ParsedBody {
            snippet: crate::mime::make_body_snippet(Some(&text), None),
            text: Some(text),
            ..ParsedBody::default()
        }
    })
}

/// Download a stored message's original again after its file went missing
/// from this device. Returns the new file's path.
async fn restore(ctx: &SyncCtx, config: &AccountConfig, message_id: i64) -> Result<String> {
    let account_id = config.id;
    let uidl = ctx
        .db
        .read(move |conn| repo::pop3::uidl_of_message(conn, message_id))
        .await?
        .ok_or_else(|| CoreError::NotFound(MISSING_ORIGINAL.into()))?;
    let state = ctx
        .db
        .read(move |conn| repo::pop3::sync_state(conn, account_id))
        .await?;
    if let Some(at) = state.next_sign_in_at()
        && at > now_ms()
    {
        return Err(CoreError::Pop3(wait_message(at)));
    }
    let password = load_password(ctx, config)
        .await
        .map_err(Failure::into_error)?;
    let mut session = connect(config, &password)
        .await
        .map_err(Failure::into_error)?;
    let signed_in_at = now_ms();
    let capabilities = session.capabilities().clone();
    ctx.db
        .write(move |conn| {
            repo::pop3::record_sign_in(
                conn,
                account_id,
                signed_in_at,
                capabilities.login_delay,
                capabilities.expire,
            )
        })
        .await?;
    let number = session
        .unique_ids()
        .await
        .map_err(|error| failure(config, error).into_error())?
        .into_iter()
        .find_map(|(number, id)| (id == uidl).then_some(number))
        .ok_or_else(|| CoreError::NotFound(MISSING_ORIGINAL.into()))?;
    let raw = session
        .retrieve(number, crate::MAX_CACHED_MESSAGE_BYTES)
        .await
        .map_err(|error| failure(config, error).into_error())?;
    let path = new_original_path(ctx, account_id);
    crate::file_io::write_durable(&path, &raw, "POP3 message").await?;
    let path_string = path.to_string_lossy().into_owned();
    let new_path = path_string.clone();
    let replaced = ctx
        .db
        .write_durable(move |tx| {
            let previous = repo::messages::get_row(tx, message_id)?
                .ok_or_else(|| CoreError::NotFound("message".into()))?
                .raw_path;
            tx.execute(
                "UPDATE messages SET raw_path = ?2 WHERE id = ?1",
                rusqlite::params![message_id, new_path],
            )?;
            Ok(previous)
        })
        .await;
    match replaced {
        Ok(Some(previous)) if previous != path_string => {
            let _ = tokio::fs::remove_file(previous).await;
        }
        Ok(_) => {}
        Err(error) => {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(error);
        }
    }
    if let Err(error) = session.quit().await {
        tracing::debug!(account_id, %error, "POP3 QUIT after restoring a message failed");
    }
    Ok(path_string)
}

#[cfg(test)]
#[path = "pop3_tests.rs"]
mod tests;
