//! ManageSieve client (RFC 5804): the protocol servers such as Dovecot,
//! Cyrus and Stalwart offer for managing a user's Sieve scripts. The
//! connection is secured before the password travels (section 2.2), unless
//! the account's mail travels unencrypted too.

use crate::models::ConnectionSecurity;
use crate::net::{self, MailStream};
use base64::Engine as _;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// The port ManageSieve servers listen on (RFC 5804 section 1.8).
pub(crate) const PORT: u16 = 4190;
/// Bound for one token, such as a script, and for one response line.
const MAX_TOKEN: usize = 1024 * 1024;
/// A session is abandoned when the server sends nothing for this long.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound for the connection, TLS and sign-in.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
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
        /// The response code, such as `QUOTA` or `NONEXISTENT`.
        code: Option<String>,
        text: String,
    },
    #[error("the server ended the session{}", detail(.0))]
    Bye(String),
    #[error("the server offers no secure way to sign in")]
    NoSignIn,
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

fn detail(text: &str) -> String {
    if text.is_empty() {
        String::new()
    } else {
        format!(": {text}")
    }
}

/// Where the ManageSieve server is, and how to reach it.
pub(crate) struct Server<'a> {
    /// Whose proxy settings the connection follows.
    pub(crate) owner: net::proxy::Owner,
    pub(crate) host: &'a str,
    pub(crate) port: u16,
    pub(crate) security: ConnectionSecurity,
    /// Additional trusted certificates of the account, as PEM.
    pub(crate) trusted_certificate_pem: &'a str,
    /// The account's server timeout, which stretches every wait (#252).
    pub(crate) timeout: Duration,
}

/// What the server announced it supports (RFC 5804 section 1.7).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Capabilities {
    pub(crate) implementation: String,
    /// The Sieve extensions scripts may use.
    pub(crate) sieve: Vec<String>,
    pub(crate) sasl: Vec<String>,
    pub(crate) starttls: bool,
}

impl Capabilities {
    fn parse(lines: &[Vec<Token>]) -> Self {
        let mut capabilities = Self::default();
        for line in lines {
            let mut values = line.iter().filter_map(Token::text);
            let (Some(name), value) = (values.next(), values.next()) else {
                continue;
            };
            let words = || {
                value
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            };
            match name.to_ascii_uppercase().as_str() {
                "IMPLEMENTATION" => capabilities.implementation = value.unwrap_or_default().into(),
                "SIEVE" => capabilities.sieve = words(),
                "SASL" => {
                    capabilities.sasl = words()
                        .into_iter()
                        .map(|mechanism| mechanism.to_ascii_uppercase())
                        .collect();
                }
                "STARTTLS" => capabilities.starttls = true,
                _ => {}
            }
        }
        capabilities
    }

    /// Whether scripts may use Sieve extension `name`.
    pub(crate) fn has_extension(&self, name: &str) -> bool {
        self.sieve
            .iter()
            .any(|extension| extension.eq_ignore_ascii_case(name))
    }
}

/// A script on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Script {
    pub(crate) name: String,
    pub(crate) active: bool,
}

/// One unit of a response line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Atom(String),
    /// A quoted string or a literal.
    Text(String),
    Open,
    Close,
    /// The end of the line.
    End,
}

impl Token {
    fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) | Self::Atom(text) => Some(text),
            _ => None,
        }
    }
}

/// How a command ended.
#[derive(Debug, Default)]
struct Done {
    code: Option<String>,
    text: String,
}

/// A signed-in session.
pub(crate) struct Session {
    stream: BufReader<MailStream>,
    capabilities: Capabilities,
    /// The account's server timeout, which stretches every wait (#252).
    timeout: Duration,
}

impl Session {
    /// Connects and secures the connection as `server` asks, without
    /// signing in, and reads what the server offers.
    pub(crate) async fn open(server: &Server<'_>) -> Result<Self> {
        let timeout = crate::net::timeouts::stretch(CONNECT_TIMEOUT, server.timeout);
        tokio::time::timeout(timeout, Self::start(server))
            .await
            .map_err(|_| Error::Timeout(timeout))?
    }

