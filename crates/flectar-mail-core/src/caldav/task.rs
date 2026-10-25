//! Per-account calendar task: an HTTP-only loop separate from the IMAP actor
//! (a slow calendar server must never stall mail sync). Each cycle serves
//! every enabled connection of the account side by side, each with its own
//! sign-in and error state, so one failing server never holds back another.
//! Wakes on the selected sync cadence or when nudged (local mutation queued,
//! "sync now", due-check).

use std::collections::HashMap;

use tokio::sync::mpsc;

use crate::accounts::credentials::{self, CredentialStoreHandle, Slot};
use crate::db::Db;
use crate::db::repo;
use crate::db::repo::caldav::ConnectionConfig;
use crate::error::{CoreError, Result};
use crate::events::{CoreEvent, EventBus};
use crate::models::{CalendarConnectionKind, Provider, now_ms};
use crate::oauth::tokens::TokenProvider;

use super::http::{DavAuth, HttpTransport};
use super::route::PendingPush;
use super::sync::{Synced, sync_connection};

const COLLECTION_REFRESH_SECS: i64 = 6 * 60 * 60;
const MAX_ATTEMPTS: i64 = 8;

#[derive(Clone)]
pub struct CalTaskHandle {
    pub account_id: i64,
    tx: mpsc::Sender<()>,
}

impl CalTaskHandle {
    pub fn nudge(&self) {
        // Nudges are idempotent; one pending wake represents every newer one.
        let _ = self.tx.try_send(());
    }
}

pub fn spawn(
    db: Db,
    mail_db: Db,
    bus: EventBus,
    tokens: TokenProvider,
    credentials: CredentialStoreHandle,
    account_id: i64,
) -> CalTaskHandle {
    let (tx, mut rx) = mpsc::channel::<()>(1);
    let handle = CalTaskHandle { account_id, tx };
    tokio::spawn(async move {
        // When each Google connection last refreshed its calendar list.
        let mut lists_refreshed: HashMap<i64, i64> = HashMap::new();
        loop {
            let poll_delay = crate::sync::engine::configured_sync_interval(&mail_db).await;
            let woke = tokio::select! {
                m = rx.recv() => m.is_some(),
                _ = tokio::time::sleep(poll_delay) => true,
            };
            if !woke {
                break; // handle dropped: account removed / calendars paused
            }
            // Drain coalesced nudges.
            while rx.try_recv().is_ok() {}

            let cycle = Cycle {
                db: &db,
                mail_db: &mail_db,
                bus: &bus,
                tokens: &tokens,
                credentials: &credentials,
                account_id,
            };
            if let Err(e) = cycle.run(&mut lists_refreshed).await {
                tracing::warn!("calendar cycle for account {account_id} failed: {e}");
            }
        }
        tracing::debug!("calendar task for account {account_id} stopped");
    });
    handle
}

/// A connection signed in for one cycle.
enum Session {
    /// CalDAV. Google connections also carry the token their calendar list
    /// refresh needs.
    Dav {
        transport: HttpTransport,
        google_token: Option<String>,
    },
    /// Microsoft Graph.
    Graph { token: String },
}

async fn sign_in(
    db: &Db,
    tokens: &TokenProvider,
    credentials: &CredentialStoreHandle,
    cfg: &ConnectionConfig,
) -> Result<Session> {
    match cfg.kind {
        CalendarConnectionKind::Google => {
            let token = tokens.access_token(cfg.account_id, Provider::Gmail).await?;
            Ok(Session::Dav {
                transport: HttpTransport::new(DavAuth::Bearer(token.clone()), &cfg.base_url)?,
                google_token: Some(token),
            })
        }
        CalendarConnectionKind::Generic => {
            let password = connection_password(db, credentials.clone(), cfg).await?;
            Ok(Session::Dav {
                transport: HttpTransport::new(
                    DavAuth::Basic(cfg.username.clone(), password),
                    &cfg.base_url,
                )?,
                google_token: None,
            })
        }
        CalendarConnectionKind::Microsoft => {
            let token = tokens
                .access_token_for_scope(
                    cfg.account_id,
                    Provider::Microsoft,
                    crate::oauth::providers::MS_CALENDARS_SCOPE,
                )
                .await?;
            Ok(Session::Graph { token })
        }
    }
}

