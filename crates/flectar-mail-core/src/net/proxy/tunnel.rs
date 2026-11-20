//! Tunnels for the mail protocols through a SOCKS5 or HTTP proxy.
//!
//! IMAP, POP3, SMTP and ManageSieve servers speak first: on a plain or
//! STARTTLS port the greeting can arrive in the same packet as the proxy's
//! reply. Both handshakes therefore read exactly the reply and nothing past
//! it, so the greeting stays in the stream for the protocol. curl reads an
//! HTTP CONNECT reply one byte at a time for the same reason. The proxy
//! clients built into HTTP libraries can read ahead, which is harmless for
//! HTTPS, where the client speaks first.

use super::{Credentials, ProxyError, ProxyKind, ProxyServer};
use base64::Engine;
use std::net::IpAddr;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The longest reply head an HTTP proxy may send before its blank line.
const MAX_CONNECT_REPLY: usize = 16 * 1024;

/// Ask the proxy `server`, already connected through `stream`, for a tunnel
/// to `host`:`port`. On success the stream carries the tunnel.
pub(super) async fn open<S>(
    stream: &mut S,
    server: &ProxyServer,
    host: &str,
    port: u16,
) -> Result<(), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let target = Target::parse(host)?;
    match server.kind {
        ProxyKind::Socks5 => socks5(stream, server, &target, port).await,
        ProxyKind::Http => http_connect(stream, server, &target, port).await,
        ProxyKind::Socks4a => socks4a(stream, server, &target, port).await,
    }
}

/// Whether a proxy can be asked to connect to `host`: a valid server name or
/// address.
pub(super) fn check_name(host: &str) -> Result<(), ProxyError> {
    Target::parse(host).map(|_| ())
}

/// The server to reach: a name the proxy resolves, or an address.
enum Target {
    Name(String),
    Address(IpAddr),
}

impl Target {
    fn parse(host: &str) -> Result<Self, ProxyError> {
        let bare = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        if let Ok(address) = bare.parse::<IpAddr>() {
            return Ok(Self::Address(address));
        }
        // The proxy resolves the name, so send it the way DNS spells it:
        // internationalized names as their ASCII (punycode) form.
        match url::Host::parse(host) {
            Ok(url::Host::Domain(name)) if !name.is_empty() => Ok(Self::Name(name)),
            Ok(url::Host::Ipv4(address)) => Ok(Self::Address(address.into())),
            Ok(url::Host::Ipv6(address)) => Ok(Self::Address(address.into())),
            _ => Err(ProxyError::Settings(format!(
                "\u{201c}{host}\u{201d} isn't a server name the proxy can connect to"
            ))),
        }
    }

    /// The `host:port` of an HTTP CONNECT request (RFC 9110 section 9.3.6),
    /// with IPv6 addresses in brackets.
    fn authority(&self, port: u16) -> String {
        match self {
            Self::Name(name) => format!("{name}:{port}"),
            Self::Address(IpAddr::V4(address)) => format!("{address}:{port}"),
            Self::Address(IpAddr::V6(address)) => format!("[{address}]:{port}"),
        }
    }
}

/// SOCKS version 5 (RFC 1928), with user name and password authentication
/// (RFC 1929). Names go to the proxy unresolved, so DNS lookups happen there
/// as well, the way `socks5h` works in curl.
async fn socks5<S>(
    stream: &mut S,
    server: &ProxyServer,
    target: &Target,
    port: u16,
) -> Result<(), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    const VERSION: u8 = 5;
    const NO_AUTHENTICATION: u8 = 0;
    const USER_NAME_AND_PASSWORD: u8 = 2;
    const NO_ACCEPTABLE_METHOD: u8 = 0xff;

    let io = |error| server.io_error(error);
    let greeting: &[u8] = if server.credentials.is_some() {
        &[VERSION, 2, NO_AUTHENTICATION, USER_NAME_AND_PASSWORD]
    } else {
        &[VERSION, 1, NO_AUTHENTICATION]
    };
    stream.write_all(greeting).await.map_err(io)?;
    let mut choice = [0; 2];
    stream.read_exact(&mut choice).await.map_err(io)?;
    if choice[0] != VERSION {
        return Err(server.protocol("it doesn't speak SOCKS version 5"));
    }
    match (choice[1], &server.credentials) {
        (NO_AUTHENTICATION, _) => {}
        (USER_NAME_AND_PASSWORD, Some(credentials)) => {
            socks5_sign_in(stream, server, credentials).await?;
        }
        (NO_ACCEPTABLE_METHOD, None) => return Err(server.credentials_needed()),
        (NO_ACCEPTABLE_METHOD, Some(_)) => {
            return Err(ProxyError::UnsupportedSignIn {
                proxy: server.label(),
                offered: String::new(),
            });
        }
        (method, _) => {
            return Err(server.protocol(&format!(
                "it chose sign-in method {method}, which wasn't offered"
            )));
        }
    }

    // CONNECT request: VER CMD RSV ATYP DST.ADDR DST.PORT.
    let mut request = vec![VERSION, 1, 0];
    match target {
        Target::Name(name) => {
            let length = u8::try_from(name.len()).map_err(|_| {
                ProxyError::Settings(format!(
                    "the server name {name} is longer than a SOCKS5 proxy accepts"
                ))
            })?;
            request.push(3);
            request.push(length);
            request.extend_from_slice(name.as_bytes());
        }
        Target::Address(IpAddr::V4(address)) => {
            request.push(1);
            request.extend_from_slice(&address.octets());
        }
        Target::Address(IpAddr::V6(address)) => {
            request.push(4);
            request.extend_from_slice(&address.octets());
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await.map_err(io)?;

    // Reply: VER REP RSV ATYP BND.ADDR BND.PORT. Read exactly its length:
    // whatever follows already belongs to the server.
    let mut head = [0; 4];
    stream.read_exact(&mut head).await.map_err(io)?;
    if head[0] != VERSION {
        return Err(server.protocol("its reply isn't SOCKS version 5"));
    }
    if head[1] != 0 {
        return Err(ProxyError::Refused {
            proxy: server.label(),
            target: target.authority(port),
            reason: socks5_reply(head[1]),
        });
    }
    let bound_address = match head[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut length = [0; 1];
            stream.read_exact(&mut length).await.map_err(io)?;
            usize::from(length[0])
        }
        kind => {
            return Err(server.protocol(&format!("its reply names an unknown address type {kind}")));
        }
    };
    let mut bound = vec![0; bound_address + 2];
    stream.read_exact(&mut bound).await.map_err(io)?;
    Ok(())
}

