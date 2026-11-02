//! Bounded HTTP response-body readers.
//!
//! `reqwest::Response::text` and `bytes` buffer until EOF. Provider endpoints
//! are remote trust boundaries, so every caller supplies a protocol-appropriate
//! ceiling and chunked responses are rejected before they can grow past it.

use crate::error::{CoreError, Result};

pub(crate) async fn text(
    response: reqwest::Response,
    max_bytes: usize,
    context: &'static str,
) -> Result<String> {
    let bytes = bytes(response, max_bytes, context).await?;
    String::from_utf8(bytes).map_err(|error| {
        CoreError::Network(format!(
            "{context} was not valid UTF-8: {}",
            error.utf8_error()
        ))
    })
}

pub(crate) async fn bytes(
    mut response: reqwest::Response,
    max_bytes: usize,
    context: &'static str,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(too_large(context, max_bytes));
    }

    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(8 * 1024)
        .min(max_bytes)
        .min(64 * 1024);
    let mut body = Vec::with_capacity(capacity);
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() || error.is_connect() {
            CoreError::Offline
        } else {
            CoreError::Network(format!("{context} read failed: {error}"))
        }
    })? {
        append_bounded(&mut body, &chunk, max_bytes, context)?;
    }
    Ok(body)
}

fn append_bounded(
    body: &mut Vec<u8>,
    chunk: &[u8],
    max_bytes: usize,
    context: &'static str,
) -> Result<()> {
    if chunk.len() > max_bytes.saturating_sub(body.len()) {
        return Err(too_large(context, max_bytes));
    }
    let required = body.len() + chunk.len();
    if required > body.capacity() {
        // Vec's implicit doubling can exceed the caller's response ceiling.
        let target = required
            .max(body.capacity().saturating_mul(2))
            .min(max_bytes);
        body.reserve_exact(target - body.len());
    }
    body.extend_from_slice(chunk);
    Ok(())
}

fn too_large(context: &'static str, max_bytes: usize) -> CoreError {
    CoreError::ResponseTooLarge { context, max_bytes }
}

/// Collapse Unicode whitespace while retaining at most `max_chars` visible
/// characters. Error excerpts use this instead of building a full vector of
/// every word in a potentially multi-megabyte provider response.
pub(crate) fn single_line_excerpt(value: &str, max_chars: usize) -> String {
    let mut output = String::with_capacity(max_chars.min(value.len()));
    let mut written = 0_usize;
    let mut truncated = false;

    'words: for word in value.split_whitespace() {
        if !output.is_empty() {
            if written == max_chars {
                truncated = true;
                break;
            }
            output.push(' ');
            written += 1;
        }
        for character in word.chars() {
            if written == max_chars {
                truncated = true;
                break 'words;
            }
            output.push(character);
            written += 1;
        }
    }
    if truncated {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_body_never_grows_past_its_limit() {
        let mut body = vec![1, 2, 3];
        append_bounded(&mut body, &[4, 5], 5, "test body").unwrap();
        assert_eq!(body, [1, 2, 3, 4, 5]);

        let error = append_bounded(&mut body, &[6], 5, "test body").unwrap_err();
        assert!(error.to_string().contains("5-byte response limit"));
        assert_eq!(body, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn excerpts_are_single_line_and_stop_early() {
        assert_eq!(single_line_excerpt("  one\n two  ", 20), "one two");
        assert_eq!(single_line_excerpt("one two three", 7), "one two…");
        assert_eq!(single_line_excerpt("abcdefgh", 4), "abcd…");
    }
}
