//! SMTP submission with password or XOAUTH2 sign-in. Connections are opened
//! here and handed to lettre's protocol client, so every security mode uses
//! the account's own TLS trust and one code path.

use crate::accounts::credentials::{self, CredentialStoreHandle, Slot};
use crate::error::{CoreError, Result};
use crate::models::{AccountConfig, AuthKind, ConnectionSecurity, LoginMethod};
use crate::net::{self, MailStream};
use lettre::address::Envelope;
use lettre::transport::smtp::{
    authentication::{Credentials, Mechanism},
    client::{AsyncSmtpConnection, AsyncTokioStream},
    commands::{Data, Mail, Noop, Rcpt},
    extension::{ClientId, Extension, MailBodyParameter, MailParameter},
    response::Severity,
};
use std::{
    fmt,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::{Instant, Sleep};

/// Bound for connecting, the greeting, STARTTLS, and sign-in. Exchange
/// Online can leave a throttled session open without completing one of these
/// phases; without a deadline a queued send would stay in flight forever.
const SMTP_TIMEOUT: Duration = Duration::from_secs(30);
/// A connection that moves no bytes for this long has stalled.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the reply to the end of a message may take. Until it comes the
/// server may have taken the message or not, so RFC 5321 section 4.5.3.2.6
/// asks clients to wait this long rather than time out and send it again.
pub(crate) const MESSAGE_REPLY_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The message goes out in parts of this size, so the end of each part is a
/// point where the send can still stop without delivering anything.
const PART_BYTES: usize = 64 * 1024;

/// The send's limits, shared with the stream it writes through.
#[derive(Debug, Default)]
struct Gate {
    /// When the whole message must have gone out by. Past it the stream
    /// writes nothing more, so a message not yet finished never is, and the
    /// server discards it (RFC 5321 section 4.1.1.4).
    deadline: Option<Instant>,
    /// A write was turned away at the deadline.
    refused: AtomicBool,
    /// The whole message went out and only its end-of-data marker follows:
    /// from here the server may take it, and its reply may take as long as
    /// [`MESSAGE_REPLY_TIMEOUT`].
    ended: AtomicBool,
    /// The account's server timeout, which stretches the idle wait (#252).
    timeout: Duration,
}

impl Gate {
    fn until(deadline: Option<Instant>, timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            deadline,
            timeout,
            ..Self::default()
        })
    }

    fn idle_timeout(&self) -> Duration {
        let idle = net::timeouts::stretch(IDLE_TIMEOUT, self.timeout);
        if self.ended.load(Ordering::Acquire) {
            MESSAGE_REPLY_TIMEOUT.max(idle)
        } else {
            idle
        }
    }
}

pub enum SmtpAuth {
    Password(String),
    XOAuth2(String),
    /// No sign-in, for a server that relays the mail of its network (#252).
    None,
}

/// How account `config` signs in to its SMTP server, with its OAuth token,
/// the SMTP server's own password or the account's, or not at all (#252).
pub(crate) async fn account_auth(
    ctx: &crate::sync::engine::SyncCtx,
    config: &AccountConfig,
) -> Result<SmtpAuth> {
    Ok(match config.auth_kind {
        AuthKind::Oauth2 => {
            SmtpAuth::XOAuth2(ctx.tokens.access_token(config.id, config.provider).await?)
        }
        AuthKind::Password if config.settings.connection.smtp_login == LoginMethod::None => {
            SmtpAuth::None
        }
        AuthKind::Password => SmtpAuth::Password(password(ctx.credentials.clone(), config).await?),
    })
}

/// The SMTP password of account `config`. That's the SMTP server's own when
/// it signs in with its own username and one is saved, else the account's.
pub(crate) async fn password(
    store: CredentialStoreHandle,
    config: &AccountConfig,
) -> Result<String> {
    if config.settings.connection.smtp_username.is_some() {
        match credentials::load_async(store.clone(), config.id, Slot::SmtpPassword).await {
            Ok(own) if !own.is_empty() => return Ok(own),
            Err(error @ CoreError::CredentialStoreUnavailable(_)) => return Err(error),
            _ => {}
        }
    }
    credentials::load_async(store, config.id, Slot::Password).await
}