/// RFC 1929: one round trip with the user name and password.
async fn socks5_sign_in<S>(
    stream: &mut S,
    server: &ProxyServer,
    credentials: &Credentials,
) -> Result<(), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let too_long = |what: &str| {
        ProxyError::Settings(format!(
            "the proxy {what} is longer than the 255 bytes SOCKS5 allows"
        ))
    };
    let user = credentials.user.as_bytes();
    let password = credentials.password.as_bytes();
    let user_length = u8::try_from(user.len()).map_err(|_| too_long("user name"))?;
    let password_length = u8::try_from(password.len()).map_err(|_| too_long("password"))?;
    let mut request = Vec::with_capacity(3 + user.len() + password.len());
    request.push(1);
    request.push(user_length);
    request.extend_from_slice(user);
    request.push(password_length);
    request.extend_from_slice(password);
    let written = stream.write_all(&request).await;
    // The request holds the password: don't leave it in freed memory.
    request.fill(0);
    written.map_err(|error| server.io_error(error))?;
    let mut status = [0; 2];
    stream
        .read_exact(&mut status)
        .await
        .map_err(|error| server.io_error(error))?;
    if status[0] != 1 {
        return Err(server.protocol("its sign-in reply has the wrong version"));
    }
    if status[1] != 0 {
        return Err(ProxyError::CredentialsRejected {
            proxy: server.label(),
        });
    }
    Ok(())
}

/// The meaning of a SOCKS5 reply code (RFC 1928 section 6).
fn socks5_reply(code: u8) -> String {
    match code {
        1 => "the proxy failed".into(),
        2 => "the proxy's rules don't allow it".into(),
        3 => "the network can't be reached".into(),
        4 => "the server can't be reached".into(),
        5 => "the server refused the connection".into(),
        6 => "the server took too long to answer".into(),
        7 => "the proxy doesn't support connecting".into(),
        8 => "the proxy doesn't support this kind of address".into(),
        code => format!("the proxy answered with code {code}"),
    }
}

/// SOCKS version 4a: a name goes to the proxy in place of an address, after
/// the invalid address 0.0.0.1. SOCKS4 has no passwords and no IPv6.
async fn socks4a<S>(
    stream: &mut S,
    server: &ProxyServer,
    target: &Target,
    port: u16,
) -> Result<(), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    const GRANTED: u8 = 90;
    // VN CD DSTPORT DSTIP USERID NUL, then the name and a NUL for SOCKS4a.
    let mut request = vec![4, 1];
    request.extend_from_slice(&port.to_be_bytes());
    match target {
        Target::Address(IpAddr::V4(address)) => {
            request.extend_from_slice(&address.octets());
            request.push(0);
        }
        Target::Address(IpAddr::V6(_)) => {
            return Err(ProxyError::Refused {
                proxy: server.label(),
                target: target.authority(port),
                reason: "a SOCKS4 proxy can't reach IPv6 addresses".into(),
            });
        }
        Target::Name(name) => {
            request.extend_from_slice(&[0, 0, 0, 1]);
            request.push(0);
            request.extend_from_slice(name.as_bytes());
            request.push(0);
        }
    }
    stream
        .write_all(&request)
        .await
        .map_err(|error| server.io_error(error))?;
    // Reply: VN CD DSTPORT DSTIP, eight bytes.
    let mut reply = [0; 8];
    stream
        .read_exact(&mut reply)
        .await
        .map_err(|error| server.io_error(error))?;
    if reply[0] != 0 {
        return Err(server.protocol("its reply isn't SOCKS version 4"));
    }
    if reply[1] != GRANTED {
        return Err(ProxyError::Refused {
            proxy: server.label(),
            target: target.authority(port),
            reason: match reply[1] {
                92 | 93 => "the proxy couldn't confirm who is connecting".into(),
                _ => "the proxy turned the connection down".into(),
            },
        });
    }
    Ok(())
}

