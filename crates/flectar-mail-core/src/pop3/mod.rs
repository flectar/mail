//! POP3 client: RFC 1939 with the CAPA, RESP-CODES, AUTH-RESP-CODE,
//! LOGIN-DELAY, PIPELINING, and EXPIRE extensions (RFC 2449, RFC 3206), STLS
//! (RFC 2595), SASL PLAIN (RFC 5034), and UTF8 (RFC 6856).
//!
//! A session holds the server's maildrop lock until it ends. Ending it with
//! [`Session::quit`] commits deletions, and on servers that expire retrieved
//! mail (`EXPIRE 0`) it also lets the server remove every message retrieved
//! in the session. Dropping a session without QUIT leaves the maildrop as it
//! was, so callers end with QUIT only after storing what they retrieved.

mod response;
#[cfg(test)]
mod tests;

pub use response::ResponseCode;

use crate::models::ConnectionSecurity;
use crate::net::{self, MailStream};
use base64::Engine as _;
use response::Status;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// Upper bound for one response line. RFC 2449 allows 512 octets for a
/// status line; real servers send longer listing and capability lines.
const MAX_LINE: usize = 8 * 1024;
/// Commands are limited to 255 octets including CRLF (RFC 2449 section 4).
const MAX_COMMAND: usize = 255;
/// Bound for CAPA, UIDL, and LIST responses.
const MAX_LISTING: usize = 64 * 1024 * 1024;
/// A session is abandoned when the server sends nothing for this long.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound for the connection, TLS, greeting, and sign-in sequence.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Requests sent before reading their responses when the server pipelines.
const PIPELINE_DEPTH: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("the server did not respond for {} seconds", .0.as_secs())]
    Timeout(Duration),
    #[error("{0}")]
    Tls(String),
    #[error("unexpected server response: {0}")]
    Protocol(String),
    #[error("{command} was refused{}", detail(.text))]
    Refused {
        command: &'static str,
        code: Option<ResponseCode>,
        text: String,
    },
    #[error("the sign-in was refused{}", detail(.text))]
    SignIn {
        refusal: SignInRefusal,
        text: String,
    },
    #[error("the server accepts no password on this connection")]
    NoPasswordSignIn,
    /// A line break or NUL would corrupt the commands, and USER and PASS
    /// must fit the 255-octet command limit.
    #[error("the user name or password cannot be sent to a POP3 server")]
    UnsendableCredentials,
    #[error("the response is larger than {0} bytes")]
    TooLarge(usize),
    #[error("the session ended after an earlier error")]
    Ended,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Why the server refused a sign-in, from its response code (RFC 2449,
/// RFC 3206).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInRefusal {
    /// The user name or password was rejected.
    Credentials,
    /// The credentials were accepted, but another session holds the
    /// maildrop.
    InUse,
    /// The previous sign-in was too recent.
    LoginDelay,
    /// A server problem that should pass without the user's help. A refusal
    /// that rules out the credentials but gives no reason counts as one.
    Temporary,
    /// A server problem that needs the provider's support.
    Permanent,
    /// No reason given, so the credentials may be at fault.
    Unspecified,
}

/// How long the server keeps retrieved mail (RFC 2449 section 6.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expire {
    Never,
    /// Retrieved messages may be removed this many days later. Zero means
    /// they are removed when the session that retrieved them ends with QUIT.
    Days(u32),
}

/// What the server announced with CAPA. A server without CAPA supports only
/// the commands RFC 1939 requires, so every optional flag reads `false`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// CAPA succeeded, so a capability that is not listed is unsupported.
    pub announced: bool,
    pub top: bool,
    pub uidl: bool,
    pub user: bool,
    pub pipelining: bool,
    /// Credentials problems carry the AUTH response code, so a refusal
    /// without it has another cause (RFC 3206).
    pub auth_resp_code: bool,
    pub stls: bool,
    /// UTF-8 mode, in which internationalized messages arrive unchanged.
    pub utf8: bool,
    /// SASL mechanisms, in upper case.
    pub sasl: Vec<String>,
    /// Minimum time between sign-ins, when the server enforces one.
    pub login_delay: Option<Duration>,
    pub expire: Option<Expire>,
}