/// A CalDAV connection's app password. One saved before an account could
/// sync several servers still sits under the account; the first read moves
/// it to the connection's own entry.
pub async fn connection_password(
    db: &Db,
    store: CredentialStoreHandle,
    cfg: &ConnectionConfig,
) -> Result<String> {
    move_secret(db, store.clone(), cfg.id, cfg.account_id).await?;
    credentials::load_async(store, cfg.account_id, Slot::CaldavPassword(cfg.id)).await
}

/// Re-file a connection's password from its account's pre-connection
/// keyring entry, when it still waits there. Safe to interrupt: the new
/// entry is written before the old one is deleted, and the pending mark
/// goes last.
pub async fn move_secret(
    db: &Db,
    store: CredentialStoreHandle,
    connection_id: i64,
    account_id: i64,
) -> Result<()> {
    let pending = db
        .read(move |conn| repo::caldav::secret_move_pending(conn, connection_id))
        .await?;
    if !pending {
        return Ok(());
    }
    match credentials::load_async(store.clone(), account_id, Slot::AccountCaldavPassword).await {
        Ok(password) if !password.is_empty() => {
            credentials::store_async(
                store.clone(),
                account_id,
                Slot::CaldavPassword(connection_id),
                password,
            )
            .await?;
            credentials::delete_async(store, account_id, Slot::AccountCaldavPassword).await?;
        }
        // Nothing saved: the connection asks for a new password.
        Ok(_) | Err(CoreError::Auth(_)) => {}
        Err(error) => return Err(error),
    }
    db.write(move |conn| repo::caldav::finish_secret_move(conn, connection_id))
        .await
}

/// Save a CalDAV connection's new app password. A password still waiting
/// under the account from before is now stale and goes.
pub async fn save_password(
    db: &Db,
    store: CredentialStoreHandle,
    connection_id: i64,
    account_id: i64,
    password: String,
) -> Result<()> {
    credentials::store_async(
        store.clone(),
        account_id,
        Slot::CaldavPassword(connection_id),
        password,
    )
    .await?;
    let pending = db
        .read(move |conn| repo::caldav::secret_move_pending(conn, connection_id))
        .await?;
    if pending {
        credentials::delete_async(store, account_id, Slot::AccountCaldavPassword).await?;
        db.write(move |conn| repo::caldav::finish_secret_move(conn, connection_id))
            .await?;
    }
    Ok(())
}

/// How a failed connection reads in account settings: stable codes for the
/// states the interface explains, the error text otherwise.
pub fn error_text(error: &CoreError) -> String {
    match error {
        CoreError::NeedsReauth | CoreError::Offline => error.code().to_owned(),
        _ => error.to_string(),
    }
}

struct Cycle<'a> {
    db: &'a Db,
    mail_db: &'a Db,
    bus: &'a EventBus,
    tokens: &'a TokenProvider,
    credentials: &'a CredentialStoreHandle,
    account_id: i64,
}

