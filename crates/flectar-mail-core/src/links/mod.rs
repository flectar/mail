//! Files sent by link: uploaded to a storage service the user connects, such as a Nextcloud
//! server or a mail account's Google Drive or OneDrive, and shared there by a public link
//! that a message carries in place of the file.

mod drive;
mod nextcloud;
mod onedrive;
#[cfg(test)]
mod tests;

use crate::Core;
use crate::accounts::credentials::{self, Slot};
use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::models::{Account, CalendarConnectionKind, Provider, Settings};
use crate::net::proxy::Owner;
use crate::oauth;
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

/// Where a storage service keeps the files it shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkServiceKind {
    /// A Nextcloud, ownCloud or OpenCloud server, signed in to with an app password.
    Nextcloud,
    /// A Gmail account's Google Drive, reached through the account's sign-in.
    GoogleDrive,
    /// A Microsoft account's OneDrive, reached through the account's sign-in.
    OneDrive,
}

impl LinkServiceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Nextcloud => "nextcloud",
            Self::GoogleDrive => "google_drive",
            Self::OneDrive => "onedrive",
        }
    }

    fn from_column(value: &str) -> Self {
        match value {
            "google_drive" => Self::GoogleDrive,
            "onedrive" => Self::OneDrive,
            _ => Self::Nextcloud,
        }
    }
}

/// A storage service files are sent by link from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkService {
    pub id: i64,
    pub kind: LinkServiceKind,
    /// The mail account whose storage it is, for Google Drive and OneDrive.
    pub account_id: Option<i64>,
    /// The server, as `https://cloud.example.com/`, or the storage's API.
    pub server_url: String,
    /// The name the user signs in to the server with, or the account's address.
    pub login_name: String,
    /// What the server calls itself, or its host, or the storage's name.
    pub display_name: String,
}

