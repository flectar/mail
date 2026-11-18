//! A SOCKS5 or HTTP proxy on this computer, for tests. Whatever server it's
//! asked for, it connects to one port here, so tests can name servers the way
//! real ones are named and keep clear of the loopback bypass.

use base64::Engine;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Socks5,
    Http,
}

pub struct TestProxy {
    pub port: u16,
    /// What each connection asked for, in order: `host:port` for a tunnel,
    /// the request line for a request an HTTP proxy forwards.
    pub asked: Arc<Mutex<Vec<String>>>,
    /// How many connections reached the proxy.
    pub connections: Arc<Mutex<usize>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TestProxy {
    /// A proxy whose tunnels lead to `target` on this computer, and that
    /// wants `credentials` when given.
    pub async fn start(kind: Kind, target: u16, credentials: Option<(&str, &str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(Mutex::new(0));
        let credentials =
            credentials.map(|(user, password)| (user.to_owned(), password.to_owned()));
        let task = {
            let asked = Arc::clone(&asked);
            let connections = Arc::clone(&connections);
            tokio::spawn(async move {
                loop {
                    let Ok((client, _)) = listener.accept().await else {
                        return;
                    };
                    *connections.lock().unwrap() += 1;
                    let asked = Arc::clone(&asked);
                    let credentials = credentials.clone();
                    tokio::spawn(async move {
                        let _ = match kind {
                            Kind::Socks5 => socks5(client, target, credentials, asked).await,
                            Kind::Http => http(client, target, credentials, asked).await,
                        };
                    });
                }
            })
        };
        Self {
            port,
            asked,
            connections,
            task,
        }
    }

    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    pub fn connections(&self) -> usize {
        *self.connections.lock().unwrap()
    }
}

async fn socks5(
    mut client: TcpStream,
    target: u16,
    credentials: Option<(String, String)>,
    asked: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let mut head = [0; 2];
    client.read_exact(&mut head).await?;
    let mut methods = vec![0; usize::from(head[1])];
    client.read_exact(&mut methods).await?;
    match &credentials {
        Some((user, password)) if methods.contains(&2) => {
            client.write_all(&[5, 2]).await?;
            let mut version = [0; 2];
            client.read_exact(&mut version).await?;
            let mut name = vec![0; usize::from(version[1])];
            client.read_exact(&mut name).await?;
            let mut length = [0; 1];
            client.read_exact(&mut length).await?;
            let mut secret = vec![0; usize::from(length[0])];
            client.read_exact(&mut secret).await?;
            let accepted = name == user.as_bytes() && secret == password.as_bytes();
            client.write_all(&[1, u8::from(!accepted)]).await?;
            if !accepted {
                return Ok(());
            }
        }
        Some(_) => return client.write_all(&[5, 0xff]).await,
        None => client.write_all(&[5, 0]).await?,
    }
    let mut request = [0; 4];
    client.read_exact(&mut request).await?;
    let host = match request[3] {
        3 => {
            let mut length = [0; 1];
            client.read_exact(&mut length).await?;
            let mut name = vec![0; usize::from(length[0])];
            client.read_exact(&mut name).await?;
            String::from_utf8(name).unwrap()
        }
        1 => {
            let mut address = [0; 4];
            client.read_exact(&mut address).await?;
            std::net::Ipv4Addr::from(address).to_string()
        }
        _ => {
            let mut address = [0; 16];
            client.read_exact(&mut address).await?;
            format!("[{}]", std::net::Ipv6Addr::from(address))
        }
    };
    let mut port = [0; 2];
    client.read_exact(&mut port).await?;
    asked
        .lock()
        .unwrap()
        .push(format!("{host}:{}", u16::from_be_bytes(port)));
    let mut server = TcpStream::connect(("127.0.0.1", target)).await?;
    client.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await?;
    tokio::io::copy_bidirectional(&mut client, &mut server).await?;
    Ok(())
}

async fn http(
    client: TcpStream,
    target: u16,
    credentials: Option<(String, String)>,
    asked: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let mut client = BufReader::new(client);
    let mut head = Vec::new();
    loop {
        let mut line = String::new();
        if client.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let blank = line == "\r\n";
        head.push(line);
        if blank {
            break;
        }
    }
    let request_line = head[0].trim_end().to_owned();
    let authorized = credentials.as_ref().is_none_or(|(user, password)| {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        head.iter().any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("proxy-authorization")
                    && value.trim() == format!("Basic {token}")
            })
        })
    });
    if !authorized {
        return client
            .get_mut()
            .write_all(
                b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                  Proxy-Authenticate: Basic realm=\"test\"\r\nContent-Length: 0\r\n\r\n",
            )
            .await;
    }
    let mut server = TcpStream::connect(("127.0.0.1", target)).await?;
    if let Some(authority) = request_line
        .strip_prefix("CONNECT ")
        .and_then(|rest| rest.split(' ').next())
    {
        asked.lock().unwrap().push(authority.to_owned());
        client
            .get_mut()
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await?;
    } else {
        // A request forwarded in absolute form goes to the server as it is.
        asked.lock().unwrap().push(request_line);
        server.write_all(head.concat().as_bytes()).await?;
    }
    let buffered = client.buffer().to_vec();
    server.write_all(&buffered).await?;
    tokio::io::copy_bidirectional(client.get_mut(), &mut server).await?;
    Ok(())
}
