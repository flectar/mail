//! Per-account CalDAV task: an HTTP-only loop separate from the IMAP actor
//! (a slow calendar server must never stall mail sync). Wakes on the selected
//! sync cadence or when nudged (local mutation queued, "sync now", due-check).

use tokio::sync::mpsc;

use crate::accounts::credentials::{self, CredentialStoreHandle, Slot};
use crate::db::Db;
use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::events::EventBus;
use crate::models::{Provider, now_ms};
use crate::oauth::tokens::TokenProvider;

use super::http::{DavAuth, HttpTransport};
use super::sync::sync_account;

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
        let mut last_collection_refresh_ms = 0;
        loop {
            bus.wait_for_platform_active().await;
            let poll_delay = crate::sync::engine::configured_sync_interval(&mail_db).await;
            let woke = tokio::select! {
                m = rx.recv() => m.is_some(),
                _ = tokio::time::sleep(poll_delay) => true,
            };
            if !woke {
                break; // handle dropped: account removed / calendar disconnected
            }
            // Drain coalesced nudges.
            while rx.try_recv().is_ok() {}
            bus.wait_for_platform_active().await;

            let refresh_collections =
                now_ms() - last_collection_refresh_ms >= COLLECTION_REFRESH_SECS * 1_000;
            match run_cycle(
                &db,
                &mail_db,
                &bus,
                &tokens,
                credentials.clone(),
                account_id,
                refresh_collections,
            )
            .await
            {
                Ok(()) => {
                    if refresh_collections {
                        last_collection_refresh_ms = now_ms();
                    }
                }
                Err(CoreError::NeedsReauth) => {
                    let _ = db
                        .write(move |conn| {
                            repo::caldav::set_config_error(conn, account_id, Some("needs_reauth"))
                        })
                        .await;
                }
                Err(e) => {
                    tracing::warn!("caldav cycle for account {account_id} failed: {e}");
                    let msg = e.to_string();
                    let _ = db
                        .write(move |conn| {
                            repo::caldav::set_config_error(conn, account_id, Some(&msg))
                        })
                        .await;
                }
            }
        }
        tracing::debug!("caldav task for account {account_id} stopped");
    });
    handle
}

/// Resolve the account's transport from its config (Bearer for Google via the
/// OAuth token cache, Basic for generic servers via the keyring).
pub async fn transport_for(
    db: &Db,
    tokens: &TokenProvider,
    credential_store: CredentialStoreHandle,
    account_id: i64,
) -> Result<Option<HttpTransport>> {
    let cfg = db
        .read(move |conn| repo::caldav::get_config(conn, account_id))
        .await?;
    let Some(cfg) = cfg.filter(|c| c.enabled) else {
        return Ok(None);
    };
    let auth = match cfg.kind.as_str() {
        "google" => {
            let token = tokens.access_token(account_id, Provider::Gmail).await?;
            DavAuth::Bearer(token)
        }
        _ => {
            let user = cfg.username.clone().unwrap_or_default();
            let pass =
                credentials::load_async(credential_store, account_id, Slot::CaldavPassword).await?;
            DavAuth::Basic(user, pass)
        }
    };
    Ok(Some(HttpTransport::new(auth, &cfg.base_url)?))
}

async fn run_cycle(
    db: &Db,
    mail_db: &Db,
    bus: &EventBus,
    tokens: &TokenProvider,
    credential_store: CredentialStoreHandle,
    account_id: i64,
    refresh_collections: bool,
) -> Result<()> {
    let cfg = db
        .read(move |conn| repo::caldav::get_config(conn, account_id))
        .await?;
    let Some(cfg) = cfg.filter(|c| c.enabled) else {
        return Ok(());
    };

    // Claim due cal_% actions up front; sync_account's push-dirty pass is
    // what actually performs them (actions are nudges carrying intent).
    let now = now_ms();
    let claimed: Vec<(i64, i64)> = db
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

    // Microsoft calendars sync over Graph (no CalDAV endpoint exists);
    // everything else goes through the DAV transport.
    let result = if cfg.kind == "microsoft" {
        crate::graphcal::sync_account(db, bus, tokens, account_id).await
    } else if cfg.kind == "google" {
        let token = tokens.access_token(account_id, Provider::Gmail).await?;
        if refresh_collections {
            let discovered = crate::googlecal::list_calendars(&token).await?;
            db.write(move |conn| {
                crate::googlecal::reconcile_calendars(conn, account_id, &discovered).map(|_| ())
            })
            .await?;
            bus.emit(crate::events::CoreEvent::CalendarUpdated { account_id });
        }
        let transport = HttpTransport::new(DavAuth::Bearer(token), &cfg.base_url)?;
        let self_email = mail_db
            .read(move |conn| Ok(repo::accounts::get(conn, account_id)?.map(|a| a.email)))
            .await?
            .unwrap_or_default();
        sync_account(db, bus, &transport, account_id, &self_email).await
    } else {
        let self_email = mail_db
            .read(move |conn| Ok(repo::accounts::get(conn, account_id)?.map(|a| a.email)))
            .await?
            .unwrap_or_default();
        match transport_for(db, tokens, credential_store, account_id).await? {
            Some(transport) => sync_account(db, bus, &transport, account_id, &self_email).await,
            None => return Ok(()),
        }
    };

    match &result {
        Ok(_) => {
            db.write(move |conn| {
                for (id, _) in &claimed {
                    repo::actions::set_state(conn, *id, "done", None)?;
                }
                repo::caldav::set_config_error(conn, account_id, None)?;
                Ok(())
            })
            .await?;
        }
        Err(e) => {
            let msg = e.to_string();
            db.write(move |conn| {
                for (id, attempts) in &claimed {
                    if *attempts + 1 >= MAX_ATTEMPTS {
                        repo::actions::set_state(conn, *id, "failed", Some(&msg))?;
                    } else {
                        let backoff = 60_000 * (1 << (*attempts).min(6));
                        repo::actions::bump_attempt(conn, *id, now_ms() + backoff, &msg)?;
                    }
                }
                Ok(())
            })
            .await?;
        }
    }
    result.map(|_| ())
}