impl LinkService {
    /// Whose proxy settings the service's requests follow: its account's,
    /// or the app's for a storage server of its own.
    fn owner(&self) -> Owner {
        self.account_id.map_or(Owner::App, Owner::Account)
    }
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

const SERVICE_COLUMNS: &str = "id, kind, account_id, server_url, login_name, display_name";

fn service_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LinkService> {
    Ok(LinkService {
        id: row.get(0)?,
        kind: LinkServiceKind::from_column(&row.get::<_, String>(1)?),
        account_id: row.get(2)?,
        server_url: row.get(3)?,
        login_name: row.get(4)?,
        display_name: row.get(5)?,
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
                let mut statement = conn.prepare(&format!(
                    "SELECT {SERVICE_COLUMNS} FROM link_services ORDER BY id"
                ))?;
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
                        &format!("SELECT {SERVICE_COLUMNS} FROM link_services WHERE id = ?1"),
                        params![id],
                        service_from_row,
                    )
                    .optional()?)
            })
            .await?
            .ok_or_else(|| err("The storage service was removed."))
    }

    /// The service a message from the account with id `account_id` sends files by link from:
    /// the account's own Google Drive or OneDrive when that's on, else the Nextcloud or
    /// ownCloud server. Another account's storage isn't used, to keep each account's files
    /// in its own.
    pub async fn link_service_for_account(&self, account_id: i64) -> Result<Option<LinkService>> {
        let services = self.link_services().await?;
        let own = services
            .iter()
            .find(|service| service.account_id == Some(account_id));
        Ok(own
            .or_else(|| {
                services
                    .iter()
                    .find(|service| service.kind == LinkServiceKind::Nextcloud)
            })
            .cloned())
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
                        "INSERT INTO link_services
                           (kind, server_url, login_name, display_name, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            LinkServiceKind::Nextcloud.as_str(),
                            server_url,
                            login_name,
                            display_name,
                            crate::models::now_ms()
                        ],
                    )?;
                    Ok(conn.last_insert_rowid())
                }
            })
            .await
            .map(|id| LinkService {
                id,
                kind: LinkServiceKind::Nextcloud,
                account_id: None,
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

    /// Turns on sending files by link from the account's own storage: Google Drive for a
    /// Gmail account, OneDrive for a Microsoft one. The account's sign-in is widened in the
    /// browser, which `open_url` opens, to reach the files the app makes there.
    pub async fn add_cloud_link_service(
        &self,
        account_id: i64,
        open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
    ) -> Result<LinkService> {
        let account = self
            .db
            .read(move |conn| repo::accounts::get(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))?;
        let services = self.link_services().await?;
        if let Some(service) = services
            .into_iter()
            .find(|service| service.account_id == Some(account_id))
        {
            return Ok(service);
        }
        let (kind, api, display_name) = match account.provider {
            Provider::Gmail => {
                self.grant_google_drive(&account, open_url).await?;
                let token = self
                    .tokens
                    .access_token(account_id, Provider::Gmail)
                    .await?;
                // Looking for the app's folder shows that Drive answers the app.
                drive::Drive::new(Owner::Account(account_id), drive::API, token)?
                    .find_folder(FOLDER)
                    .await?;
                (LinkServiceKind::GoogleDrive, drive::API, "Google Drive")
            }
            Provider::Microsoft => {
                let token = self
                    .graph_token_with_consent(
                        account_id,
                        oauth::providers::MS_FILES_SCOPE,
                        open_url,
                    )
                    .await?;
                onedrive::OneDrive::new(Owner::Account(account_id), onedrive::API, token)?
                    .check()
                    .await?;
                (LinkServiceKind::OneDrive, onedrive::API, "OneDrive")
            }
            Provider::Generic => {
                return Err(err("Only Gmail and Microsoft accounts come with storage."));
            }
        };
        let email = account.email.clone();
        let id = self
            .db
            .write(move |conn| {
                conn.execute(
                    "INSERT INTO link_services
                       (kind, account_id, server_url, login_name, display_name, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        kind.as_str(),
                        account_id,
                        api,
                        email,
                        display_name,
                        crate::models::now_ms()
                    ],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .await?;
        Ok(LinkService {
            id,
            kind,
            account_id: Some(account_id),
            server_url: api.to_owned(),
            login_name: account.email,
            display_name: display_name.to_owned(),
        })
    }

    /// Widens the Gmail account's sign-in in the browser to reach the files the app makes in
    /// its Google Drive, keeping what else it reaches.
    async fn grant_google_drive(
        &self,
        account: &Account,
        open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
    ) -> Result<()> {
        credentials::check_available_async(self.credentials.clone()).await?;
        let mut scopes = self.google_scopes_in_use(account.id).await?;
        scopes.push(oauth::providers::GOOGLE_DRIVE_FILE_SCOPE);
        let outcome = tokio::select! {
            outcome = oauth::flow::authorize_with_broker(
                Provider::Gmail,
                &scopes,
                Some(&account.email),
                self.oauth_redirects.clone(),
                open_url,
            ) => outcome?,
            _ = self.oauth_cancel.notified() => {
                return Err(CoreError::Auth("sign-in cancelled".into()));
            }
        };
        self.store_sign_in(account, outcome).await
    }

    /// The Google permissions past mail that the Gmail account with id `account_id` uses,
    /// which a sign-in asks for again so that it keeps them: Calendar while its calendars
    /// are connected, Drive while files go by link from it, and Gmail settings once the
    /// app saved its automatic replies. Google keeps earlier grants
    /// on the web, but Android asks for what the sign-in lists, each time.
    pub(crate) async fn google_scopes_in_use(&self, account_id: i64) -> Result<Vec<&'static str>> {
        let calendar = self
            .calendar_db
            .read(move |conn| {
                Ok(repo::caldav::provider_connection(
                    conn,
                    account_id,
                    CalendarConnectionKind::Google,
                )?
                .is_some_and(|connection| connection.enabled))
            })
            .await?;
        let drive = self
            .db
            .read(move |conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM link_services WHERE account_id = ?1 AND kind = ?2)",
                    params![account_id, LinkServiceKind::GoogleDrive.as_str()],
                    |row| row.get::<_, bool>(0),
                )?)
            })
            .await?;
        let settings = self
            .db
            .read(move |conn| repo::auto_replies::uses_settings(conn, account_id))
            .await?;
        let mut scopes = Vec::new();
        if calendar {
            scopes.push(oauth::providers::GOOGLE_CALENDAR_SCOPE);
        }
        if drive {
            scopes.push(oauth::providers::GOOGLE_DRIVE_FILE_SCOPE);
        }
        if settings {
            scopes.push(oauth::providers::GMAIL_SETTINGS_SCOPE);
        }
        Ok(scopes)
    }

    /// Forgets the service, and a server's app password. Links already sent keep working.
    pub async fn remove_link_service(&self, id: i64) -> Result<()> {
        let kind = self
            .db
            .write(move |conn| {
                let kind = conn
                    .query_row(
                        "SELECT kind FROM link_services WHERE id = ?1",
                        params![id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                conn.execute("DELETE FROM link_services WHERE id = ?1", params![id])?;
                Ok(kind)
            })
            .await?;
        // An account's storage signs in with the account, and has no password of its own.
        if kind
            .is_some_and(|kind| LinkServiceKind::from_column(&kind) != LinkServiceKind::Nextcloud)
        {
            return Ok(());
        }
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

    /// Uploads the file at `path` to the service as `name` and shares it by a link anyone with
    /// it can open, working as long as Settings says where the service lets links expire, and
    /// asking for a password where Settings or a server says so.
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
        match service.kind {
            LinkServiceKind::Nextcloud => {
                self.share_on_server(&service, &settings, path, name).await
            }
            LinkServiceKind::GoogleDrive => {
                let drive = drive::Drive::new(
                    service.owner(),
                    &service.server_url,
                    self.drive_token(&service).await?,
                )?;
                share_on_drive(&drive, service.id, path, name).await
            }
            LinkServiceKind::OneDrive => {
                let drive = onedrive::OneDrive::new(
                    service.owner(),
                    &service.server_url,
                    self.onedrive_token(&service).await?,
                )?;
                let expires_on = expiry_date(
                    chrono::Local::now().date_naive(),
                    settings.link_expiry_days,
                    None,
                );
                share_on_onedrive(&drive, service.id, path, name, expires_on).await
            }
        }
    }

    /// The Gmail account's access token, for its Google Drive.
    async fn drive_token(&self, service: &LinkService) -> Result<String> {
        let account_id = service
            .account_id
            .ok_or_else(|| err("The storage service was removed."))?;
        self.tokens
            .access_token(account_id, Provider::Gmail)
            .await
            .map_err(|error| match error {
                CoreError::NeedsReauth => {
                    err("Sign in to the account again to send files from its Google Drive.")
                }
                error => error,
            })
    }

    /// A token for the Microsoft account's OneDrive.
    async fn onedrive_token(&self, service: &LinkService) -> Result<String> {
        let account_id = service
            .account_id
            .ok_or_else(|| err("The storage service was removed."))?;
        self.tokens
            .access_token_for_scope(account_id, Provider::Microsoft, oauth::providers::MS_FILES_SCOPE)
            .await
            .map_err(|error| match error {
                CoreError::NeedsReauth => err(
                    "Microsoft signed this app out of OneDrive. Turn OneDrive on again in Settings.",
                ),
                error => error,
            })
    }

    /// Uploads the file at `path` to the Nextcloud or ownCloud server as `name`, in a folder
    /// of its own, and shares it by a link that works as long as Settings and the server say,
    /// asking for a password if either says so.
    async fn share_on_server(
        &self,
        service: &LinkService,
        settings: &Settings,
        path: &Path,
        name: &str,
    ) -> Result<SharedLink> {
        let service_id = service.id;
        let client = self.link_client(service).await?;
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
        match service.kind {
            LinkServiceKind::Nextcloud => {
                let client = self.link_client(&service).await?;
                client.unshare(&link.share_id).await?;
                client.delete(&link.folder).await
            }
            // Deleting the file takes its link with it.
            LinkServiceKind::GoogleDrive => {
                drive::Drive::new(
                    service.owner(),
                    &service.server_url,
                    self.drive_token(&service).await?,
                )?
                .delete(&link.share_id)
                .await
            }
            // The file's folder of its own goes, and the file and its link with it.
            LinkServiceKind::OneDrive => {
                onedrive::OneDrive::new(
                    service.owner(),
                    &service.server_url,
                    self.onedrive_token(&service).await?,
                )?
                .delete(&link.folder)
                .await
            }
        }
    }
}

/// Uploads the file at `path` to the app's folder in Google Drive as `name`, and shares it by
/// a link anyone with it can open, which Drive can't make expire.
async fn share_on_drive(
    drive: &drive::Drive,
    service_id: i64,
    path: &Path,
    name: &str,
) -> Result<SharedLink> {
    let folder = drive.folder(FOLDER).await?;
    let (id, size) = drive.upload(path, name, &folder).await?;
    match drive.share(&id).await {
        Ok(url) => Ok(SharedLink {
            service_id,
            url,
            name: name.to_owned(),
            size,
            expires_on: None,
            password: None,
            share_id: id,
            folder: String::new(),
        }),
        Err(error) => {
            // What went up goes again, so a failed share leaves nothing behind.
            let _ = drive.delete(&id).await;
            Err(error)
        }
    }
}

/// Uploads the file at `path` to OneDrive as `name`, in a folder of its own in the app's
/// folder, and shares it by a link anyone with it can open, until the end of `expires_on`
/// where the account lets links expire.
async fn share_on_onedrive(
    drive: &onedrive::OneDrive,
    service_id: i64,
    path: &Path,
    name: &str,
    expires_on: Option<String>,
) -> Result<SharedLink> {
    let parent = drive.folder(FOLDER).await?;
    let folder = drive.make_folder(&parent, &random_token(12)).await?;
    let shared = async {
        let (id, size) = drive.upload(path, &folder, name).await?;
        let (url, expires_on) = drive.share(&id, expires_on.as_deref()).await?;
        Ok::<_, CoreError>(SharedLink {
            service_id,
            url,
            name: name.to_owned(),
            size,
            expires_on,
            password: None,
            share_id: id,
            folder: folder.clone(),
        })
    }
    .await;
    if shared.is_err() {
        // What went up goes again, so a failed upload leaves nothing behind.
        let _ = drive.delete(&folder).await;
    }
    shared
}