/// The stream lettre drives. After STARTTLS the server's greeting has been
/// consumed, so `greeting` replays one for lettre's post-upgrade EHLO. Reads
/// and writes fail once the connection has been idle for [`IDLE_TIMEOUT`],
/// or for [`MESSAGE_REPLY_TIMEOUT`] while the end of a message waits for its
/// reply, and writes stop at the [`Gate`]'s deadline.
struct SmtpStream {
    inner: MailStream,
    greeting: &'static [u8],
    idle: Pin<Box<Sleep>>,
    gate: Arc<Gate>,
}

impl SmtpStream {
    fn new(inner: MailStream, greeting: &'static [u8], gate: Arc<Gate>) -> Self {
        Self {
            inner,
            greeting,
            idle: Box::pin(tokio::time::sleep(gate.idle_timeout())),
            gate,
        }
    }

    /// Restart the idle deadline after progress, or fail once it passed.
    fn track<T>(
        &mut self,
        context: &mut Context<'_>,
        poll: Poll<std::io::Result<T>>,
    ) -> Poll<std::io::Result<T>> {
        match poll {
            Poll::Ready(result) => {
                self.idle
                    .as_mut()
                    .reset(Instant::now() + self.gate.idle_timeout());
                Poll::Ready(result)
            }
            Poll::Pending => match self.idle.as_mut().poll(context) {
                Poll::Ready(()) => Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "SMTP connection timed out after {}s",
                        self.gate.idle_timeout().as_secs()
                    ),
                ))),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

impl fmt::Debug for SmtpStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmtpStream")
            .field("encrypted", &self.inner.is_encrypted())
            .finish_non_exhaustive()
    }
}

impl AsyncRead for SmtpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if !this.greeting.is_empty() && buffer.remaining() != 0 {
            let length = this.greeting.len().min(buffer.remaining());
            buffer.put_slice(&this.greeting[..length]);
            this.greeting = &this.greeting[length..];
            return Poll::Ready(Ok(()));
        }
        let poll = Pin::new(&mut this.inner).poll_read(context, buffer);
        this.track(context, poll)
    }
}

impl AsyncWrite for SmtpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this
            .gate
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            this.gate.refused.store(true, Ordering::Release);
            return Poll::Ready(Err(std::io::Error::other(
                "the time to send this message ended before it was complete",
            )));
        }
        let poll = Pin::new(&mut this.inner).poll_write(context, buffer);
        this.track(context, poll)
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_flush(context);
        this.track(context, poll)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(context)
    }
}

impl AsyncTokioStream for SmtpStream {
    fn peer_addr(&self) -> std::io::Result<SocketAddr> {
        self.inner.peer_addr()
    }
}

async fn read_smtp_response(stream: &mut TcpStream, stage: &str) -> Result<String> {
    const MAX_RESPONSE_BYTES: usize = 100_000;
    const MAX_LINE_BYTES: usize = 1_000;

    let mut response = Vec::new();
    loop {
        let line_start = response.len();
        loop {
            if response.len() >= MAX_RESPONSE_BYTES
                || response.len().saturating_sub(line_start) >= MAX_LINE_BYTES
            {
                return Err(CoreError::Smtp(format!(
                    "SMTP {stage} response was too large"
                )));
            }
            let mut byte = [0];
            stream
                .read_exact(&mut byte)
                .await
                .map_err(|error| CoreError::Smtp(format!("SMTP {stage}: {error}")))?;
            response.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }

        let line = &response[line_start..];
        if line.len() < 4 || !line[..3].iter().all(u8::is_ascii_digit) {
            return Err(CoreError::Smtp(format!(
                "SMTP {stage} returned an invalid response"
            )));
        }
        if line[3] == b' ' {
            return String::from_utf8(response)
                .map_err(|_| CoreError::Smtp(format!("SMTP {stage} was not valid UTF-8")));
        }
        if line[3] != b'-' {
            return Err(CoreError::Smtp(format!(
                "SMTP {stage} returned an invalid response"
            )));
        }
    }
}

