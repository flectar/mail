//! What the bridge asks the running app, and what the app answers: one line
//! of JSON each way on a fresh connection.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::io;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Raised whenever a request or response changes incompatibly.
pub(crate) const PROTOCOL: u32 = 1;
/// Larger than any request the tools take.
pub(crate) const MAX_REQUEST: u64 = 64 * 1024;
/// Larger than any answer the tools give.
pub(crate) const MAX_RESPONSE: u64 = 4 * 1024 * 1024;

/// One tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Request {
    pub protocol: u32,
    /// The bridge's version. A running app can be older than the copy on disk.
    pub version: String,
    /// The name the agent's app gave itself.
    pub agent: String,
    pub tool: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

/// A tool's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Response {
    pub protocol: u32,
    pub version: String,
    /// What the agent reads.
    pub text: String,
    /// The same as data, for agents that read structured results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    #[serde(default)]
    pub is_error: bool,
}

impl Response {
    pub(crate) fn ok(text: String, structured: Option<Value>) -> Self {
        Self {
            protocol: PROTOCOL,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            text,
            structured,
            is_error: false,
        }
    }

    pub(crate) fn error(text: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::ok(text.into(), None)
        }
    }
}

/// Reads one line of JSON of at most `limit` bytes.
pub(crate) async fn read_line<T: DeserializeOwned>(
    stream: impl AsyncRead + Unpin,
    limit: u64,
) -> io::Result<T> {
    let mut line = Vec::new();
    BufReader::new(tokio::io::AsyncReadExt::take(stream, limit))
        .read_until(b'\n', &mut line)
        .await?;
    if line.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the message ended early or was too long",
        ));
    }
    serde_json::from_slice(&line).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Writes `value` as one line of JSON.
pub(crate) async fn write_line(
    mut stream: impl AsyncWrite + Unpin,
    value: &impl Serialize,
) -> io::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    stream.write_all(&line).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn requests_and_answers_are_single_lines_of_json() {
        let request = Request {
            protocol: PROTOCOL,
            version: "1.2.3".into(),
            agent: "Test agent".into(),
            tool: "read_message".into(),
            arguments: serde_json::json!({ "message_id": 7, "text": "two\nlines" })
                .as_object()
                .unwrap()
                .clone(),
        };
        let mut wire = Vec::new();
        write_line(&mut wire, &request).await.unwrap();
        assert_eq!(wire.iter().filter(|byte| **byte == b'\n').count(), 1);
        let read: Request = read_line(wire.as_slice(), MAX_REQUEST).await.unwrap();
        assert_eq!(read, request);

        // Without its newline, or past the limit, a message is refused.
        let read = read_line::<Request>(&wire[..wire.len() - 1], MAX_REQUEST).await;
        assert!(read.is_err());
        let read = read_line::<Request>(wire.as_slice(), 10).await;
        assert!(read.is_err());

        let answer = Response::error("Agent access is off.");
        let mut wire = Vec::new();
        write_line(&mut wire, &answer).await.unwrap();
        let read: Response = read_line(wire.as_slice(), MAX_RESPONSE).await.unwrap();
        assert!(read.is_error);
        assert_eq!(read.text, "Agent access is off.");
    }
}