impl Capabilities {
    fn parse(lines: &[Vec<u8>]) -> Self {
        let mut capabilities = Self {
            announced: true,
            ..Self::default()
        };
        for line in lines {
            let line = String::from_utf8_lossy(line);
            let mut words = line.split_ascii_whitespace();
            let Some(name) = words.next() else {
                continue;
            };
            let argument = words.next();
            match name.to_ascii_uppercase().as_str() {
                "TOP" => capabilities.top = true,
                "UIDL" => capabilities.uidl = true,
                "USER" => capabilities.user = true,
                "PIPELINING" => capabilities.pipelining = true,
                "AUTH-RESP-CODE" => capabilities.auth_resp_code = true,
                "STLS" => capabilities.stls = true,
                "UTF8" => capabilities.utf8 = true,
                "SASL" => {
                    capabilities.sasl = argument
                        .into_iter()
                        .chain(words)
                        .map(str::to_ascii_uppercase)
                        .collect();
                }
                // A `USER` argument announces a per-user value that CAPA
                // reports again after sign-in.
                "LOGIN-DELAY" => {
                    capabilities.login_delay = argument
                        .and_then(|seconds| seconds.parse().ok())
                        .map(Duration::from_secs);
                }
                "EXPIRE" => {
                    capabilities.expire = argument.and_then(|value| {
                        if value.eq_ignore_ascii_case("NEVER") {
                            Some(Expire::Never)
                        } else {
                            value.parse().ok().map(Expire::Days)
                        }
                    });
                }
                _ => {}
            }
        }
        capabilities
    }
}

/// Where and how to reach the server.
pub struct Server<'a> {
    pub host: &'a str,
    pub port: u16,
    pub security: ConnectionSecurity,
    /// Additional trusted certificates of the account, as PEM.
    pub trusted_certificate_pem: &'a str,
}

/// An authenticated session in the TRANSACTION state.
pub struct Session {
    stream: BufReader<MailStream>,
    capabilities: Capabilities,
    /// A response was cut short or the connection failed; the next bytes
    /// cannot be trusted to start a response.
    ended: bool,
}

impl Session {
    /// Connect, secure the connection as configured, and sign in.
    pub async fn connect(server: &Server<'_>, user: &str, password: &str) -> Result<Self> {
        tokio::time::timeout(CONNECT_TIMEOUT, Self::open(server, user, password))
            .await
            .map_err(|_| Error::Timeout(CONNECT_TIMEOUT))?
    }

    async fn open(server: &Server<'_>, user: &str, password: &str) -> Result<Self> {
        let connector = net::tls::connector(server.trusted_certificate_pem)
            .map_err(|error| Error::Tls(error.to_string()))?;
        let tcp = net::connect_tcp(server.host, server.port).await?;
        let stream = match server.security {
            ConnectionSecurity::Tls => tls_handshake(&connector, tcp, server.host).await?,
            ConnectionSecurity::Starttls | ConnectionSecurity::None => MailStream::Plain(tcp),
        };
        let mut session = Self::new(stream);
        session.expect_ok("greeting").await?;
        if server.security == ConnectionSecurity::Starttls {
            session = session.start_tls(&connector, server.host).await?;
        }
        session.capabilities = session.query_capabilities().await?;
        session.sign_in(user, password).await?;
        // Values such as EXPIRE can depend on the user and are only final
        // after sign-in.
        let capabilities = session.query_capabilities().await?;
        if capabilities.announced {
            session.capabilities = capabilities;
        }
        Ok(session)
    }

    fn new(stream: MailStream) -> Self {
        Self {
            stream: BufReader::new(stream),
            capabilities: Capabilities::default(),
            ended: false,
        }
    }

    /// Upgrade with STLS before anything else is sent. A refusal ends the
    /// attempt, so credentials never travel unencrypted.
    async fn start_tls(mut self, connector: &TlsConnector, host: &str) -> Result<Self> {
        self.send_line("STLS").await?;
        self.expect_ok("STLS").await?;
        // Bytes buffered before the handshake could have been injected by an
        // attacker; RFC 2595 requires discarding them, and a correct server
        // never sends any.
        if !self.stream.buffer().is_empty() {
            return Err(Error::Protocol(
                "data arrived before the TLS handshake".into(),
            ));
        }
        let MailStream::Plain(tcp) = self.stream.into_inner() else {
            return Err(Error::Tls("STLS needs an unencrypted connection".into()));
        };
        Ok(Self::new(tls_handshake(connector, tcp, host).await?))
    }