fn expect_smtp_status(response: &str, expected: &str, stage: &str) -> Result<()> {
    if response.as_bytes().get(..3) == Some(expected.as_bytes()) {
        Ok(())
    } else {
        Err(CoreError::Smtp(format!(
            "SMTP {stage} failed: {}",
            response.trim_end()
        )))
    }
}

/// TLS is negotiated before lettre sees the connection, so its errors are
/// protocol and sign-in failures.
fn map_smtp_error(cfg: &AccountConfig, error: lettre::transport::smtp::Error) -> CoreError {
    CoreError::Smtp(format!("{}:{}: {error}", cfg.smtp_host, cfg.smtp_port))
}

/// A failed AUTH. Only this reply can reject credentials, and only with 535
/// (RFC 4954), the one reply K-9 Mail also treats as a wrong password. 454 is
/// a temporary failure, and 534 and 538 ask for a stronger mechanism or an
/// encrypted connection. Replies after sign-in, such as a refused sender,
/// are never about the password.
fn map_smtp_sign_in_error(
    cfg: &AccountConfig,
    auth: &SmtpAuth,
    error: lettre::transport::smtp::Error,
) -> CoreError {
    let (host, port, user) = (&cfg.smtp_host, cfg.smtp_port, cfg.smtp_username());
    match error.status().map(u16::from) {
        Some(535) => CoreError::Auth(match auth {
            SmtpAuth::Password(_) | SmtpAuth::None => {
                format!("{host} rejected the username {user} or its password ({error})")
            }
            SmtpAuth::XOAuth2(_) => {
                format!(
                    "{host} rejected the OAuth sign-in for {} ({error})",
                    cfg.username
                )
            }
        }),
        Some(534) => CoreError::Smtp(format!(
            "{host}:{port} requires a stronger sign-in method than this account uses ({error})"
        )),
        Some(538) => CoreError::Smtp(format!(
            "{host}:{port} accepts this sign-in only over an encrypted connection ({error}). Choose STARTTLS or SSL/TLS."
        )),
        Some(_) if error.is_transient() => CoreError::Smtp(format!(
            "{host}:{port} can't check sign-ins right now ({error})"
        )),
        _ => map_smtp_error(cfg, error),
    }
}

/// What `auth` signs in with, and the mechanisms the account allows, in
/// order of preference. `None` signs in not at all.
fn smtp_auth(auth: &SmtpAuth, cfg: &AccountConfig) -> Option<(Credentials, Vec<Mechanism>)> {
    match auth {
        SmtpAuth::Password(password) => Some((
            Credentials::new(cfg.smtp_username().to_owned(), password.clone()),
            match cfg.settings.connection.smtp_login {
                LoginMethod::Plain => vec![Mechanism::Plain],
                LoginMethod::Login => vec![Mechanism::Login],
                LoginMethod::Automatic | LoginMethod::None => {
                    vec![Mechanism::Plain, Mechanism::Login]
                }
            },
        )),
        SmtpAuth::XOAuth2(token) => Some((
            Credentials::new(cfg.username.clone(), token.clone()),
            vec![Mechanism::Xoauth2],
        )),
        SmtpAuth::None => None,
    }
}

async fn tls_handshake(cfg: &AccountConfig, tcp: TcpStream) -> Result<MailStream> {
    let connector = net::tls::connector(&cfg.settings.connection.trusted_certificate_pem)?;
    let tls = net::tls::handshake(&connector, tcp, &cfg.smtp_host)
        .await
        .map_err(|error| {
            CoreError::Tls(format!(
                "SMTP {}:{}: {error}. {}",
                cfg.smtp_host,
                cfg.smtp_port,
                net::tls::handshake_advice(&error)
            ))
        })?;
    Ok(MailStream::Tls(Box::new(tls)))
}

