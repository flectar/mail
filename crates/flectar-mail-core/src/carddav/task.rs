//! Per-account CardDAV polling task.

use super::{
    http::{DavAuth, HttpTransport, Transport},
    sync::sync_account,
};
use crate::{
    accounts::credentials::{self, CredentialStoreHandle, Slot},
    db::{Db, repo},
    error::{CoreError, Result},
    events::EventBus,
    models::{CardDavKind, Provider},
    net::proxy::Owner,
    oauth::tokens::TokenProvider,
};
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
    tokens: TokenProvider,
    account_id: i64,
) -> CardDavTaskHandle {
    let (tx, mut rx) = mpsc::channel(1);
    let task = tokio::spawn(async move {
        // Connection setup has just discovered collections, and persisted
        // startup configurations are immediately usable. Refresh later
        // without delaying the first contact sync behind another discovery.
        let mut last_collection_refresh_ms = crate::models::now_ms();
        loop {
            let delay = crate::sync::engine::configured_sync_interval(&db).await;
            let alive = tokio::select! { message = rx.recv() => message.is_some(), _ = tokio::time::sleep(delay) => true };
            if !alive {
                break;
            }
            while rx.try_recv().is_ok() {}
            let refresh_collections = crate::models::now_ms() - last_collection_refresh_ms
                >= COLLECTION_REFRESH_SECS * 1_000;
            let result = run_cycle(
                &db,
                &bus,
                credentials.clone(),
                &tokens,
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

/// A transport signed in for one cycle: an app password for a CardDAV
/// server, or a fresh access token for Google Contacts.
async fn sign_in(
    config: &repo::carddav::CardDavConfig,
    credentials: CredentialStoreHandle,
    tokens: &TokenProvider,
) -> Result<Box<dyn Transport>> {
    let owner = Owner::Account(config.account_id);
    let auth = match config.kind {
        CardDavKind::Generic => DavAuth::Basic(
            config.username.clone(),
            credentials::load_async(credentials, config.account_id, Slot::CarddavPassword).await?,
        ),
        CardDavKind::Google => DavAuth::Bearer(
            tokens
                .access_token(config.account_id, Provider::Gmail)
                .await?,
        ),
    };
    // Google's refusals read the same whichever way the connection signs in.
    Ok(if super::google::is_google(&config.base_url) {
        Box::new(super::google::transport(owner, auth, &config.base_url)?)
    } else {
        Box::new(HttpTransport::new(owner, auth, &config.base_url)?)
    })
}

async fn run_cycle(
    db: &Db,
    bus: &EventBus,
    credentials: CredentialStoreHandle,
    tokens: &TokenProvider,
    account_id: i64,
    refresh_collections: bool,
) -> Result<()> {
    let config = db
        .read(move |conn| repo::carddav::get_config(conn, account_id))
        .await?;
    let Some(config) = config.filter(|config| config.enabled) else {
        return Ok(());
    };
    let transport = sign_in(&config, credentials, tokens).await?;
    if refresh_collections {
        let discovered =
            super::discovery::discover_for(config.kind, transport.as_ref(), &config.base_url)
                .await?;
        let books = discovered.addressbooks;
        let refreshed = repo::carddav::CardDavConfig {
            principal_url: discovered.principal_url,
            home_set_url: discovered.home_set_url,
            last_error: None,
            ..config
        };
        db.write(move |conn| {
            let tx = conn.transaction()?;
            repo::carddav::save_discovery(&tx, &refreshed, &books)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    }
    sync_account(db, bus, transport.as_ref(), account_id)
        .await
        .map(|_| ())
}
