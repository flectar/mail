//! SMTP submission with password or XOAUTH2 sign-in. Connections are opened
//! here and handed to lettre's protocol client, so every security mode uses
//! the account's own TLS trust and one code path.

use crate::error::{CoreError, Result};
use crate::models::{AccountConfig, ConnectionSecurity};
use crate::net::{self, MailStream};
use lettre::transport::smtp::{
    authentication::{Credentials, Mechanism},
    client::{AsyncSmtpConnection, AsyncTokioStream},
    commands::Noop,
    extension::ClientId,
};
use std::{
    fmt,
    future::Future,
    net::SocketAddr,
    pin::Pin,
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

pub enum SmtpAuth {
    Password(String),
    XOAuth2(String),
}

/// The stream lettre drives. After STARTTLS the server's greeting has been
/// consumed, so `greeting` replays one for lettre's post-upgrade EHLO. Reads
/// and writes fail once the connection has been idle for [`IDLE_TIMEOUT`].
struct SmtpStream {
    inner: MailStream,
    greeting: &'static [u8],
    idle: Pin<Box<Sleep>>,
}

impl SmtpStream {
    fn new(inner: MailStream, greeting: &'static [u8]) -> Self {
        Self {
            inner,
            greeting,
            idle: Box::pin(tokio::time::sleep(IDLE_TIMEOUT)),
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
                self.idle.as_mut().reset(Instant::now() + IDLE_TIMEOUT);
                Poll::Ready(result)
            }
            Poll::Pending => match self.idle.as_mut().poll(context) {
                Poll::Ready(()) => Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "SMTP connection timed out after {}s",
                        IDLE_TIMEOUT.as_secs()
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
    let message = error.to_string();
    if message.contains("535") || message.to_ascii_lowercase().contains("auth") {
        CoreError::Auth(format!("smtp auth: {message}"))
    } else {
        CoreError::Smtp(format!("{}:{}: {message}", cfg.smtp_host, cfg.smtp_port))
    }
}

fn smtp_auth(auth: &SmtpAuth, username: &str) -> (Credentials, Vec<Mechanism>) {
    match auth {
        SmtpAuth::Password(password) => (
            Credentials::new(username.to_owned(), password.clone()),
            vec![Mechanism::Plain, Mechanism::Login],
        ),
        SmtpAuth::XOAuth2(token) => (
            Credentials::new(username.to_owned(), token.clone()),
            vec![Mechanism::Xoauth2],
        ),
    }
}

async fn tls_handshake(cfg: &AccountConfig, tcp: TcpStream) -> Result<MailStream> {
    let connector = net::tls::connector(&cfg.settings.connection.trusted_certificate_pem)?;
    let tls = net::tls::handshake(&connector, tcp, &cfg.smtp_host)
        .await
        .map_err(|error| {
        CoreError::Tls(format!(
            "SMTP {}:{}: {error}. Check SSL/TLS versus STARTTLS and the server certificate. For Proton Bridge, import its exported certificate.",
            cfg.smtp_host, cfg.smtp_port
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
async fn connect(cfg: &AccountConfig, auth: &SmtpAuth) -> Result<AsyncSmtpConnection> {
    let security = cfg.settings.connection.smtp_security;
    if !security.is_encrypted() && matches!(auth, SmtpAuth::XOAuth2(_)) {
        return Err(CoreError::Tls(
            "OAuth sign-in needs an encrypted connection.".into(),
        ));
    }
    let open = async {
        let tcp = net::connect_tcp(&cfg.smtp_host, cfg.smtp_port)
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
            Box::new(SmtpStream::new(stream, greeting)),
            &ClientId::default(),
        )
        .await
        .map_err(|error| map_smtp_error(cfg, error))?;
        let (credentials, mechanisms) = smtp_auth(auth, &cfg.username);
        // Servers commonly offer sign-in only after STARTTLS. Say so instead
        // of reporting a failed sign-in.
        if !encrypted
            && !mechanisms
                .iter()
                .any(|mechanism| connection.server_info().supports_auth_mechanism(*mechanism))
        {
            connection.abort().await;
            return Err(CoreError::Smtp(format!(
                "{}:{} accepts passwords only over an encrypted connection. Choose STARTTLS or SSL/TLS.",
                cfg.smtp_host, cfg.smtp_port
            )));
        }
        connection
            .auth(&mechanisms, &credentials)
            .await
            .map_err(|error| map_smtp_error(cfg, error))?;
        Ok(connection)
    };

    tokio::time::timeout(SMTP_TIMEOUT, open)
        .await
        .map_err(|_| {
            CoreError::Smtp(format!(
                "SMTP {}:{} connection timed out after {}s",
                cfg.smtp_host,
                cfg.smtp_port,
                SMTP_TIMEOUT.as_secs()
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
    use lettre::address::Envelope;
    let from_addr = from
        .parse()
        .map_err(|e| CoreError::Smtp(format!("bad from address: {e}")))?;
    let mut tos = Vec::with_capacity(recipients.len());
    for r in recipients {
        tos.push(
            r.parse()
                .map_err(|e| CoreError::Smtp(format!("bad recipient {r}: {e}")))?,
        );
    }
    let envelope = Envelope::new(Some(from_addr), tos)
        .map_err(|e| CoreError::Smtp(format!("Invalid message envelope: {e}")))?;

    tracing::debug!(
        host = %cfg.smtp_host,
        port = cfg.smtp_port,
        auth = match auth {
            SmtpAuth::Password(_) => "password",
            SmtpAuth::XOAuth2(_) => "xoauth2",
        },
        bytes = raw.len(),
        recipients = recipients.len(),
        "smtp send_raw: connecting to relay",
    );
    let message = crate::mail_security::without_bcc_for_delivery(raw)?;
    let mut connection = connect(cfg, auth).await?;
    connection
        .send(&envelope, &message)
        .await
        .map_err(|error| map_smtp_error(cfg, error))?;
    // The message is accepted; a failed QUIT changes nothing about that.
    connection.quit().await.ok();
    Ok(())
}

/// Cheap connectivity/auth probe used by test_connection.
pub async fn test_connection(cfg: &AccountConfig, auth: &SmtpAuth) -> Result<()> {
    let mut connection = connect(cfg, auth).await?;
    connection
        .command(Noop)
        .await
        .map_err(|error| map_smtp_error(cfg, error))?;
    connection.quit().await.ok();
    Ok(())
}
