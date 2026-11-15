//! Files sent by link: uploaded to a storage service the user connects, such as a Nextcloud
//! server, and shared there by a public link that a message carries in place of the file.

mod nextcloud;
#[cfg(test)]
mod tests;

use crate::Core;
use crate::accounts::credentials::{self, Slot};
use crate::db::repo;
use crate::error::{CoreError, Result};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The folder on the service that files sent by link go in, each in a folder of its own so
/// that its name stays as it was.
const FOLDER: &str = "Mail attachments";
/// The largest file that goes by link.
pub const MAX_LINKED_FILE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// Characters a generated password takes, without ones that read alike.
const PASSWORD_CHARACTERS: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

fn err(message: &str) -> CoreError {
    CoreError::Other(message.to_owned())
}

/// `length` random letters and digits.
fn random_token(length: usize) -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..length)
        .map(|_| char::from(PASSWORD_CHARACTERS[rng.random_range(0..PASSWORD_CHARACTERS.len())]))
        .collect()
}

/// A storage service files are sent by link from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkService {
    pub id: i64,
    /// The server, as `https://cloud.example.com/`.
    pub server_url: String,
    /// The name the user signs in to the server with.
    pub login_name: String,
    /// What the server calls itself, or its host.
    pub display_name: String,
}

/// A sign-in started in the browser, which [`Core::finish_link_service_login`] waits for.
pub struct LinkLogin {
    /// The page the user signs in on, in their browser.
    pub login_url: String,
    login: nextcloud::Login,
}

/// A file shared by link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedLink {
    pub service_id: i64,
    pub url: String,
    pub name: String,
    pub size: u64,
    /// The day the link stops working, as `YYYY-MM-DD`, if it does.
    pub expires_on: Option<String>,
    /// The password the link asks for, for the sender to pass on another way.
    pub password: Option<String>,
    /// The share on the server, and the folder the file is in, to take them back.
    pub share_id: String,
    pub folder: String,
}

fn service_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LinkService> {
    Ok(LinkService {
        id: row.get(0)?,
        server_url: row.get(1)?,
        login_name: row.get(2)?,
        display_name: row.get(3)?,
    })
}

/// The day a link made `days` days from `today` stops working, where it does: as Settings
/// says, or as soon as the server says when it limits links, and on the server's longest
/// when Settings has none.
fn expiry_date(today: chrono::NaiveDate, days: u32, max_days: Option<u32>) -> Option<String> {
    let days = match (days, max_days) {
        (0, None) => return None,
        (0, Some(max)) => max,
        (days, Some(max)) => days.min(max),
        (days, None) => days,
    };
    let date = today.checked_add_days(chrono::Days::new(u64::from(days)))?;
    Some(date.format("%Y-%m-%d").to_string())
}