/// Upgrade with STARTTLS before lettre takes over. A server that does not
/// offer the upgrade is never sent credentials.
async fn start_tls(cfg: &AccountConfig, mut tcp: TcpStream) -> Result<MailStream> {
    let greeting = read_smtp_response(&mut tcp, "greeting").await?;
    expect_smtp_status(&greeting, "220", "greeting")?;
    tcp.write_all(b"EHLO flectar.local\r\n")
        .await
        .map_err(|error| CoreError::Smtp(format!("SMTP EHLO: {error}")))?;
    let capabilities = read_smtp_response(&mut tcp, "EHLO").await?;
    expect_smtp_status(&capabilities, "250", "EHLO")?;
    if !capabilities.lines().any(|line| {
        line.get(4..)
            .and_then(|text| text.split_ascii_whitespace().next())
            .is_some_and(|capability| capability.eq_ignore_ascii_case("STARTTLS"))
    }) {
        return Err(CoreError::Tls(format!(
            "SMTP {}:{} does not advertise STARTTLS",
            cfg.smtp_host, cfg.smtp_port
        )));
    }
    tcp.write_all(b"STARTTLS\r\n")
        .await
        .map_err(|error| CoreError::Smtp(format!("SMTP STARTTLS: {error}")))?;
    let response = read_smtp_response(&mut tcp, "STARTTLS").await?;
    expect_smtp_status(&response, "220", "STARTTLS")?;
    tls_handshake(cfg, tcp).await
}

/// Open an authenticated SMTP session as the account's security requires.
async fn connect(
    cfg: &AccountConfig,
    auth: &SmtpAuth,
    gate: Arc<Gate>,
) -> Result<AsyncSmtpConnection> {
    let security = cfg.settings.connection.smtp_security;
    if !security.is_encrypted() && matches!(auth, SmtpAuth::XOAuth2(_)) {
        return Err(CoreError::Tls(
            "OAuth sign-in needs an encrypted connection.".into(),
        ));
    }
    let open = async {
        let scheme = if security == ConnectionSecurity::Tls {
            "smtps"
        } else {
            "smtp"
        };
        let tcp = net::proxy::connect_for(cfg.proxy_owner(), scheme, &cfg.smtp_host, cfg.smtp_port)
            .await
            .map_err(|error| {
                CoreError::Smtp(format!(
                    "could not connect to {}:{}: {error}",
                    cfg.smtp_host, cfg.smtp_port
                ))
            })?;
        let (stream, greeting) = match security {
            ConnectionSecurity::Tls => (tls_handshake(cfg, tcp).await?, b"".as_slice()),
            ConnectionSecurity::Starttls => (
                start_tls(cfg, tcp).await?,
                b"220 Flectar STARTTLS ready\r\n".as_slice(),
            ),
            ConnectionSecurity::None => (MailStream::Plain(tcp), b"".as_slice()),
        };
        let encrypted = stream.is_encrypted();
        let mut connection = AsyncSmtpConnection::connect_with_transport(
            Box::new(SmtpStream::new(stream, greeting, gate)),
            &ClientId::default(),
        )
        .await
        .map_err(|error| map_smtp_error(cfg, error))?;
        let Some((credentials, mechanisms)) = smtp_auth(auth, cfg) else {
            return Ok(connection);
        };
        let offers =
            |mechanism: &Mechanism| connection.server_info().supports_auth_mechanism(*mechanism);
        if matches!(auth, SmtpAuth::Password(_)) && !mechanisms.iter().any(offers) {
            let offers_password = [Mechanism::Plain, Mechanism::Login].iter().any(offers);
            let (host, port) = (&cfg.smtp_host, cfg.smtp_port);
            let message = if offers_password {
                format!(
                    "{host}:{port} doesn't offer {} sign-in. Choose another sign-in method.",
                    mechanisms[0]
                )
            } else if !encrypted {
                // Servers commonly offer sign-in only after STARTTLS. Say so
                // instead of reporting a failed sign-in.
                format!(
                    "{host}:{port} accepts passwords only over an encrypted connection. Choose STARTTLS or SSL/TLS."
                )
            } else {
                format!(
                    "{host}:{port} offers no sign-in with a password. If it sends mail without one, choose No sign-in."
                )
            };
            connection.abort().await;
            return Err(CoreError::Smtp(message));
        }
        connection
            .auth(&mechanisms, &credentials)
            .await
            .map_err(|error| map_smtp_sign_in_error(cfg, auth, error))?;
        Ok(connection)
    };

    let timeout = net::timeouts::stretch(SMTP_TIMEOUT, cfg.settings.connection.server_timeout());
    tokio::time::timeout(timeout, open).await.map_err(|_| {
        CoreError::Smtp(format!(
            "SMTP {}:{} connection timed out after {}s",
            cfg.smtp_host,
            cfg.smtp_port,
            timeout.as_secs()
        ))
    })?
}

