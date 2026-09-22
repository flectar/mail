use crate::error::{CoreError, Result};
use crate::models::AccountConfig;
use jmap_client::URI;
use jmap_client::client::{Client, Credentials};
use jmap_client::core::session::{Capabilities, Session};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use url::Url;

pub const CORE_CAPABILITY: &str = "urn:ietf:params:jmap:core";
pub const MAIL_CAPABILITY: &str = "urn:ietf:params:jmap:mail";
pub const SUBMISSION_CAPABILITY: &str = "urn:ietf:params:jmap:submission";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthenticationScheme {
    Bearer,
    Basic,
}

static AUTHENTICATION_SCHEMES: OnceLock<Mutex<HashMap<String, AuthenticationScheme>>> =
    OnceLock::new();

/// An authenticated, capability-checked JMAP session and the Mail account it
/// projects. A JMAP Session may expose shared accounts; Flectar intentionally
/// pins the selected account id instead of relying on map iteration order.
pub struct ConnectedClient {
    pub client: Client,
    pub download_http: reqwest::Client,
    pub account_id: String,
    pub base_url: String,
    pub supports_submission: bool,
    pub may_create_top_level_mailbox: bool,
}

/// Turn a user-entered host/base URL into the base expected by RFC 8620
/// discovery. Direct `/.well-known/jmap` and conventional `/jmap` suffixes are
/// accepted for convenience. A path prefix is preserved because Stalwart can
/// intentionally publish JMAP below a reverse-proxy mount point.
pub fn normalize_base_url(value: &str, email: &str) -> Result<String> {
    let entered = value.trim();
    let candidate = if entered.is_empty() {
        let domain = email
            .rsplit_once('@')
            .map(|(_, domain)| domain.trim())
            .filter(|domain| !domain.is_empty())
            .ok_or_else(|| CoreError::Auth("enter a valid email address".into()))?;
        format!("https://{domain}")
    } else if entered.contains("://") {
        entered.to_owned()
    } else {
        format!("https://{entered}")
    };

    let mut url = Url::parse(&candidate)
        .map_err(|error| CoreError::Auth(format!("invalid JMAP server address: {error}")))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(CoreError::Auth(
            "do not include credentials in the JMAP server address".into(),
        ));
    }
    if url.host().is_none() {
        return Err(CoreError::Auth("JMAP server address has no host".into()));
    }
    let secure = url.scheme() == "https";
    let loopback = match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    if !secure && !(url.scheme() == "http" && loopback) {
        return Err(CoreError::Auth(
            "JMAP requires HTTPS (plain HTTP is allowed only for localhost)".into(),
        ));
    }

    let path = url.path().trim_end_matches('/');
    let base_path = path
        .strip_suffix("/.well-known/jmap")
        .or_else(|| path.strip_suffix("/jmap"))
        .unwrap_or(path)
        .trim_end_matches('/')
        .to_owned();
    url.set_path(&base_path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

pub async fn connect(config: &AccountConfig, secret: &str) -> Result<ConnectedClient> {
    connect_with(
        &config.email,
        &config.username,
        secret,
        &config.jmap_url,
        config.jmap_account_id.as_deref(),
    )
    .await
}

pub async fn connect_with(
    email: &str,
    username: &str,
    secret: &str,
    server: &str,
    preferred_account_id: Option<&str>,
) -> Result<ConnectedClient> {
    let base_url = normalize_base_url(server, email)?;
    let url = Url::parse(&base_url)
        .map_err(|error| CoreError::Auth(format!("invalid JMAP server address: {error}")))?;
    let host = url.host_str().unwrap_or_default().to_owned();
    let username = if username.trim().is_empty() {
        email.trim()
    } else {
        username.trim()
    };
    // JMAP deliberately uses standard HTTP authentication without mandating a
    // scheme, and RFC 8620 discourages Basic. Prefer a usable Bearer header,
    // fall back once on 401, then remember only the scheme for this login.
    let cache_key = format!("{base_url}\n{username}");
    let bearer_supported = secret.is_ascii()
        && reqwest::header::HeaderValue::from_bytes(format!("Bearer {secret}").as_bytes()).is_ok();
    let cached_scheme = authentication_schemes()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&cache_key)
        .copied();
    let first_scheme = match cached_scheme {
        Some(AuthenticationScheme::Bearer) if !bearer_supported => AuthenticationScheme::Basic,
        Some(scheme) => scheme,
        None if bearer_supported => AuthenticationScheme::Bearer,
        None => AuthenticationScheme::Basic,
    };
    let second_scheme = match (first_scheme, bearer_supported) {
        (AuthenticationScheme::Bearer, _) => Some(AuthenticationScheme::Basic),
        (AuthenticationScheme::Basic, true) => Some(AuthenticationScheme::Bearer),
        (AuthenticationScheme::Basic, false) => None,
    };
    let mut client = match connect_authenticated(&base_url, &host, username, secret, first_scheme)
        .await
    {
        Ok(client) => {
            remember_authentication_scheme(&cache_key, first_scheme);
            client
        }
        Err(error) if is_unauthorized(&error) => {
            let Some(second_scheme) = second_scheme else {
                return Err(map_error(error));
            };
            match connect_authenticated(&base_url, &host, username, secret, second_scheme).await {
                Ok(client) => {
                    remember_authentication_scheme(&cache_key, second_scheme);
                    client
                }
                Err(error) => return Err(map_error(error)),
            }
        }
        Err(error) => return Err(map_error(error)),
    };

    let session = client.session();
    if !session.has_capability(CORE_CAPABILITY) || !session.has_capability(MAIL_CAPABILITY) {
        return Err(CoreError::Jmap(
            "server session does not advertise JMAP Core and Mail capabilities".into(),
        ));
    }
    validate_session_endpoints(&session, &url)?;
    if !session.has_capability(SUBMISSION_CAPABILITY) {
        return Err(CoreError::Jmap(
            "server does not advertise JMAP EmailSubmission".into(),
        ));
    }

    let account_id = if let Some(preferred) = preferred_account_id {
        if account_is_eligible(&session, preferred) {
            preferred.to_owned()
        } else {
            return Err(CoreError::Auth(
                "the selected JMAP Mail account is no longer writable or available; reconnect the account"
                    .into(),
            ));
        }
    } else {
        session
            .primary_accounts()
            .find(|(capability, account_id)| {
                capability.as_str() == MAIL_CAPABILITY && account_is_eligible(&session, account_id)
            })
            .map(|(_, account_id)| account_id.clone())
            .or_else(|| {
                session
                    .accounts()
                    .find(|account_id| account_is_eligible(&session, account_id))
                    .cloned()
            })
            .ok_or_else(|| {
                CoreError::Jmap(
                    "no writable JMAP Mail account with EmailSubmission is available".into(),
                )
            })?
    };
    let may_create_top_level_mailbox = session
        .account(&account_id)
        .and_then(|account| account.capability(MAIL_CAPABILITY))
        .and_then(|capability| match capability {
            Capabilities::Mail(mail) => Some(mail.may_create_top_level_mailbox()),
            _ => None,
        })
        .or_else(|| {
            session
                .mail_capabilities()
                .map(|mail| mail.may_create_top_level_mailbox())
        })
        .unwrap_or(false);
    drop(session);
    client.set_default_account_id(account_id.clone());
    let mut download_headers = client.headers().clone();
    download_headers.remove(reqwest::header::CONTENT_TYPE);
    let download_http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .default_headers(download_headers)
        .build()
        .map_err(|error| CoreError::Network(format!("JMAP download client failed: {error}")))?;

    Ok(ConnectedClient {
        client,
        download_http,
        account_id,
        base_url,
        supports_submission: true,
        may_create_top_level_mailbox,
    })
}