impl Cycle<'_> {
    async fn run(&self, lists_refreshed: &mut HashMap<i64, i64>) -> Result<()> {
        let account_id = self.account_id;
        let connections: Vec<ConnectionConfig> = self
            .db
            .read(move |conn| repo::caldav::account_connections(conn, account_id))
            .await?
            .into_iter()
            .filter(|connection| connection.enabled)
            .collect();
        if connections.is_empty() {
            return Ok(());
        }

        // Claim due cal_% actions up front; the routed pushes below are what
        // actually perform them (actions are nudges carrying intent).
        let now = now_ms();
        let claimed: Vec<(i64, i64)> = self
            .db
            .write(move |conn| {
                let due = repo::actions::due_calendar(conn, account_id, now, 50)?;
                let mut out = Vec::new();
                for a in due {
                    if repo::actions::try_claim(conn, a.id)? {
                        out.push((a.id, a.attempts));
                    }
                }
                Ok(out)
            })
            .await?;

        let self_email = self
            .mail_db
            .read(move |conn| Ok(repo::accounts::get(conn, account_id)?.map(|a| a.email)))
            .await?
            .unwrap_or_default();

        // A connection that cannot sign in records why and sits this cycle
        // out; the others go ahead. Settings shows each connection's state,
        // so a change to one is announced like a calendar change.
        let mut all_synced = true;
        let mut states_changed = false;
        let mut sessions = Vec::new();
        for cfg in connections {
            match sign_in(self.db, self.tokens, self.credentials, &cfg).await {
                Ok(session) => sessions.push((cfg, session)),
                Err(error) => {
                    all_synced = false;
                    tracing::warn!("calendar connection {} cannot sign in: {error}", cfg.id);
                    states_changed |= self.record(&cfg, Some(error_text(&error))).await?;
                }
            }
        }

        // One snapshot routes every pending change to exactly one connection.
        let mut routes = self
            .db
            .read(move |conn| super::route::pending(conn, account_id))
            .await?;
        if !routes.discard.is_empty() {
            let discard = std::mem::take(&mut routes.discard);
            self.db
                .write(move |conn| {
                    for event_id in &discard {
                        repo::calendar::hard_delete(conn, *event_id)?;
                    }
                    Ok(())
                })
                .await?;
        }

        let work: Vec<(ConnectionConfig, Session, Vec<PendingPush>)> = sessions
            .into_iter()
            .map(|(cfg, session)| {
                let pushes = routes.pushes.remove(&cfg.id).unwrap_or_default();
                (cfg, session, pushes)
            })
            .collect();
        let refresh_due = |connection_id: i64| {
            now - lists_refreshed.get(&connection_id).copied().unwrap_or(0)
                >= COLLECTION_REFRESH_SECS * 1_000
        };
        let outcomes = futures::future::join_all(work.iter().map(|(cfg, session, pushes)| {
            self.sync_connection(cfg, session, pushes, &self_email, refresh_due(cfg.id))
        }))
        .await;

        let mut changed = false;
        let mut new_events = Vec::new();
        for ((cfg, _, _), outcome) in work.iter().zip(outcomes) {
            match outcome {
                Ok((synced, refreshed_list)) => {
                    if refreshed_list {
                        lists_refreshed.insert(cfg.id, now);
                    }
                    changed |= synced.changed;
                    let remaining =
                        super::sync::MAX_NEW_EVENT_NOTIFICATIONS.saturating_sub(new_events.len());
                    new_events.extend(synced.new_events.into_iter().take(remaining));
                    states_changed |= self.record(cfg, synced.calendar_error).await?;
                }
                Err(error) => {
                    all_synced = false;
                    tracing::warn!("calendar connection {} failed: {error}", cfg.id);
                    states_changed |= self.record(cfg, Some(error_text(&error))).await?;
                }
            }
        }

        if changed || states_changed {
            self.bus.emit(CoreEvent::CalendarUpdated { account_id });
        }
        if !new_events.is_empty() {
            self.bus.emit(CoreEvent::CalendarEventsAdded {
                account_id,
                events: new_events,
            });
        }

        let failure = (!all_synced).then(|| "a calendar connection did not sync".to_owned());
        self.db
            .write(move |conn| {
                for (id, attempts) in &claimed {
                    match &failure {
                        None => repo::actions::set_state(conn, *id, "done", None)?,
                        Some(message) if *attempts + 1 >= MAX_ATTEMPTS => {
                            repo::actions::set_state(conn, *id, "failed", Some(message))?
                        }
                        Some(message) => {
                            let backoff = 60_000 * (1 << (*attempts).min(6));
                            repo::actions::bump_attempt(conn, *id, now_ms() + backoff, message)?
                        }
                    }
                }
                Ok(())
            })
            .await
    }

    /// One connection's pushes and pull. Also reports whether a Google
    /// calendar list was refreshed.
    async fn sync_connection(
        &self,
        cfg: &ConnectionConfig,
        session: &Session,
        pushes: &[PendingPush],
        self_email: &str,
        refresh_list: bool,
    ) -> Result<(Synced, bool)> {
        match session {
            Session::Dav {
                transport,
                google_token,
            } => {
                let mut refreshed = false;
                let mut list_changed = false;
                if let Some(token) = google_token.as_deref().filter(|_| refresh_list) {
                    let listed = crate::googlecal::list_calendars(token).await?;
                    // An empty answer is never a reason to forget every
                    // calendar.
                    if !listed.is_empty() {
                        let connection = cfg.clone();
                        self.db
                            .write(move |conn| {
                                crate::googlecal::reconcile_calendars(conn, &connection, &listed)
                                    .map(|_| ())
                            })
                            .await?;
                        list_changed = true;
                    }
                    refreshed = true;
                }
                let mut synced = sync_connection(
                    self.db,
                    self.bus,
                    transport,
                    cfg.account_id,
                    cfg.id,
                    self_email,
                    pushes,
                )
                .await?;
                synced.changed |= list_changed;
                Ok((synced, refreshed))
            }
            Session::Graph { token } => Ok((
                crate::graphcal::sync_connection(self.db, token, cfg.account_id, cfg.id, pushes)
                    .await?,
                false,
            )),
        }
    }

    /// Store the connection's state after this cycle; true when it changed.
    async fn record(&self, cfg: &ConnectionConfig, error: Option<String>) -> Result<bool> {
        if error == cfg.last_error {
            return Ok(false);
        }
        let connection_id = cfg.id;
        self.db
            .write(move |conn| {
                repo::caldav::set_connection_error(conn, connection_id, error.as_deref())
            })
            .await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::credentials::CredentialStore;
    use crate::db::repo::caldav::tests::add_connection;

    #[derive(Default)]
    struct MemoryStore(std::sync::Mutex<HashMap<(i64, Slot), String>>);

    impl CredentialStore for MemoryStore {
        fn store(&self, account_id: i64, slot: Slot, secret: &str) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert((account_id, slot), secret.into());
            Ok(())
        }

        fn load(&self, account_id: i64, slot: Slot) -> Result<String> {
            self.0
                .lock()
                .unwrap()
                .get(&(account_id, slot))
                .cloned()
                .ok_or_else(|| CoreError::Auth("no stored credential".into()))
        }

        fn delete(&self, account_id: i64, slot: Slot) -> Result<()> {
            self.0.lock().unwrap().remove(&(account_id, slot));
            Ok(())
        }

        fn delete_all(&self, account_id: i64) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .retain(|(id, _), _| *id != account_id);
            Ok(())
        }
    }

    #[tokio::test]
    async fn an_account_password_moves_to_its_connection_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_calendar(&dir.path().join("calendar.db")).unwrap();
        let (moved, empty) = db
            .write(|conn| {
                let moved = add_connection(conn, 4, CalendarConnectionKind::Generic);
                let empty = add_connection(conn, 5, CalendarConnectionKind::Generic);
                conn.execute(
                    "INSERT INTO calendar_secret_moves (connection_id) VALUES (?1), (?2)",
                    rusqlite::params![moved.id, empty.id],
                )?;
                Ok((moved, empty))
            })
            .await
            .unwrap();
        let memory = std::sync::Arc::new(MemoryStore::default());
        let store: CredentialStoreHandle = memory.clone();
        memory
            .store(4, Slot::AccountCaldavPassword, "app-password")
            .unwrap();

        assert_eq!(
            connection_password(&db, store.clone(), &moved)
                .await
                .unwrap(),
            "app-password"
        );
        assert!(memory.load(4, Slot::AccountCaldavPassword).is_err());
        // Later reads use the connection's own entry only.
        memory
            .store(4, Slot::AccountCaldavPassword, "unrelated")
            .unwrap();
        assert_eq!(
            connection_password(&db, store.clone(), &moved)
                .await
                .unwrap(),
            "app-password"
        );

        // Nothing was saved for the other account: its move still finishes
        // and the connection reports the missing password.
        assert!(matches!(
            connection_password(&db, store, &empty).await,
            Err(CoreError::Auth(_))
        ));
        let pending = db
            .read(|conn| repo::caldav::pending_secret_moves(conn))
            .await
            .unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn interface_states_keep_stable_codes() {
        assert_eq!(error_text(&CoreError::NeedsReauth), "needs_reauth");
        assert_eq!(error_text(&CoreError::Offline), "offline");
        assert_eq!(
            error_text(&CoreError::CalDav("PROPFIND: HTTP 500".into())),
            "caldav error: PROPFIND: HTTP 500"
        );
    }
}
