//! ManageSieve servers for tests, over TLS with the test certificate: the
//! pieces the protocol tests script by hand.

use super::managesieve::Server;
use crate::models::ConnectionSecurity;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

pub(crate) const CERT: &str = include_str!("../../tests/fixtures/tls/server.pem");
const KEY: &str = include_str!("../../tests/fixtures/tls/server-key.pem");

pub(crate) fn acceptor() -> tokio_rustls::TlsAcceptor {
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from_pem_slice(CERT.as_bytes()).unwrap()],
        PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap(),
    )
    .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
}

pub(crate) trait Socket:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send
{
}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Socket for T {}

/// The next command, with a literal it ends with after a line break, and
/// recorded in `log`; `None` once the client hung up.
pub(crate) async fn read_command(
    socket: &mut BufReader<Box<dyn Socket>>,
    log: &Mutex<Vec<String>>,
) -> Option<String> {
    let mut line = String::new();
    if socket.read_line(&mut line).await.unwrap_or(0) == 0 {
        return None;
    }
    let mut line = line.trim_end_matches(['\r', '\n']).to_owned();
    if let Some(length) = line
        .strip_suffix("+}")
        .and_then(|start| start.rsplit_once('{'))
        .and_then(|(_, length)| length.parse::<usize>().ok())
    {
        let mut literal = vec![0; length + 2];
        socket.read_exact(&mut literal).await.unwrap();
        line.push('\n');
        line.push_str(&String::from_utf8(literal).unwrap()[..length]);
    }
    log.lock().unwrap().push(line.clone());
    Some(line)
}

pub(crate) fn server_at(port: u16) -> Server<'static> {
    Server {
        host: "127.0.0.1",
        port,
        security: ConnectionSecurity::Starttls,
        trusted_certificate_pem: CERT,
    }
}