fn authentication_schemes() -> &'static Mutex<HashMap<String, AuthenticationScheme>> {
    AUTHENTICATION_SCHEMES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remember_authentication_scheme(cache_key: &str, scheme: AuthenticationScheme) {
    authentication_schemes()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(cache_key.to_owned(), scheme);
}

async fn connect_authenticated(
    base_url: &str,
    host: &str,
    username: &str,
    secret: &str,
    scheme: AuthenticationScheme,
) -> std::result::Result<Client, jmap_client::Error> {
    let credentials = match scheme {
        AuthenticationScheme::Bearer => Credentials::bearer(secret),
        AuthenticationScheme::Basic => Credentials::basic(username, secret),
    };
    Client::new()
        .credentials(credentials)
        .timeout(std::time::Duration::from_secs(30))
        // RFC 8620 permits the well-known resource to redirect. Limit the
        // library to the origin the user selected; advertised endpoint URLs
        // come from the authenticated Session resource itself.
        .follow_redirects([host])
        .connect(base_url)
        .await
}

fn is_authentication_rejection(error: &jmap_client::Error) -> bool {
    match error {
        jmap_client::Error::Problem(problem) => matches!(problem.status(), Some(401 | 403)),
        jmap_client::Error::Server(message) => {
            message.starts_with("401 ") || message.starts_with("403 ")
        }
        _ => false,
    }
}