    async fn start(server: &Server<'_>) -> Result<Self> {
        let connector = net::tls::connector(server.trusted_certificate_pem)
            .map_err(|error| Error::Tls(error.to_string()))?;
        let tcp = net::proxy::connect_for(server.owner, "sieve", server.host, server.port).await?;
        let stream = match server.security {
            ConnectionSecurity::Tls => tls_handshake(&connector, tcp, server.host).await?,
            ConnectionSecurity::Starttls | ConnectionSecurity::None => MailStream::Plain(tcp),
        };
        let mut session = Self {
            stream: BufReader::new(stream),
            capabilities: Capabilities::default(),
            timeout: server.timeout,
        };
        session.capabilities = session.read_capabilities().await?;
        if server.security == ConnectionSecurity::Starttls {
            if !session.capabilities.starttls {
                return Err(Error::NoSignIn);
            }
            session = session.start_tls(&connector, server.host).await?;
        }
        Ok(session)
    }

    /// Upgrades the connection before anything else is sent, and reads the
    /// capabilities again, as the server sends them anew (section 2.2).
    async fn start_tls(mut self, connector: &TlsConnector, host: &str) -> Result<Self> {
        self.send("STARTTLS").await?;
        self.finish("STARTTLS").await?;
        // Bytes before the handshake could have been injected by an attacker.
        if !self.stream.buffer().is_empty() {
            return Err(Error::Protocol(
                "data arrived before the TLS handshake".into(),
            ));
        }
        let MailStream::Plain(tcp) = self.stream.into_inner() else {
            return Err(Error::Tls(
                "STARTTLS needs an unencrypted connection".into(),
            ));
        };
        let mut session = Self {
            stream: BufReader::new(tls_handshake(connector, tcp, host).await?),
            capabilities: Capabilities::default(),
            timeout: self.timeout,
        };
        session.capabilities = session.read_capabilities().await?;
        Ok(session)
    }

    /// The capability lines and the OK after them, as the server sends them
    /// on connecting and after STARTTLS.
    async fn read_capabilities(&mut self) -> Result<Capabilities> {
        let (lines, _) = self.response("greeting").await?;
        Ok(Capabilities::parse(&lines))
    }

    /// Signs in with the mail account's user name and password, as PLAIN,
    /// the mechanism every server has (section 2.1).
    pub(crate) async fn sign_in(&mut self, user: &str, password: &str) -> Result<()> {
        let timeout = crate::net::timeouts::stretch(CONNECT_TIMEOUT, self.timeout);
        tokio::time::timeout(timeout, self.authenticate(user, password))
            .await
            .map_err(|_| Error::Timeout(timeout))?
    }

    async fn authenticate(&mut self, user: &str, password: &str) -> Result<()> {
        if !self
            .capabilities
            .sasl
            .iter()
            .any(|offered| offered == "PLAIN")
        {
            return Err(Error::NoSignIn);
        }
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(format!("\0{user}\0{password}"));
        self.send_string("AUTHENTICATE \"PLAIN\"", &encoded).await?;
        // PLAIN needs nothing more, so a challenge is cancelled with "*".
        let first = self.token().await?;
        if matches!(first, Token::Text(_)) {
            self.rest_of_line().await?;
            self.send("\"*\"").await?;
            return self.finish("AUTHENTICATE").await.map(drop);
        }
        self.status(first, "AUTHENTICATE").await.map(drop)
    }

    pub(crate) fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// The user's scripts, and which one is active.
    pub(crate) async fn list_scripts(&mut self) -> Result<Vec<Script>> {
        self.send("LISTSCRIPTS").await?;
        let (lines, _) = self.response("LISTSCRIPTS").await?;
        Ok(lines
            .iter()
            .filter_map(|line| {
                let name = line.first()?.text()?.to_owned();
                let active = line
                    .get(1)
                    .and_then(Token::text)
                    .is_some_and(|word| word.eq_ignore_ascii_case("ACTIVE"));
                Some(Script { name, active })
            })
            .collect())
    }

    pub(crate) async fn get_script(&mut self, name: &str) -> Result<String> {
        self.send(&format!("GETSCRIPT {}", quoted(name)?)).await?;
        let (lines, _) = self.response("GETSCRIPT").await?;
        lines
            .first()
            .and_then(|line| line.first())
            .and_then(Token::text)
            .map(str::to_owned)
            .ok_or_else(|| Error::Protocol("GETSCRIPT gave no script".into()))
    }