    async fn query_capabilities(&mut self) -> Result<Capabilities> {
        self.send_line("CAPA").await?;
        match self.read_status().await? {
            Status::Ok(_) => Ok(Capabilities::parse(&self.read_lines(MAX_LISTING).await?)),
            // RFC 1939 servers predate CAPA.
            Status::Err(..) => Ok(Capabilities::default()),
        }
    }

    /// SASL PLAIN when offered, otherwise USER and PASS. Credentials travel
    /// as UTF-8 either way.
    async fn sign_in(&mut self, user: &str, password: &str) -> Result<()> {
        if [user, password]
            .iter()
            .any(|value| value.contains(['\r', '\n', '\0']))
        {
            return Err(Error::UnsendableCredentials);
        }
        let plain = self
            .capabilities
            .sasl
            .iter()
            .any(|mechanism| mechanism == "PLAIN");
        if !plain && self.capabilities.announced && !self.capabilities.user {
            return Err(Error::NoPasswordSignIn);
        }
        // Without UTF-8 mode, the server may replace internationalized
        // messages with a surrogate or refuse them. The mode can only be
        // entered before signing in, and the server may decline it.
        if self.capabilities.utf8 {
            self.send_line("UTF8").await?;
            self.read_status().await?;
        }
        if plain {
            return self.authenticate_plain(user, password).await;
        }
        let commands = [format!("USER {user}"), format!("PASS {password}")];
        if commands
            .iter()
            .any(|command| command.len() + 2 > MAX_COMMAND)
        {
            return Err(Error::UnsendableCredentials);
        }
        for command in &commands {
            self.send_line(command).await?;
            match self.read_status().await? {
                Status::Ok(_) => {}
                Status::Err(code, text) => return Err(self.refused_sign_in(code, text)),
            }
        }
        Ok(())
    }

    async fn authenticate_plain(&mut self, user: &str, password: &str) -> Result<()> {
        let response =
            base64::engine::general_purpose::STANDARD.encode(format!("\0{user}\0{password}"));
        let inline = format!("AUTH PLAIN {response}");
        // An initial response that would make the command longer than 255
        // octets follows the server's empty challenge instead (RFC 5034).
        let sent_inline = inline.len() + 2 <= MAX_COMMAND;
        self.send_line(if sent_inline { &inline } else { "AUTH PLAIN" })
            .await?;
        let mut line = self.read_line().await?;
        if !sent_inline && is_continuation(&line) {
            self.send_line(&response).await?;
            line = self.read_line().await?;
        }
        if is_continuation(&line) {
            // PLAIN completes in one step; cancel any other challenge.
            self.send_line("*").await?;
            self.read_status().await?;
            return Err(Error::Protocol("unexpected SASL challenge".into()));
        }
        match response::parse_status(&line) {
            Some(Status::Ok(_)) => Ok(()),
            Some(Status::Err(code, text)) => Err(self.refused_sign_in(code, text)),
            None => Err(self.unexpected("AUTH", &line)),
        }
    }

