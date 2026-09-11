//! Minimal localhost loopback server for the OAuth authorization-code
//! redirect. Binds an ephemeral port on both IPv4 (127.0.0.1) and IPv6 (::1)
//! loopback so it is reachable however the browser resolves `localhost`, waits
//! for exactly one GET /?code=...&state=..., and replies with a close-this-tab
//! page.

use crate::error::{CoreError, Result};
use std::net::{Ipv4Addr, Ipv6Addr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub struct LoopbackServer {
    v4: TcpListener,
    v6: Option<TcpListener>,
    pub port: u16,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AuthCode {
    pub code: String,
    pub state: Option<String>,
}

impl std::fmt::Debug for AuthCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthCode")
            .field("code", &"[REDACTED]")
            .field("state", &self.state.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl LoopbackServer {
    pub async fn bind() -> Result<Self> {
        // Bind IPv4 loopback on an ephemeral port first, then try to grab the
        // same port on IPv6 loopback. `localhost` may resolve to either 127.0.0.1
        // or ::1 depending on the OS/browser, so we must listen on both to
        // reliably catch the redirect. IPv6 is best-effort: if it (or the port)
        // is unavailable we fall back to IPv4-only.
        let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = v4.local_addr()?.port();
        let v6 = TcpListener::bind((Ipv6Addr::LOCALHOST, port)).await.ok();
        tracing::debug!(
            port,
            ipv6 = v6.is_some(),
            "oauth: loopback redirect server listening"
        );
        Ok(LoopbackServer { v4, v6, port })
    }

    pub fn localhost_redirect_uri(&self) -> String {
        // `localhost` (not the 127.0.0.1 literal) matches the redirect URI
        // registered for desktop OAuth clients and is what Microsoft requires
        // to ignore the ephemeral port. We listen on both loopback stacks so
        // whichever address `localhost` resolves to reaches us.
        format!("http://localhost:{}/", self.port)
    }

    /// Google desktop registrations use the loopback-IP redirect described by
    /// the installed-app OAuth flow. Keep this separate from Microsoft's
    /// `localhost` redirect, whose dynamic port matching has different rules.
    pub fn ipv4_redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    /// Wait (with timeout) for the browser redirect carrying ?code=.
    pub async fn wait_for_code(&self, timeout: std::time::Duration) -> Result<AuthCode> {
        let fut = async {
            loop {
                let (mut stream, _) = match &self.v6 {
                    Some(v6) => tokio::select! {
                        r = self.v4.accept() => r?,
                        r = v6.accept() => r?,
                    },
                    None => self.v4.accept().await?,
                };
                if let Some(result) = handle_connection(&mut stream).await? {
                    return Ok(result);
                }
                // Ignore stray/speculative requests (favicon etc.) and keep
                // listening.
            }
        };
        tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| CoreError::Auth("sign-in timed out".into()))?
    }
}

/// Parse one loopback request. Returns `Ok(Some(code))` on a callback carrying
/// a code, `Err` if the provider redirected back with an error, and `Ok(None)`
/// for connections we should ignore (dropped/speculative/stray requests).
async fn handle_connection(stream: &mut TcpStream) -> Result<Option<AuthCode>> {
    let mut buf = vec![0u8; 8192];
    // Browsers open speculative connections to localhost and drop them without
    // sending anything; a reset or empty read must not abort the whole sign-in
    // (it used to surface as a bogus "File system error" toast).
    let n = match tokio::time::timeout(std::time::Duration::from_secs(3), stream.read(&mut buf))
        .await
    {
        Err(_) => {
            tracing::debug!("oauth: ignoring silent loopback connection");
            return Ok(None);
        }
        Ok(Ok(0)) => return Ok(None),
        Ok(Ok(n)) => n,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "oauth: ignoring dropped loopback connection");
            return Ok(None);
        }
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    let first_line = req.lines().next().unwrap_or("");
    // GET /?code=...&state=... HTTP/1.1
    let path = first_line.split_whitespace().nth(1).unwrap_or("");
    let query = path.split_once('?').map(|x| x.1).unwrap_or("");
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        let v = urldecode(v);
        match k {
            "code" => code = Some(v),
            "state" => state = Some(v),
            "error" => error = Some(v),
            _ => {}
        }
    }
    let body = if code.is_some() {
        "<html><body style=\"font-family:sans-serif;padding:4rem;text-align:center\">\
         <h2>Flectar Mail is connected.</h2><p>You can close this tab.</p></body></html>"
    } else {
        "<html><body style=\"font-family:sans-serif;padding:4rem;text-align:center\">\
         <h2>Sign-in failed.</h2><p>Return to Flectar Mail and try again.</p></body></html>"
    };
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;

    if let Some(e) = error {
        tracing::warn!(error = %e, "oauth: provider redirected back with an error");
        return Err(CoreError::Auth(format!("oauth error: {e}")));
    }
    if let Some(code) = code {
        return Ok(Some(AuthCode { code, state }));
    }
    Ok(None)
}

fn urldecode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provider_loopback_uris_use_the_expected_host_and_dynamic_port() {
        let server = LoopbackServer::bind().await.unwrap();
        assert_eq!(
            server.localhost_redirect_uri(),
            format!("http://localhost:{}/", server.port)
        );
        assert_eq!(
            server.ipv4_redirect_uri(),
            format!("http://127.0.0.1:{}/", server.port)
        );
    }
}