    /// Stores `script` as `name`. A failed upload keeps the old script
    /// (section 2.6). The warnings the server gave, to show the user.
    pub(crate) async fn put_script(&mut self, name: &str, script: &str) -> Result<Option<String>> {
        self.send_literal(&format!("PUTSCRIPT {}", quoted(name)?), script)
            .await?;
        Ok(warnings(self.finish("PUTSCRIPT").await?))
    }

    /// Makes `name` the active script, the only one the server runs.
    pub(crate) async fn set_active(&mut self, name: &str) -> Result<()> {
        self.send(&format!("SETACTIVE {}", quoted(name)?)).await?;
        self.finish("SETACTIVE").await.map(drop)
    }

    pub(crate) async fn logout(mut self) -> Result<()> {
        self.send("LOGOUT").await?;
        match self.response("LOGOUT").await {
            Ok(_) | Err(Error::Bye(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn send(&mut self, line: &str) -> Result<()> {
        let stream = self.stream.get_mut();
        stream.write_all(line.as_bytes()).await?;
        stream.write_all(b"\r\n").await?;
        stream.flush().await?;
        Ok(())
    }

    /// `command` followed by `text` as a literal the server takes without
    /// asking for it (section 4).
    async fn send_literal(&mut self, command: &str, text: &str) -> Result<()> {
        let stream = self.stream.get_mut();
        stream
            .write_all(format!("{command} {{{}+}}\r\n", text.len()).as_bytes())
            .await?;
        stream.write_all(text.as_bytes()).await?;
        stream.write_all(b"\r\n").await?;
        stream.flush().await?;
        Ok(())
    }

    /// `command` followed by `text`, quoted while short enough (section 4
    /// limits quoted strings to 1024 octets), else as a literal.
    async fn send_string(&mut self, command: &str, text: &str) -> Result<()> {
        if text.len() <= 1024 {
            self.send(&format!("{command} {}", quoted(text)?)).await
        } else {
            self.send_literal(command, text).await
        }
    }

    /// The lines before a command's OK, and how it ended.
    async fn response(&mut self, command: &'static str) -> Result<(Vec<Vec<Token>>, Done)> {
        let mut lines = Vec::new();
        loop {
            let first = self.token().await?;
            if matches!(&first, Token::Atom(word) if is_status(word)) {
                return Ok((lines, self.status(first, command).await?));
            }
            let mut line = vec![first];
            line.extend(self.rest_of_line().await?);
            lines.push(line);
            if lines.len() > 10_000 {
                return Err(Error::Protocol(format!(
                    "{command} answered too many lines"
                )));
            }
        }
    }

    /// How a command without data ended.
    async fn finish(&mut self, command: &'static str) -> Result<Done> {
        let first = self.token().await?;
        self.status(first, command).await
    }

    /// A status line that starts with `first`: OK, or the refusal NO and BYE
    /// are.
    async fn status(&mut self, first: Token, command: &'static str) -> Result<Done> {
        let Token::Atom(word) = first else {
            return Err(Error::Protocol(format!("{command} gave no status")));
        };
        let rest = self.rest_of_line().await?;
        let mut done = Done::default();
        let mut tokens = rest.into_iter().peekable();
        if tokens.peek() == Some(&Token::Open) {
            tokens.next();
            let mut code = Vec::new();
            for token in tokens.by_ref() {
                match token {
                    Token::Close => break,
                    token => code.extend(token.text().map(str::to_owned)),
                }
            }
            done.code = Some(code.join(" "));
        }
        done.text = tokens
            .filter_map(|token| token.text().map(str::to_owned))
            .collect::<Vec<_>>()
            .join(" ");
        match word.to_ascii_uppercase().as_str() {
            "OK" => Ok(done),
            "NO" => Err(Error::Refused {
                command,
                code: done.code,
                text: done.text,
            }),
            _ => Err(Error::Bye(done.text)),
        }
    }

    /// The tokens up to the end of the line.
    async fn rest_of_line(&mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();
        loop {
            match self.token().await? {
                Token::End => return Ok(tokens),
                token => tokens.push(token),
            }
            if tokens.len() > 1_000 {
                return Err(Error::Protocol("a response line is too long".into()));
            }
        }
    }

    async fn peek(&mut self) -> Result<u8> {
        let buffer = tokio::time::timeout(
            crate::net::timeouts::stretch(IDLE_TIMEOUT, self.timeout),
            self.stream.fill_buf(),
        )
        .await
        .map_err(|_| Error::Timeout(crate::net::timeouts::stretch(IDLE_TIMEOUT, self.timeout)))??;
        buffer.first().copied().ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the server closed the connection",
            ))
        })
    }