    fn refused_sign_in(&self, code: Option<ResponseCode>, text: String) -> Error {
        let refusal = match code {
            Some(ResponseCode::Auth) => SignInRefusal::Credentials,
            Some(ResponseCode::InUse) => SignInRefusal::InUse,
            Some(ResponseCode::LoginDelay) => SignInRefusal::LoginDelay,
            Some(ResponseCode::SysTemp) => SignInRefusal::Temporary,
            Some(ResponseCode::SysPerm) => SignInRefusal::Permanent,
            // These servers mark every credentials problem with AUTH.
            _ if self.capabilities.auth_resp_code => SignInRefusal::Temporary,
            _ => SignInRefusal::Unspecified,
        };
        Error::SignIn { refusal, text }
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// Message numbers and unique ids of every message (RFC 1939 UIDL).
    pub async fn unique_ids(&mut self) -> Result<Vec<(u32, String)>> {
        self.send_line("UIDL").await?;
        self.expect_ok("UIDL").await?;
        let lines = self.read_lines(MAX_LISTING).await?;
        lines
            .iter()
            .map(|line| response::parse_unique_id(line))
            .collect()
    }

    /// Message numbers and sizes in octets of every message.
    pub async fn sizes(&mut self) -> Result<Vec<(u32, u64)>> {
        self.send_line("LIST").await?;
        self.expect_ok("LIST").await?;
        let lines = self.read_lines(MAX_LISTING).await?;
        lines
            .iter()
            .map(|line| response::parse_size(line))
            .collect()
    }

    /// The header section of each message, with requests pipelined when the
    /// server allows it. `None` marks a message the server refused, for
    /// example because another client deleted it, or whose header section is
    /// larger than `limit`.
    pub async fn headers(
        &mut self,
        messages: &[u32],
        limit: usize,
    ) -> Result<Vec<Option<Vec<u8>>>> {
        let depth = if self.capabilities.pipelining {
            PIPELINE_DEPTH
        } else {
            1
        };
        let mut sections = Vec::with_capacity(messages.len());
        for chunk in messages.chunks(depth) {
            for message in chunk {
                self.send_line(&format!("TOP {message} 0")).await?;
            }
            for _ in chunk {
                sections.push(match self.read_status().await? {
                    Status::Ok(_) => self.read_data_or_skip(limit).await?,
                    Status::Err(..) => None,
                });
            }
        }
        Ok(sections)
    }

    /// The complete message as the server stores it, up to `limit` octets.
    /// A larger message ends the session.
    pub async fn retrieve(&mut self, message: u32, limit: usize) -> Result<Vec<u8>> {
        self.send_line(&format!("RETR {message}")).await?;
        self.expect_ok("RETR").await?;
        self.read_data(limit).await
    }

    /// Mark a message for deletion when the session ends with QUIT.
    pub async fn delete(&mut self, message: u32) -> Result<()> {
        self.send_line(&format!("DELE {message}")).await?;
        self.expect_ok("DELE").await.map(drop)
    }

    /// End the session and commit its deletions. After an error the server
    /// may or may not have committed them.
    pub async fn quit(mut self) -> Result<()> {
        self.send_line("QUIT").await?;
        self.expect_ok("QUIT").await.map(drop)
    }

    async fn send_line(&mut self, line: &str) -> Result<()> {
        if self.ended {
            return Err(Error::Ended);
        }
        // Only credentials come from outside, and they must never be able
        // to start a second command.
        if line.contains(['\r', '\n', '\0']) {
            return Err(Error::UnsendableCredentials);
        }
        let stream = self.stream.get_mut();
        let write = async {
            stream.write_all(line.as_bytes()).await?;
            stream.write_all(b"\r\n").await?;
            stream.flush().await
        };
        match tokio::time::timeout(IDLE_TIMEOUT, write).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.ended = true;
                Err(error.into())
            }
            Err(_) => {
                self.ended = true;
                Err(Error::Timeout(IDLE_TIMEOUT))
            }
        }
    }

    /// One line including its terminator, at most `limit` octets.
    async fn read_raw_line(&mut self, limit: usize) -> Result<Vec<u8>> {
        if self.ended {
            return Err(Error::Ended);
        }
        let mut line = Vec::new();
        loop {
            let available = match tokio::time::timeout(IDLE_TIMEOUT, self.stream.fill_buf()).await {
                Ok(Ok(available)) if !available.is_empty() => available,
                Ok(Ok(_)) => {
                    self.ended = true;
                    return Err(Error::Io(std::io::ErrorKind::UnexpectedEof.into()));
                }
                Ok(Err(error)) => {
                    self.ended = true;
                    return Err(error.into());
                }
                Err(_) => {
                    self.ended = true;
                    return Err(Error::Timeout(IDLE_TIMEOUT));
                }
            };
            let (length, complete) = match available.iter().position(|&byte| byte == b'\n') {
                Some(end) => (end + 1, true),
                None => (available.len(), false),
            };
            if line.len() + length > limit {
                self.ended = true;
                return Err(Error::TooLarge(limit));
            }
            line.extend_from_slice(&available[..length]);
            self.stream.consume(length);
            if complete {
                return Ok(line);
            }
        }
    }

    /// One response line without its terminator.
    async fn read_line(&mut self) -> Result<Vec<u8>> {
        let line = self.read_raw_line(MAX_LINE).await?;
        Ok(line_content(&line).to_vec())
    }

    async fn read_status(&mut self) -> Result<Status> {
        let line = self.read_line().await?;
        response::parse_status(&line).ok_or_else(|| self.unexpected("response", &line))
    }

    async fn expect_ok(&mut self, command: &'static str) -> Result<String> {
        match self.read_status().await? {
            Status::Ok(text) => Ok(text),
            Status::Err(code, text) => Err(Error::Refused {
                command,
                code,
                text,
            }),
        }
    }

    /// The lines of a multi-line response, terminators dropped and
    /// dot-stuffing removed.
    async fn read_lines(&mut self, limit: usize) -> Result<Vec<Vec<u8>>> {
        let mut lines = Vec::new();
        let mut total = 0;
        loop {
            let line = self.read_raw_line(MAX_LINE).await?;
            let content = line_content(&line);
            if content == b"." {
                return Ok(lines);
            }
            let content = content.strip_prefix(b".").unwrap_or(content);
            total += content.len();
            if total > limit {
                self.ended = true;
                return Err(Error::TooLarge(limit));
            }
            lines.push(content.to_vec());
        }
    }

    /// A multi-line response as message data: dot-stuffing removed, each
    /// line's own terminator kept, at most `limit` octets.
    async fn read_data(&mut self, limit: usize) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        loop {
            // The terminating ".\r\n" must fit even when the data used the
            // whole limit.
            let room = limit.saturating_sub(data.len()).max(3);
            let line = match self.read_raw_line(room).await {
                Err(Error::TooLarge(_)) => return Err(Error::TooLarge(limit)),
                line => line?,
            };
            if line_content(&line) == b"." {
                return Ok(data);
            }
            let line = line.strip_prefix(b".").unwrap_or(&line);
            if data.len() + line.len() > limit {
                self.ended = true;
                return Err(Error::TooLarge(limit));
            }
            data.extend_from_slice(line);
        }
    }

    /// [`Self::read_data`] for responses that may be skipped: data larger
    /// than `limit` is read to its end and dropped, so the session stays
    /// usable. Only a response beyond [`MAX_LISTING`] ends it.
    async fn read_data_or_skip(&mut self, limit: usize) -> Result<Option<Vec<u8>>> {
        let mut data = Some(Vec::new());
        let mut total = 0usize;
        loop {
            let line = match self.read_raw_line(MAX_LISTING - total).await {
                Err(Error::TooLarge(_)) => return Err(Error::TooLarge(MAX_LISTING)),
                line => line?,
            };
            if line_content(&line) == b"." {
                return Ok(data);
            }
            total += line.len();
            let line = line.strip_prefix(b".").unwrap_or(&line);
            if let Some(kept) = &mut data {
                if kept.len() + line.len() > limit {
                    data = None;
                } else {
                    kept.extend_from_slice(line);
                }
            }
        }
    }

    fn unexpected(&mut self, command: &str, line: &[u8]) -> Error {
        self.ended = true;
        Error::Protocol(format!(
            "{command}: {}",
            String::from_utf8_lossy(&line[..line.len().min(200)])
        ))
    }
}

async fn tls_handshake(connector: &TlsConnector, tcp: TcpStream, host: &str) -> Result<MailStream> {
    let tls = net::tls::handshake(connector, tcp, host)
        .await
        .map_err(|error| Error::Tls(error.to_string()))?;
    Ok(MailStream::Tls(Box::new(tls)))
}

/// The server's explanation of a refusal, when it gave one.
fn detail(text: &str) -> String {
    if text.is_empty() {
        String::new()
    } else {
        format!(": {text}")
    }
}

/// A SASL continuation: `+` alone or followed by a space.
fn is_continuation(line: &[u8]) -> bool {
    line == b"+" || line.starts_with(b"+ ")
}

/// A line without its CRLF or LF terminator.
fn line_content(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}