/// Send a fully built RFC 5322 message.
pub async fn send_raw(
    cfg: &AccountConfig,
    auth: &SmtpAuth,
    from: &str,
    recipients: &[String],
    raw: &[u8],
) -> Result<()> {
    send_raw_from(cfg, auth, Some(from), recipients, raw).await
}

/// Send a fully built message that must have gone out by `deadline`, in
/// milliseconds since the epoch on this device's clock. A message not
/// complete by then is never finished, so it doesn't go out: another of the
/// user's devices may then send it without delivering a second copy.
pub async fn send_raw_before(
    cfg: &AccountConfig,
    auth: &SmtpAuth,
    from: &str,
    recipients: &[String],
    raw: &[u8],
    deadline: Option<i64>,
) -> Result<()> {
    let deadline = deadline.map(|deadline| {
        let left = deadline.saturating_sub(crate::models::now_ms()).max(0);
        Instant::now() + Duration::from_millis(u64::try_from(left).unwrap_or(0))
    });
    send_message(
        cfg,
        auth,
        Some(from),
        recipients,
        raw,
        Gate::until(deadline, cfg.settings.connection.server_timeout()),
    )
    .await
}

/// Send a fully built message with `from` as the envelope sender, or the
/// null one (`MAIL FROM:<>`) that notifications such as read receipts use,
/// so nothing answers them in turn.
pub async fn send_raw_from(
    cfg: &AccountConfig,
    auth: &SmtpAuth,
    from: Option<&str>,
    recipients: &[String],
    raw: &[u8],
) -> Result<()> {
    let gate = Gate::until(None, cfg.settings.connection.server_timeout());
    send_message(cfg, auth, from, recipients, raw, gate).await
}

async fn send_message(
    cfg: &AccountConfig,
    auth: &SmtpAuth,
    from: Option<&str>,
    recipients: &[String],
    raw: &[u8],
    gate: Arc<Gate>,
) -> Result<()> {
    let from_addr = from
        .map(|from| {
            from.parse()
                .map_err(|e| CoreError::Smtp(format!("bad from address: {e}")))
        })
        .transpose()?;
    let mut tos = Vec::with_capacity(recipients.len());
    for r in recipients {
        tos.push(
            r.parse()
                .map_err(|e| CoreError::Smtp(format!("bad recipient {r}: {e}")))?,
        );
    }
    let envelope = Envelope::new(from_addr, tos)
        .map_err(|e| CoreError::Smtp(format!("Invalid message envelope: {e}")))?;

    tracing::debug!(
        host = %cfg.smtp_host,
        port = cfg.smtp_port,
        auth = match auth {
            SmtpAuth::Password(_) => "password",
            SmtpAuth::XOAuth2(_) => "xoauth2",
            SmtpAuth::None => "none",
        },
        bytes = raw.len(),
        recipients = recipients.len(),
        "smtp send_raw: connecting to relay",
    );
    let message = crate::mail_security::without_bcc_for_delivery(raw)?;
    let mut connection = connect(cfg, auth, Arc::clone(&gate)).await?;
    submit(cfg, &mut connection, &envelope, &message, &gate).await?;
    // The message is accepted; a failed QUIT changes nothing about that.
    connection.quit().await.ok();
    Ok(())
}

/// The message in parts, noting when the last one has gone out.
struct Parts<'a> {
    rest: &'a [u8],
    gate: &'a Gate,
}

impl<'a> Iterator for Parts<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.rest.is_empty() {
            self.gate.ended.store(true, Ordering::Release);
            return None;
        }
        let (part, rest) = self.rest.split_at(self.rest.len().min(PART_BYTES));
        self.rest = rest;
        Some(part)
    }
}