fn is_unauthorized(error: &jmap_client::Error) -> bool {
    match error {
        jmap_client::Error::Problem(problem) => problem.status() == Some(401),
        jmap_client::Error::Server(message) => message.starts_with("401 "),
        _ => false,
    }
}

fn account_is_eligible(session: &Session, account_id: &str) -> bool {
    session.account(account_id).is_some_and(|account| {
        !account.is_read_only()
            && account.capability(MAIL_CAPABILITY).is_some()
            && account.capability(SUBMISSION_CAPABILITY).is_some()
    })
}

fn validate_session_endpoints(session: &Session, discovery: &Url) -> Result<()> {
    for (name, value) in [
        ("apiUrl", session.api_url()),
        ("downloadUrl", session.download_url()),
        ("uploadUrl", session.upload_url()),
        ("eventSourceUrl", session.event_source_url()),
    ] {
        validate_session_endpoint(name, value, discovery)?;
    }
    Ok(())
}

fn validate_session_endpoint(name: &str, value: &str, discovery: &Url) -> Result<()> {
    let expanded = [
        "accountId",
        "blobId",
        "type",
        "name",
        "types",
        "closeafter",
        "ping",
    ]
    .into_iter()
    .fold(value.to_owned(), |url, parameter| {
        url.replace(&format!("{{{parameter}}}"), "x")
    });
    let endpoint = Url::parse(&expanded)
        .map_err(|error| CoreError::Jmap(format!("Session {name} is not a valid URL: {error}")))?;
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        return Err(CoreError::Jmap(format!(
            "Session {name} must not contain embedded credentials"
        )));
    }
    if endpoint.scheme() == "https" {
        return Ok(());
    }
    let discovery_is_loopback = discovery.host().is_some_and(|host| match host {
        url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(address) => address.is_loopback(),
        url::Host::Ipv6(address) => address.is_loopback(),
    });
    let local_development = discovery.scheme() == "http"
        && endpoint.scheme() == "http"
        && endpoint.host_str() == discovery.host_str()
        && endpoint.port_or_known_default() == discovery.port_or_known_default()
        && discovery_is_loopback;
    if local_development {
        Ok(())
    } else {
        Err(CoreError::Jmap(format!("Session {name} must use HTTPS")))
    }
}

pub fn map_error(error: jmap_client::Error) -> CoreError {
    match &error {
        _ if is_authentication_rejection(&error) => {
            CoreError::Auth("JMAP rejected the password, app password, or API token".into())
        }
        jmap_client::Error::Transport(source) if source.is_timeout() || source.is_connect() => {
            CoreError::Network(format!("JMAP connection failed: {source}"))
        }
        _ => CoreError::Jmap(error.to_string()),
    }
}

