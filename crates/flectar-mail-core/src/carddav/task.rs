//! Per-account CardDAV polling task.

use super::{http::HttpTransport, sync::sync_account};
use crate::{
    accounts::credentials::{self, CredentialStoreHandle, Slot},
    db::{Db, repo},
    error::{CoreError, Result},
    events::EventBus,
};
use std::collections::HashSet;
use tokio::sync::mpsc;

const COLLECTION_REFRESH_SECS: i64 = 6 * 60 * 60;

pub struct CardDavTaskHandle {
    pub account_id: i64,
    tx: mpsc::Sender<()>,
    abort: tokio::task::AbortHandle,
}
impl CardDavTaskHandle {
    pub fn nudge(&self) {
        let _ = self.tx.try_send(());
    }

    pub fn abort(&self) {
        self.abort.abort();
    }
}

pub fn spawn(
    db: Db,
    bus: EventBus,
    credentials: CredentialStoreHandle,
    account_id: i64,
) -> CardDavTaskHandle {
    let (tx, mut rx) = mpsc::channel(1);
    let task = tokio::spawn(async move {
        // Connection setup has just discovered collections, and persisted
        // startup configurations are immediately usable. Refresh later
        // without delaying the first contact sync behind another discovery.
        let mut last_collection_refresh_ms = crate::models::now_ms();
        loop {
            bus.wait_for_platform_active().await;
            let delay = crate::sync::engine::configured_sync_interval(&db).await;
            let alive = tokio::select! { message = rx.recv() => message.is_some(), _ = tokio::time::sleep(delay) => true };
            if !alive {
                break;
            }
            while rx.try_recv().is_ok() {}
            bus.wait_for_platform_active().await;
            let refresh_collections = crate::models::now_ms() - last_collection_refresh_ms
                >= COLLECTION_REFRESH_SECS * 1_000;
            let result = run_cycle(
                &db,
                &bus,
                credentials.clone(),
                account_id,
                refresh_collections,
            )
            .await;
            if result.is_ok() && refresh_collections {
                last_collection_refresh_ms = crate::models::now_ms();
            }
            let error = result.err().map(|error| match error {
                CoreError::NeedsReauth => "needs_reauth".into(),
                other => {
                    tracing::warn!(account_id, %other, "CardDAV cycle failed");
                    other.to_string()
                }
            });
            let _ = db
                .write(move |conn| repo::carddav::set_error(conn, account_id, error.as_deref()))
                .await;
        }
    });
    CardDavTaskHandle {
        account_id,
        tx,
        abort: task.abort_handle(),
    }
}

async fn run_cycle(
    db: &Db,
    bus: &EventBus,
    credentials: CredentialStoreHandle,
    account_id: i64,
    refresh_collections: bool,
) -> Result<()> {
    let config = db
        .read(move |conn| repo::carddav::get_config(conn, account_id))
        .await?;
    let Some(config) = config.filter(|config| config.enabled) else {
        return Ok(());
    };
    let password = credentials::load_async(credentials, account_id, Slot::CarddavPassword).await?;
    let transport = HttpTransport::new(config.username.clone(), password, &config.base_url)?;
    if refresh_collections {
        let discovered = super::discovery::discover(&transport, &config.base_url).await?;
        let urls = discovered
            .addressbooks
            .iter()
            .map(|book| book.url.clone())
            .collect::<HashSet<_>>();
        let books = discovered.addressbooks;
        let refreshed = repo::carddav::CardDavConfig {
            principal_url: discovered.principal_url,
            home_set_url: discovered.home_set_url,
            last_error: None,
            ..config
        };
        db.write(move |conn| {
            let tx = conn.transaction()?;
            repo::carddav::upsert_config(&tx, &refreshed)?;
            let mut first = None;
            for book in books {
                let id = repo::carddav::upsert_addressbook(
                    &tx,
                    account_id,
                    &book.url,
                    book.display_name.as_deref(),
                    book.read_only,
                )?;
                first.get_or_insert(id);
            }
            repo::carddav::retain_addressbooks(&tx, account_id, &urls)?;
            if let Some(id) = first {
                repo::carddav::ensure_default(&tx, account_id, id)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    }
    sync_account(db, bus, &transport, account_id)
        .await
        .map(|_| ())
}