/// Hand one message to the server, step by step, so a failure tells whether
/// the server can have taken it. Up to the end-of-data marker it can't: a
/// refused command, a dropped connection or a stop at the deadline all leave
/// the transaction unfinished, which RFC 5321 has the server discard, and so
/// does a refusing reply at the end. Only a reply that never comes after the
/// marker leaves it open (RFC 5321 section 4.5.3.2.6); that is
/// [`CoreError::SendUncertain`], which must not be retried as if nothing went
/// out.
async fn submit(
    cfg: &AccountConfig,
    connection: &mut AsyncSmtpConnection,
    envelope: &Envelope,
    message: &[u8],
    gate: &Gate,
) -> Result<()> {
    let not_sent = |error| map_smtp_error(cfg, error);
    let mut options = Vec::new();
    // As lettre's own send: SMTPUTF8 (RFC 6531) for addresses beyond ASCII,
    // 8BITMIME (RFC 6152) for content beyond it.
    if envelope
        .from()
        .into_iter()
        .chain(envelope.to())
        .any(|address| !AsRef::<str>::as_ref(address).is_ascii())
    {
        if !connection
            .server_info()
            .supports_feature(Extension::SmtpUtfEight)
        {
            connection.abort().await;
            return Err(CoreError::Smtp(format!(
                "{}:{} doesn't accept addresses beyond ASCII (no SMTPUTF8)",
                cfg.smtp_host, cfg.smtp_port
            )));
        }
        options.push(MailParameter::SmtpUtfEight);
    }
    if !message.is_ascii() {
        if !connection
            .server_info()
            .supports_feature(Extension::EightBitMime)
        {
            connection.abort().await;
            return Err(CoreError::Smtp(format!(
                "{}:{} doesn't accept 8-bit content (no 8BITMIME)",
                cfg.smtp_host, cfg.smtp_port
            )));
        }
        options.push(MailParameter::Body(MailBodyParameter::EightBitMime));
    }
    if let Err(error) = connection
        .command(Mail::new(envelope.from().cloned(), options))
        .await
    {
        connection.abort().await;
        return Err(not_sent(error));
    }
    for recipient in envelope.to() {
        if let Err(error) = connection
            .command(Rcpt::new(recipient.clone(), vec![]))
            .await
        {
            connection.abort().await;
            return Err(not_sent(error));
        }
    }
    match connection.command(Data).await {
        Ok(reply) if reply.code().severity == Severity::PositiveIntermediate => {}
        Ok(reply) => {
            connection.abort().await;
            return Err(CoreError::Smtp(format!(
                "{}:{} answered DATA with {}",
                cfg.smtp_host,
                cfg.smtp_port,
                reply.code()
            )));
        }
        Err(error) => {
            connection.abort().await;
            return Err(not_sent(error));
        }
    }
    let parts = Parts {
        rest: message,
        gate,
    };
    let uncertain = |what: String| {
        CoreError::SendUncertain(format!(
            "{}:{} may have accepted the message, but {what}; check Sent before sending it again",
            cfg.smtp_host, cfg.smtp_port
        ))
    };
    match connection.message_iter(parts).await {
        Ok(reply) if reply.code().severity == Severity::PositiveCompletion => Ok(()),
        Ok(reply) => Err(uncertain(format!("it answered with {}", reply.code()))),
        // The message never went out whole.
        Err(error)
            if !gate.ended.load(Ordering::Acquire) || gate.refused.load(Ordering::Acquire) =>
        {
            connection.abort().await;
            Err(not_sent(error))
        }
        // The server refused the finished message.
        Err(error) if error.is_transient() || error.is_permanent() => Err(not_sent(error)),
        Err(error) => Err(uncertain(format!("its reply was lost ({error})"))),
    }
}

/// Cheap connectivity/auth probe used by test_connection.
pub async fn test_connection(cfg: &AccountConfig, auth: &SmtpAuth) -> Result<()> {
    let gate = Gate::until(None, cfg.settings.connection.server_timeout());
    let mut connection = connect(cfg, auth, gate).await?;
    connection
        .command(Noop)
        .await
        .map_err(|error| map_smtp_error(cfg, error))?;
    connection.quit().await.ok();
    Ok(())
}

#[cfg(test)]
mod tests;