/// An HTTP CONNECT tunnel (RFC 9110 section 9.3.6), with Basic proxy
/// authentication (RFC 7617).
async fn http_connect<S>(
    stream: &mut S,
    server: &ProxyServer,
    target: &Target,
    port: u16,
) -> Result<(), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let authority = target.authority(port);
    let mut request = format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: {}\r\n",
        super::USER_AGENT
    );
    if let Some(credentials) = &server.credentials {
        let token = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", credentials.user, credentials.password));
        request.push_str("Proxy-Authorization: Basic ");
        request.push_str(&token);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    let mut request = request.into_bytes();
    let written = stream.write_all(&request).await;
    request.fill(0);
    written.map_err(|error| server.io_error(error))?;

    let head = read_reply_head(stream, server).await?;
    let status = parse_status(&head)
        .ok_or_else(|| server.protocol("its reply to CONNECT isn't an HTTP status line"))?;
    if (200..300).contains(&status.code) {
        return Ok(());
    }
    if status.code == 407 {
        let offered = proxy_authenticate_schemes(&head);
        let offers_basic = offered
            .iter()
            .any(|scheme| scheme.eq_ignore_ascii_case("basic"));
        return Err(if !offered.is_empty() && !offers_basic {
            ProxyError::UnsupportedSignIn {
                proxy: server.label(),
                offered: offered.join(", "),
            }
        } else if server.credentials.is_some() {
            ProxyError::CredentialsRejected {
                proxy: server.label(),
            }
        } else {
            server.credentials_needed()
        });
    }
    Err(ProxyError::Refused {
        proxy: server.label(),
        target: authority,
        reason: format!("it answered {}", status.line),
    })
}

/// Read the reply up to and including its blank line, one byte at a time,
/// so nothing after it is taken from the tunnel.
async fn read_reply_head<S>(stream: &mut S, server: &ProxyServer) -> Result<String, ProxyError>
where
    S: AsyncRead + Unpin,
{
    let mut head = Vec::with_capacity(256);
    let mut byte = [0; 1];
    loop {
        match stream.read(&mut byte).await {
            Ok(0) => {
                return Err(server.protocol("it closed the connection before answering"));
            }
            Ok(_) => head.push(byte[0]),
            Err(error) => return Err(server.io_error(error)),
        }
        // RFC 9112 section 2.2 allows a bare LF as a line ending.
        if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
            return Ok(String::from_utf8_lossy(&head).into_owned());
        }
        if head.len() >= MAX_CONNECT_REPLY {
            return Err(server.protocol("its reply to CONNECT is too long"));
        }
    }
}

struct Status<'a> {
    code: u16,
    line: &'a str,
}

/// `HTTP/1.1 200 Connection established` and the like.
fn parse_status(head: &str) -> Option<Status<'_>> {
    let line = head.lines().next()?.trim_end();
    let mut parts = line.splitn(3, ' ');
    let version = parts.next()?;
    if !version.starts_with("HTTP/1.") {
        return None;
    }
    let code = parts.next()?;
    if code.len() != 3 {
        return None;
    }
    Some(Status {
        code: code.parse().ok()?,
        line,
    })
}

/// The authentication schemes a 407 reply offers in its
/// `Proxy-Authenticate` fields (RFC 9110 section 11.7.1).
fn proxy_authenticate_schemes(head: &str) -> Vec<String> {
    let mut schemes: Vec<String> = Vec::new();
    for line in head.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("proxy-authenticate") {
            continue;
        }
        // A field lists challenges and their parameters, separated by commas
        // outside quoted strings. A challenge starts with its scheme, a token
        // without `=`, where a parameter is `name=value`.
        for element in split_outside_quotes(value) {
            let token = element.split_whitespace().next().unwrap_or_default();
            if !token.is_empty()
                && !token.contains('=')
                && !schemes
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(token))
            {
                schemes.push(token.to_owned());
            }
        }
    }
    schemes
}

/// Split a field value at the commas that aren't inside a quoted string.
fn split_outside_quotes(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        match character {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => {
                parts.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

#[cfg(test)]
mod tests;
