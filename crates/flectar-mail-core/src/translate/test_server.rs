//! A stand-in translation service for tests.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// A request the stand-in server answered: method, target, headers (names
/// in lowercase) and body.
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) target: String,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) body: String,
}

impl Request {
    pub(crate) fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

pub(crate) type Answer = (u16, Vec<(&'static str, String)>, String);

/// A stand-in service that answers the `n`th request with `answer(n,
/// request)` and keeps the requests.
pub(crate) async fn server(
    answer: impl Fn(usize, &Request) -> Answer + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<Request>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&requests);
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (requests, answer) = (Arc::clone(&kept), Arc::clone(&answer));
            tokio::spawn(async move {
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_owned();
                let target = parts.next().unwrap_or_default().to_owned();
                let mut headers = HashMap::new();
                loop {
                    let mut header = String::new();
                    stream.read_line(&mut header).await.unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
                    }
                }
                let length = headers
                    .get("content-length")
                    .map_or(0, |length| length.parse().unwrap());
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.unwrap();
                let request = Request {
                    method,
                    target,
                    headers,
                    body: String::from_utf8(body).unwrap(),
                };
                let n = {
                    let mut requests = requests.lock().unwrap();
                    requests.push(request.clone());
                    requests.len() - 1
                };
                let (status, extra, body) = answer(n, &request);
                let mut response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (name, value) in extra {
                    response.push_str(&format!("{name}: {value}\r\n"));
                }
                response.push_str("\r\n");
                response.push_str(&body);
                stream
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            });
        }
    });
    (origin, requests)
}

/// An answer with status 200 and this JSON.
pub(crate) fn ok(body: Value) -> Answer {
    (200, Vec::new(), body.to_string())
}