pub fn supports_mail(client: &Client) -> bool {
    client.session().has_capability(URI::Mail.as_ref())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn read_request_head(stream: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::with_capacity(1024);
        let mut chunk = [0; 1024];
        loop {
            let size = stream.read(&mut chunk).await.unwrap();
            if size == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..size]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
            assert!(request.len() <= 16 * 1024, "request headers are too large");
        }
        String::from_utf8(request).unwrap()
    }

    fn session_body(origin: &str, with_submission: bool) -> String {
        let core = serde_json::json!({
            "maxSizeUpload": 50_000_000,
            "maxConcurrentUpload": 4,
            "maxSizeRequest": 10_000_000,
            "maxConcurrentRequests": 4,
            "maxCallsInRequest": 16,
            "maxObjectsInGet": 500,
            "maxObjectsInSet": 500,
            "collationAlgorithms": []
        });
        let mail = serde_json::json!({
            "maxMailboxesPerEmail": null,
            "maxMailboxDepth": 10,
            "maxSizeMailboxName": 255,
            "maxSizeAttachmentsPerEmail": 50_000_000,
            "emailQuerySortOptions": ["receivedAt"],
            "mayCreateTopLevelMailbox": true
        });
        let submission = serde_json::json!({
            "maxDelayedSend": 0,
            "submissionExtensions": []
        });
        let mut capabilities = serde_json::Map::from_iter([
            (CORE_CAPABILITY.into(), core),
            (MAIL_CAPABILITY.into(), mail.clone()),
        ]);
        let mut account_capabilities = serde_json::Map::from_iter([(MAIL_CAPABILITY.into(), mail)]);
        if with_submission {
            capabilities.insert(SUBMISSION_CAPABILITY.into(), submission.clone());
            account_capabilities.insert(SUBMISSION_CAPABILITY.into(), submission);
        }
        serde_json::json!({
            "capabilities": capabilities,
            "accounts": {
                "mail-account": {
                    "name": "Test",
                    "isPersonal": true,
                    "isReadOnly": false,
                    "accountCapabilities": account_capabilities
                }
            },
            "primaryAccounts": { (MAIL_CAPABILITY): "mail-account" },
            "username": "me@example.test",
            "apiUrl": format!("{origin}/api"),
            "downloadUrl": format!("{origin}/download/{{accountId}}/{{blobId}}/{{name}}?accept={{type}}"),
            "uploadUrl": format!("{origin}/upload/{{accountId}}"),
            "eventSourceUrl": format!("{origin}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
            "state": "session-1"
        })
        .to_string()
    }

    pub(crate) async fn session_server(
        with_submission: bool,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let body = session_body(&origin, with_submission);
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_request_head(&mut stream).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        (origin, task)
    }

    async fn authentication_server(
        expected_authorization: &'static str,
        request_count: usize,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let body = session_body(&origin, true);
        let task = tokio::spawn(async move {
            let mut authorizations = Vec::new();
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request_head(&mut stream).await;
                let authorization = request
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("authorization: ")
                            .or_else(|| line.strip_prefix("Authorization: "))
                    })
                    .unwrap_or_default()
                    .trim_end_matches('\r')
                    .to_owned();
                let authenticated = authorization == expected_authorization;
                authorizations.push(authorization);
                let response = if authenticated {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    "HTTP/1.1 401 Unauthorized\r\nContent-Type: text/plain\r\nContent-Length: 12\r\nConnection: close\r\n\r\nUnauthorized"
                        .to_owned()
                };
                stream.write_all(response.as_bytes()).await.unwrap();
            }
            authorizations
        });
        (origin, task)
    }

    #[test]
    fn normalizes_discovery_origins_and_conventional_endpoints() {
        assert_eq!(
            normalize_base_url("mail.example.test", "me@example.test").unwrap(),
            "https://mail.example.test"
        );
        assert_eq!(
            normalize_base_url("https://mail.example.test/.well-known/jmap", "x@y").unwrap(),
            "https://mail.example.test"
        );
        assert_eq!(
            normalize_base_url("", "me@example.test").unwrap(),
            "https://example.test"
        );
    }

    #[test]
    fn refuses_cleartext_remote_credentials_but_allows_local_development() {
        assert!(normalize_base_url("http://mail.example.test", "x@y").is_err());
        assert_eq!(
            normalize_base_url("http://127.0.0.1:8080", "x@y").unwrap(),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn rejects_embedded_credentials_and_preserves_proxy_prefixes() {
        assert!(normalize_base_url("https://user:secret@mail.example.test", "x@y").is_err());
        assert_eq!(
            normalize_base_url("https://mail.example.test/mail", "x@y").unwrap(),
            "https://mail.example.test/mail"
        );
        assert_eq!(
            normalize_base_url("https://mail.example.test/mail/.well-known/jmap", "x@y").unwrap(),
            "https://mail.example.test/mail"
        );
        assert_eq!(
            normalize_base_url("https://mail.example.test/jmap", "x@y").unwrap(),
            "https://mail.example.test"
        );
    }

    #[tokio::test]
    async fn authenticated_discovery_selects_mail_account_and_requires_submission() {
        let (origin, server) = session_server(true).await;
        let connected = connect_with(
            "me@example.test",
            "me@example.test",
            "app-password",
            &origin,
            None,
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(connected.account_id, "mail-account");
        assert!(connected.supports_submission);

        let (origin, server) = session_server(false).await;
        let error = connect_with(
            "me@example.test",
            "me@example.test",
            "app-password",
            &origin,
            None,
        )
        .await
        .err()
        .expect("receive-only JMAP must be rejected");
        server.await.unwrap();
        assert!(error.to_string().contains("EmailSubmission"));
    }

    #[tokio::test]
    async fn bearer_token_authentication_is_supported() {
        let (origin, server) = authentication_server("Bearer api-token", 1).await;
        connect_with(
            "me@example.test",
            "me@example.test",
            "api-token",
            &origin,
            None,
        )
        .await
        .unwrap();

        assert_eq!(server.await.unwrap(), ["Bearer api-token"]);
    }

    #[tokio::test]
    async fn basic_authentication_falls_back_once_and_is_cached() {
        let (origin, server) =
            authentication_server("Basic bWVAZXhhbXBsZS50ZXN0OmFwcC1wYXNzd29yZA==", 3).await;
        for _ in 0..2 {
            connect_with(
                "me@example.test",
                "me@example.test",
                "app-password",
                &origin,
                None,
            )
            .await
            .unwrap();
        }

        assert_eq!(
            server.await.unwrap(),
            [
                "Bearer app-password",
                "Basic bWVAZXhhbXBsZS50ZXN0OmFwcC1wYXNzd29yZA==",
                "Basic bWVAZXhhbXBsZS50ZXN0OmFwcC1wYXNzd29yZA==",
            ]
        );
    }

    #[tokio::test]
    async fn secrets_that_cannot_be_bearer_headers_use_basic_directly() {
        let (origin, server) =
            authentication_server("Basic bWVAZXhhbXBsZS50ZXN0OnDDpHNzd29yZA==", 1).await;
        connect_with(
            "me@example.test",
            "me@example.test",
            "pässword",
            &origin,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            server.await.unwrap(),
            ["Basic bWVAZXhhbXBsZS50ZXN0OnDDpHNzd29yZA=="]
        );
    }

    #[test]
    fn plain_http_authentication_statuses_are_classified_as_auth_errors() {
        assert!(matches!(
            map_error(jmap_client::Error::Server("401 Unauthorized".into())),
            CoreError::Auth(_)
        ));
        assert!(matches!(
            map_error(jmap_client::Error::Server("403 Forbidden".into())),
            CoreError::Auth(_)
        ));
    }
}
