//! ManageSieve servers for tests, over TLS with the test certificate: the
//! pieces the protocol tests script by hand, and a server that keeps
//! scripts the way a real one does.

use super::managesieve::Server;
use crate::models::ConnectionSecurity;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

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
        owner: crate::net::proxy::Owner::App,
        host: "127.0.0.1",
        port,
        security: ConnectionSecurity::Starttls,
        trusted_certificate_pem: CERT,
        timeout: crate::net::timeouts::STANDARD,
    }
}

/// What a [`Keeper`] server holds.
#[derive(Debug, Default)]
pub(crate) struct Kept {
    pub(crate) scripts: BTreeMap<String, String>,
    pub(crate) active: Option<String>,
    /// The commands it was sent over TLS.
    pub(crate) commands: Vec<String>,
}

/// A server that keeps scripts as a real one does, offering `extensions`.
pub(crate) struct Keeper {
    pub(crate) port: u16,
    pub(crate) kept: Arc<Mutex<Kept>>,
}

impl Keeper {
    pub(crate) async fn start(extensions: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let kept = Arc::new(Mutex::new(Kept::default()));
        let served = Arc::clone(&kept);
        let extensions = extensions.to_owned();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                tokio::spawn(serve(tcp, Arc::clone(&served), extensions.clone()));
            }
        });
        Self { port, kept }
    }

    /// Stores `text` as `name`, made active when `active`.
    pub(crate) fn put(&self, name: &str, text: &str, active: bool) {
        let mut kept = self.kept.lock().unwrap();
        kept.scripts.insert(name.into(), text.into());
        if active {
            kept.active = Some(name.into());
        }
    }

    pub(crate) fn script(&self, name: &str) -> Option<String> {
        self.kept.lock().unwrap().scripts.get(name).cloned()
    }

    pub(crate) fn active(&self) -> Option<String> {
        self.kept.lock().unwrap().active.clone()
    }

    /// The commands that changed something, such as `PUTSCRIPT "name"`.
    pub(crate) fn changes(&self) -> Vec<String> {
        self.kept
            .lock()
            .unwrap()
            .commands
            .iter()
            .filter(|command| command.starts_with("PUTSCRIPT") || command.starts_with("SETACTIVE"))
            .map(|command| command.lines().next().unwrap_or_default().to_owned())
            .map(|command| command.split(" {").next().unwrap_or_default().to_owned())
            .collect()
    }
}

async fn serve(tcp: tokio::net::TcpStream, kept: Arc<Mutex<Kept>>, extensions: String) {
    let ignored = Mutex::new(Vec::new());
    let mut socket: BufReader<Box<dyn Socket>> = BufReader::new(Box::new(tcp));
    let greeting = format!(
        "\"IMPLEMENTATION\" \"Keeper\"\r\n\"SIEVE\" \"{extensions}\"\r\n\"STARTTLS\"\r\nOK\r\n"
    );
    socket
        .get_mut()
        .write_all(greeting.as_bytes())
        .await
        .unwrap();
    if read_command(&mut socket, &ignored).await.as_deref() != Some("STARTTLS") {
        return;
    }
    socket.get_mut().write_all(b"OK\r\n").await.unwrap();
    let Ok(tls) = acceptor().accept(socket.into_inner()).await else {
        return;
    };
    let mut socket: BufReader<Box<dyn Socket>> = BufReader::new(Box::new(tls));
    let greeting = format!(
        "\"IMPLEMENTATION\" \"Keeper\"\r\n\"SIEVE\" \"{extensions}\"\r\n\"SASL\" \"PLAIN\"\r\nOK\r\n"
    );
    socket
        .get_mut()
        .write_all(greeting.as_bytes())
        .await
        .unwrap();
    while let Some(command) = read_command(&mut socket, &ignored).await {
        let reply = answer(&kept, &command);
        socket.get_mut().write_all(reply.as_bytes()).await.unwrap();
        if command == "LOGOUT" {
            return;
        }
    }
}

fn answer(kept: &Mutex<Kept>, command: &str) -> String {
    let mut kept = kept.lock().unwrap();
    kept.commands.push(command.to_owned());
    let (verb, rest) = command.split_once(' ').unwrap_or((command, ""));
    let name = unquote(rest);
    match verb {
        "AUTHENTICATE" | "LOGOUT" => "OK\r\n".into(),
        "LISTSCRIPTS" => {
            let mut lines = String::new();
            for name in kept.scripts.keys() {
                let active = kept.active.as_deref() == Some(name.as_str());
                lines.push_str(&format!(
                    "\"{name}\"{}\r\n",
                    if active { " ACTIVE" } else { "" }
                ));
            }
            lines + "OK\r\n"
        }
        "GETSCRIPT" => match kept.scripts.get(&name) {
            Some(text) => format!("{{{}}}\r\n{text}\r\nOK\r\n", text.len()),
            None => "NO (NONEXISTENT) \"There is no such script\"\r\n".into(),
        },
        "PUTSCRIPT" => {
            let text = rest.split_once('\n').map_or("", |(_, text)| text);
            kept.scripts.insert(name, text.into());
            "OK\r\n".into()
        }
        "SETACTIVE" if name.is_empty() => {
            kept.active = None;
            "OK\r\n".into()
        }
        "SETACTIVE" if kept.scripts.contains_key(&name) => {
            kept.active = Some(name);
            "OK\r\n".into()
        }
        _ => "NO \"Not here\"\r\n".into(),
    }
}

/// The quoted string `text` starts with.
fn unquote(text: &str) -> String {
    let mut name = String::new();
    let mut chars = text.strip_prefix('"').unwrap_or_default().chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => name.extend(chars.next()),
            c => name.push(c),
        }
    }
    name
}