impl Core {
    /// The storage services files go by link from.
    pub async fn link_services(&self) -> Result<Vec<LinkService>> {
        self.db
            .read(|conn| {
                let mut statement = conn.prepare(
                    "SELECT id, server_url, login_name, display_name FROM link_services ORDER BY id",
                )?;
                let services = statement
                    .query_map([], service_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(services)
            })
            .await
    }

    async fn link_service(&self, id: i64) -> Result<LinkService> {
        self.db
            .read(move |conn| {
                Ok(conn
                    .query_row(
                        "SELECT id, server_url, login_name, display_name FROM link_services
                         WHERE id = ?1",
                        params![id],
                        service_from_row,
                    )
                    .optional()?)
            })
            .await?
            .ok_or_else(|| err("The storage service was removed."))
    }

    /// Starts signing in to the server at `server_url` in the browser, which gives the app a
    /// password of its own there.
    pub async fn start_link_service_login(&self, server_url: &str) -> Result<LinkLogin> {
        let base = nextcloud::base_url(server_url)?;
        let login = nextcloud::start_login(&base).await?;
        Ok(LinkLogin {
            login_url: login.login_url.clone(),
            login,
        })
    }

    /// Waits for the sign-in in the browser to finish, as long as the server keeps it, then
    /// keeps the service.
    pub async fn finish_link_service_login(&self, login: LinkLogin) -> Result<LinkService> {
        let grant = nextcloud::finish_login(&login.login).await?;
        self.add_link_service(&grant.server, &grant.login_name, &grant.app_password)
            .await
    }

    /// Keeps the service at `server_url`, signed in as `login_name` with an app password,
    /// once it's shown to share files by link.
    pub async fn add_link_service(
        &self,
        server_url: &str,
        login_name: &str,
        app_password: &str,
    ) -> Result<LinkService> {
        let base = nextcloud::base_url(server_url)?;
        let login_name = login_name.trim().to_owned();
        if login_name.is_empty() || app_password.is_empty() {
            return Err(err("Enter the name you sign in with and an app password."));
        }
        credentials::check_available_async(self.credentials.clone()).await?;
        let client = nextcloud::Client::connect(&base, &login_name, app_password).await?;
        let capabilities = client.capabilities().await?;
        if !capabilities.public_links {
            return Err(err("This server doesn't allow links to files."));
        }
        let display_name = capabilities
            .name
            .or_else(|| base.host_str().map(str::to_owned))
            .unwrap_or_else(|| base.to_string());
        let server_url = base.to_string();
        let service = self
            .db
            .write({
                let (server_url, login_name, display_name) =
                    (server_url.clone(), login_name.clone(), display_name.clone());
                move |conn| {
                    conn.execute(
                        "INSERT INTO link_services (server_url, login_name, display_name, created_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![server_url, login_name, display_name, crate::models::now_ms()],
                    )?;
                    Ok(conn.last_insert_rowid())
                }
            })
            .await
            .map(|id| LinkService {
                id,
                server_url,
                login_name,
                display_name,
            })?;
        if let Err(error) = credentials::store_async(
            self.credentials.clone(),
            0,
            Slot::LinkServicePassword(service.id),
            app_password.to_owned(),
        )
        .await
        {
            let id = service.id;
            let _ = self
                .db
                .write(move |conn| {
                    conn.execute("DELETE FROM link_services WHERE id = ?1", params![id])?;
                    Ok(())
                })
                .await;
            return Err(error);
        }
        Ok(service)
    }

    /// Forgets the service and its app password. Links already sent keep working.
    pub async fn remove_link_service(&self, id: i64) -> Result<()> {
        self.db
            .write(move |conn| {
                conn.execute("DELETE FROM link_services WHERE id = ?1", params![id])?;
                Ok(())
            })
            .await?;
        credentials::delete_async(self.credentials.clone(), 0, Slot::LinkServicePassword(id)).await
    }

    async fn link_client(&self, service: &LinkService) -> Result<nextcloud::Client> {
        let secret = credentials::load_async(
            self.credentials.clone(),
            0,
            Slot::LinkServicePassword(service.id),
        )
        .await?;
        let base = nextcloud::base_url(&service.server_url)?;
        nextcloud::Client::connect(&base, &service.login_name, &secret).await
    }

    /// Uploads the file at `path` to the service as `name` and shares it by a link that reads
    /// it, working as long as Settings says and asking for a password if Settings says so,
    /// or as the server asks.
    pub async fn share_by_link(
        &self,
        service_id: i64,
        path: &Path,
        name: &str,
    ) -> Result<SharedLink> {
        let name = name.trim();
        crate::files::validate_name(name)?;
        let size = tokio::fs::metadata(path).await?.len();
        if size > MAX_LINKED_FILE_BYTES {
            return Err(err("Files of up to 5 GB go by link."));
        }
        let service = self.link_service(service_id).await?;
        let settings = self.db.read(|conn| repo::settings::get(conn)).await?;
        let client = self.link_client(&service).await?;
        let capabilities = client.capabilities().await?;
        if !capabilities.public_links {
            return Err(err("This server doesn't allow links to files."));
        }
        let expires_on = expiry_date(
            chrono::Local::now().date_naive(),
            settings.link_expiry_days,
            capabilities.max_expiry_days,
        );
        let password = if settings.link_password || capabilities.password_enforced {
            let generated = match &capabilities.generate_password_url {
                Some(url) => client.generate_password(url).await,
                None => None,
            };
            Some(generated.unwrap_or_else(|| random_token(16)))
        } else {
            None
        };
        let folder = format!("{FOLDER}/{}", random_token(12));
        let shared = async {
            let size = client
                .upload(path, &folder, name, capabilities.chunking)
                .await?;
            let share = client
                .share(
                    &format!("/{folder}/{name}"),
                    expires_on.as_deref(),
                    password.as_deref(),
                )
                .await?;
            Ok::<_, CoreError>((size, share))
        }
        .await;
        let (size, share) = match shared {
            Ok(shared) => shared,
            Err(error) => {
                // What went up goes again, so a failed upload leaves nothing behind.
                let _ = client.delete(&folder).await;
                return Err(error);
            }
        };
        Ok(SharedLink {
            service_id,
            url: share.url,
            name: name.to_owned(),
            size,
            expires_on: share
                .expiration
                .and_then(|date| date.get(..10).map(str::to_owned))
                .or(expires_on),
            password,
            share_id: share.id,
            folder,
        })
    }

    /// Takes back a link and the file it shares, as a message drops it before it's sent.
    pub async fn unshare_link(&self, link: &SharedLink) -> Result<()> {
        let service = self.link_service(link.service_id).await?;
        let client = self.link_client(&service).await?;
        client.unshare(&link.share_id).await?;
        client.delete(&link.folder).await
    }
}