    async fn byte(&mut self) -> Result<u8> {
        let byte = self.peek().await?;
        self.stream.consume(1);
        Ok(byte)
    }

    async fn token(&mut self) -> Result<Token> {
        loop {
            match self.byte().await? {
                b' ' => continue,
                b'\r' => {
                    if self.byte().await? != b'\n' {
                        return Err(Error::Protocol("a line ended without LF".into()));
                    }
                    return Ok(Token::End);
                }
                b'\n' => return Ok(Token::End),
                b'(' => return Ok(Token::Open),
                b')' => return Ok(Token::Close),
                b'"' => return self.quoted().await,
                b'{' => return self.literal().await,
                first => {
                    let mut atom = vec![first];
                    while !matches!(self.peek().await?, b' ' | b'\r' | b'\n' | b'(' | b')') {
                        atom.push(self.byte().await?);
                        if atom.len() > MAX_TOKEN {
                            return Err(Error::Protocol("an atom is too long".into()));
                        }
                    }
                    return Ok(Token::Atom(text(atom)?));
                }
            }
        }
    }

    async fn quoted(&mut self) -> Result<Token> {
        let mut bytes = Vec::new();
        loop {
            match self.byte().await? {
                b'"' => return Ok(Token::Text(text(bytes)?)),
                b'\\' => bytes.push(self.byte().await?),
                b'\r' | b'\n' => {
                    return Err(Error::Protocol("a quoted string has a line break".into()));
                }
                byte => bytes.push(byte),
            }
            if bytes.len() > MAX_TOKEN {
                return Err(Error::Protocol("a quoted string is too long".into()));
            }
        }
    }

    /// `{N}` or `{N+}`, a line break, and N octets.
    async fn literal(&mut self) -> Result<Token> {
        let mut digits = String::new();
        loop {
            match self.byte().await? {
                b'}' => break,
                b'+' => {}
                byte @ b'0'..=b'9' if digits.len() < 9 => digits.push(char::from(byte)),
                _ => return Err(Error::Protocol("a literal has a bad length".into())),
            }
        }
        let length: usize = digits
            .parse()
            .map_err(|_| Error::Protocol("a literal has no length".into()))?;
        if length > MAX_TOKEN {
            return Err(Error::Protocol("a literal is too long".into()));
        }
        match self.byte().await? {
            b'\n' => {}
            b'\r' if self.byte().await? == b'\n' => {}
            _ => {
                return Err(Error::Protocol(
                    "a literal's length isn't on a line of its own".into(),
                ));
            }
        }
        let mut bytes = vec![0; length];
        tokio::time::timeout(
            crate::net::timeouts::stretch(IDLE_TIMEOUT, self.timeout),
            self.stream.read_exact(&mut bytes),
        )
        .await
        .map_err(|_| Error::Timeout(crate::net::timeouts::stretch(IDLE_TIMEOUT, self.timeout)))??;
        Ok(Token::Text(text(bytes)?))
    }
}

fn is_status(word: &str) -> bool {
    ["OK", "NO", "BYE"]
        .iter()
        .any(|status| word.eq_ignore_ascii_case(status))
}

fn text(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| Error::Protocol("text that isn't UTF-8".into()))
}

/// `text`, such as a script name, as a quoted string. Quoted strings
/// carry no line breaks, and script names no control characters (section
/// 1.6).
fn quoted(text: &str) -> Result<String> {
    if text.chars().any(char::is_control) {
        return Err(Error::Protocol("a name with control characters".into()));
    }
    Ok(format!(
        "\"{}\"",
        text.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// The server's warnings about a script it took (section 2.6).
fn warnings(done: Done) -> Option<String> {
    done.code
        .is_some_and(|code| code.eq_ignore_ascii_case("WARNINGS"))
        .then_some(done.text)
        .filter(|text| !text.is_empty())
}

async fn tls_handshake(connector: &TlsConnector, tcp: TcpStream, host: &str) -> Result<MailStream> {
    let tls = net::tls::handshake(connector, tcp, host)
        .await
        .map_err(|error| Error::Tls(format!("{error}. {}", net::tls::handshake_advice(&error))))?;
    Ok(MailStream::Tls(Box::new(tls)))
}

#[cfg(test)]
#[path = "managesieve_tests.rs"]
mod tests;
