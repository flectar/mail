//! Native Gmail REST provider.
//!
//! Gmail accounts never open IMAP or SMTP connections. Metadata backfill uses
//! messages.list plus 50-call multipart batches, live reconciliation uses the
//! History API, reads hydrate complete Gmail threads, mutations use native
//! labels, and drafts/sending use the Gmail API. The existing SQLite cache and
//! pending-action queue remain provider-neutral and fully offline-first.

use super::engine::{
    PriorityFetchCmd, SyncCmd, SyncCtx, configured_sync_interval, emit_sync_status, set_state,
    set_state_error,
};
use crate::db::repo::{self, messages::NewMessage};
use crate::error::{CoreError, Result};
use crate::events::{CoreEvent, SyncProgress};
use crate::http_body;
use crate::models::{
    AccountConfig, AccountSettings, Address, AuthKind, MailHistory, Provider, now_ms, roles,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::{StreamExt, stream};
use reqwest::{Client, StatusCode, header::CONTENT_TYPE};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

const GMAIL_API: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const GMAIL_BATCH_API: &str = "https://gmail.googleapis.com/batch/gmail/v1";
const SYNC_PAGE_SIZE: usize = 100;
// Gmail expands every multipart batch into concurrent inner requests. Keep
// this comfortably below the per-user concurrency ceiling, leaving room for
// one interactive reader request and other Gmail clients using the account.
const BATCH_SIZE: usize = 10;
const HISTORY_FETCH_CONCURRENCY: usize = 2;
const HISTORY_PAGE_SIZE: usize = 500;
// A full metadata page costs 5 units for messages.list plus 100 * 20 for
// messages.get. At Google's 6,000 units/user/minute, 22 seconds keeps a large
// backfill under quota with headroom for labels, actions, and interactive body
// reads. Batching reduces HTTP overhead; it does not reduce quota-unit cost.
// https://developers.google.com/workspace/gmail/api/reference/quota
const BACKFILL_INTERVAL: Duration = Duration::from_secs(22);
const HTTP_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_GMAIL_JSON_BODY_BYTES: usize = 64 * 1024 * 1024;
const MAX_GMAIL_BATCH_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_GMAIL_PAGE_TOKEN_BYTES: usize = 16 * 1024;
const MAX_GMAIL_LABELS: usize = 10_000;
const MAX_GMAIL_HEADERS_BYTES: usize = 256 * 1024;
const MAX_GMAIL_MIME_PARTS: usize = 2_000;
const MAX_HISTORY_THREADS_PER_PAGE: usize = 10_000;
const HISTORY_THREAD_BATCH: usize = 4;
const MAX_HISTORY_MESSAGES_PER_BATCH: usize = 10_000;
const MAX_MESSAGES_PER_THREAD: usize = 10_000;
const MAX_DRAFT_PAGES: usize = 100;
const MAX_DRAFTS: usize = 50_000;
const MAX_GMAIL_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
const METADATA_HEADERS: &[&str] = &[
    "Authentication-Results",
    "Bcc",
    "Cc",
    "Date",
    "From",
    "In-Reply-To",
    "List-Id",
    "List-Unsubscribe",
    "List-Unsubscribe-Post",
    "Message-ID",
    "Precedence",
    "References",
    "Reply-To",
    "Return-Path",
    "Sender",
    "Subject",
    "To",
];

type RemoteDraftAttachment = (Option<String>, Option<String>, String, Option<String>);

#[derive(Clone)]
struct GmailApi {
    http: Client,
    ctx: SyncCtx,
    account: AccountConfig,
}

#[derive(Debug, Clone)]
struct GmailLabel {
    id: String,
    name: String,
    kind: String,
    background_color: Option<String>,
    text_color: Option<String>,
}

#[derive(Debug)]
struct HistoryPage {
    affected_threads: BTreeSet<String>,
    next_page_token: Option<String>,
    completed_history_id: Option<String>,
}

impl GmailApi {
    fn new(ctx: SyncCtx, account: AccountConfig) -> Result<Self> {
        let http = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("Flectar-Mail/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| CoreError::Network(error.to_string()))?;
        Ok(Self { http, ctx, account })
    }

    async fn token(&self) -> Result<String> {
        self.ctx
            .tokens
            .access_token(self.account.id, Provider::Gmail)
            .await
    }

    async fn get_json(&self, url: &str) -> Result<Value> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let response = self
                .http
                .get(url)
                .bearer_auth(token)
                .send()
                .await
                .map_err(network_error)?;
            let result = google_json(response).await;
            if attempt == 0 && matches!(&result, Err(CoreError::NeedsReauth)) {
                self.ctx.tokens.invalidate(self.account.id).await;
                continue;
            }
            return result;
        }
        Err(CoreError::NeedsReauth)
    }

    async fn post_json(&self, url: &str, body: &Value) -> Result<Value> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let response = self
                .http
                .post(url)
                .bearer_auth(token)
                .json(body)
                .send()
                .await
                .map_err(network_error)?;
            let result = google_json(response).await;
            if attempt == 0 && matches!(&result, Err(CoreError::NeedsReauth)) {
                self.ctx.tokens.invalidate(self.account.id).await;
                continue;
            }
            return result;
        }
        Err(CoreError::NeedsReauth)
    }

    async fn put_json(&self, url: &str, body: &Value) -> Result<Value> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let response = self
                .http
                .put(url)
                .bearer_auth(token)
                .json(body)
                .send()
                .await
                .map_err(network_error)?;
            let result = google_json(response).await;
            if attempt == 0 && matches!(&result, Err(CoreError::NeedsReauth)) {
                self.ctx.tokens.invalidate(self.account.id).await;
                continue;
            }
            return result;
        }
        Err(CoreError::NeedsReauth)
    }

    async fn delete_json(&self, url: &str) -> Result<Value> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let response = self
                .http
                .delete(url)
                .bearer_auth(token)
                .send()
                .await
                .map_err(network_error)?;
            let result = google_json(response).await;
            if attempt == 0 && matches!(&result, Err(CoreError::NeedsReauth)) {
                self.ctx.tokens.invalidate(self.account.id).await;
                continue;
            }
            return result;
        }
        Err(CoreError::NeedsReauth)
    }

    async fn profile_history_id(&self) -> Result<String> {
        let history_id = self
            .get_json(&format!("{GMAIL_API}/profile"))
            .await?
            .get("historyId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| CoreError::Other("Gmail profile omitted historyId".into()))?;
        if history_id.len() > MAX_GMAIL_PAGE_TOKEN_BYTES {
            return Err(CoreError::Network(
                "Gmail returned an oversized history cursor".into(),
            ));
        }
        Ok(history_id)
    }

    async fn labels(&self) -> Result<Vec<GmailLabel>> {
        let value = self.get_json(&format!("{GMAIL_API}/labels")).await?;
        let labels = value
            .get("labels")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if labels.len() > MAX_GMAIL_LABELS {
            return Err(CoreError::Network(format!(
                "Gmail returned more than {MAX_GMAIL_LABELS} labels"
            )));
        }
        Ok(labels
            .iter()
            .filter_map(|label| {
                Some(GmailLabel {
                    id: label.get("id")?.as_str()?.to_owned(),
                    name: label.get("name")?.as_str()?.to_owned(),
                    kind: label
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("system")
                        .to_ascii_lowercase(),
                    background_color: label
                        .pointer("/color/backgroundColor")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    text_color: label
                        .pointer("/color/textColor")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect())
    }

    async fn list_messages_page(
        &self,
        page_token: Option<&str>,
        cutoff: Option<chrono::NaiveDate>,
    ) -> Result<(Vec<Value>, Option<String>)> {
        let url = messages_list_url(page_token, cutoff);
        let response = self.get_json(&url).await?;
        let remote_messages = response
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if remote_messages.len() > SYNC_PAGE_SIZE {
            return Err(CoreError::Network(format!(
                "Gmail returned more than {SYNC_PAGE_SIZE} messages in one sync page"
            )));
        }
        let ids = remote_messages
            .iter()
            .filter_map(|message| message.get("id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut messages = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(BATCH_SIZE) {
            messages.extend(self.batch_metadata(chunk).await?);
        }
        let next = gmail_page_token(&response)?;
        Ok((messages, next))
    }

    /// Find a previously accepted send by its stable RFC 5322 Message-ID. This
    /// closes the ambiguous timeout window around drafts.send: on retry we can
    /// prove Gmail already placed the message in Sent instead of sending a
    /// duplicate.
    async fn find_sent_by_message_id(&self, message_id: &str) -> Result<Option<Value>> {
        let message_id = message_id.trim().trim_matches(['<', '>']);
        if message_id.is_empty() {
            return Ok(None);
        }
        let query = format!("rfc822msgid:<{message_id}>");
        let value = self
            .get_json(&format!(
                "{GMAIL_API}/messages?maxResults=10&includeSpamTrash=true&q={}",
                urlencode(&query)
            ))
            .await?;
        for id in value
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|message| message.get("id").and_then(Value::as_str))
        {
            let resource = self
                .get_json(&format!(
                    "{GMAIL_API}/messages/{}?format=metadata",
                    urlencode(id)
                ))
                .await?;
            let sent = resource
                .get("labelIds")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|label| label.as_str() == Some("SENT"));
            if sent {
                return Ok(Some(resource));
            }
        }
        Ok(None)
    }

    async fn batch_metadata(&self, ids: &[String]) -> Result<Vec<Value>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let boundary = format!("flectar_{:016x}_{}", rand::random::<u64>(), now_ms());
        let body = build_batch_body(ids, &boundary);
        for attempt in 0..2 {
            let token = self.token().await?;
            let response = self
                .http
                .post(GMAIL_BATCH_API)
                .bearer_auth(token)
                .header(
                    CONTENT_TYPE,
                    format!("multipart/mixed; boundary={boundary}"),
                )
                .body(body.clone())
                .send()
                .await
                .map_err(network_error)?;
            let result = if !response.status().is_success() {
                google_json(response).await.map(|_| Vec::new())
            } else {
                let response_boundary = response
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .and_then(multipart_boundary)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        CoreError::Network("Gmail batch response omitted boundary".into())
                    })?;
                let text =
                    http_body::text(response, MAX_GMAIL_BATCH_BODY_BYTES, "Gmail batch response")
                        .await?;
                parse_batch_response(&text, &response_boundary, ids.len())
            };
            if attempt == 0 && matches!(&result, Err(CoreError::NeedsReauth)) {
                self.ctx.tokens.invalidate(self.account.id).await;
                continue;
            }
            return result;
        }
        Err(CoreError::NeedsReauth)
    }

    async fn thread(&self, thread_id: &str, full: bool) -> Result<Value> {
        let mut url = format!(
            "{GMAIL_API}/threads/{}?format={}",
            urlencode(thread_id),
            if full { "full" } else { "metadata" }
        );
        if !full {
            for header in METADATA_HEADERS {
                url.push_str("&metadataHeaders=");
                url.push_str(&urlencode(header));
            }
        }
        self.get_json(&url).await
    }

    async fn history(&self, history_id: &str, page_token: Option<&str>) -> Result<HistoryPage> {
        let mut url = format!(
            "{GMAIL_API}/history?startHistoryId={}&maxResults={HISTORY_PAGE_SIZE}",
            urlencode(history_id)
        );
        if let Some(token) = page_token {
            url.push_str("&pageToken=");
            url.push_str(&urlencode(token));
        }
        let value = self.get_json(&url).await?;
        parse_history_page(&value, history_id)
    }

    async fn modify_message(
        &self,
        message_id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<()> {
        ignore_not_found(
            self.post_json(
                &format!("{GMAIL_API}/messages/{}/modify", urlencode(message_id)),
                &json!({ "addLabelIds": add, "removeLabelIds": remove }),
            )
            .await,
        )
    }

    async fn modify_thread(
        &self,
        thread_id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<()> {
        ignore_not_found(
            self.post_json(
                &format!("{GMAIL_API}/threads/{}/modify", urlencode(thread_id)),
                &json!({ "addLabelIds": add, "removeLabelIds": remove }),
            )
            .await,
        )
    }

    async fn trash_message(&self, message_id: &str) -> Result<()> {
        ignore_not_found(
            self.post_json(
                &format!("{GMAIL_API}/messages/{}/trash", urlencode(message_id)),
                &json!({}),
            )
            .await,
        )
    }

    async fn trash_thread(&self, thread_id: &str) -> Result<()> {
        ignore_not_found(
            self.post_json(
                &format!("{GMAIL_API}/threads/{}/trash", urlencode(thread_id)),
                &json!({}),
            )
            .await,
        )
    }

    async fn attachment(&self, message_id: &str, attachment_id: &str) -> Result<Vec<u8>> {
        let value = self
            .get_json(&format!(
                "{GMAIL_API}/messages/{}/attachments/{}",
                urlencode(message_id),
                urlencode(attachment_id)
            ))
            .await?;
        let encoded = value
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| CoreError::Other("Gmail attachment omitted data".into()))?;
        if encoded.len() > MAX_GMAIL_ATTACHMENT_BYTES.saturating_mul(4).div_ceil(3) + 4 {
            return Err(CoreError::Other(format!(
                "Gmail attachment exceeds the {} MiB safety limit",
                MAX_GMAIL_ATTACHMENT_BYTES / (1024 * 1024)
            )));
        }
        let bytes = decode_gmail_data(encoded)?;
        if bytes.len() > MAX_GMAIL_ATTACHMENT_BYTES {
            return Err(CoreError::Other(format!(
                "Gmail attachment exceeds the {} MiB safety limit",
                MAX_GMAIL_ATTACHMENT_BYTES / (1024 * 1024)
            )));
        }
        Ok(bytes)
    }

    async fn create_label(&self, name: &str, color: Option<&str>) -> Result<GmailLabel> {
        let mut body = json!({
            "name": name,
            "labelListVisibility": "labelShow",
            "messageListVisibility": "show"
        });
        if let Some(background) = color.and_then(gmail_palette_color) {
            body["color"] = json!({
                "backgroundColor": background,
                "textColor": contrasting_text(background),
            });
        }
        let value = self
            .post_json(&format!("{GMAIL_API}/labels"), &body)
            .await?;
        Ok(GmailLabel {
            id: required_string(&value, "id")?.to_owned(),
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(name)
                .to_owned(),
            kind: "user".into(),
            background_color: value
                .pointer("/color/backgroundColor")
                .and_then(Value::as_str)
                .map(str::to_owned),
            text_color: value
                .pointer("/color/textColor")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    async fn update_label(&self, provider_id: &str, name: &str, color: Option<&str>) -> Result<()> {
        let mut body = json!({ "name": name });
        if let Some(background) = color.and_then(gmail_palette_color) {
            body["color"] = json!({
                "backgroundColor": background,
                "textColor": contrasting_text(background),
            });
        }
        self.put_json(
            &format!("{GMAIL_API}/labels/{}", urlencode(provider_id)),
            &body,
        )
        .await?;
        Ok(())
    }

    async fn delete_label(&self, provider_id: &str) -> Result<()> {
        ignore_not_found(
            self.delete_json(&format!("{GMAIL_API}/labels/{}", urlencode(provider_id)))
                .await,
        )
    }

    async fn list_drafts(&self) -> Result<Vec<(String, String)>> {
        let mut output = Vec::new();
        let mut page: Option<String> = None;
        let mut seen_page_tokens = HashSet::new();
        for _ in 0..MAX_DRAFT_PAGES {
            let mut url = format!("{GMAIL_API}/drafts?maxResults=500");
            if let Some(token) = page.as_deref() {
                url.push_str("&pageToken=");
                url.push_str(&urlencode(token));
            }
            let value = self.get_json(&url).await?;
            for draft in value
                .get("drafts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if output.len() == MAX_DRAFTS {
                    return Err(CoreError::Network(format!(
                        "Gmail returned more than {MAX_DRAFTS} drafts"
                    )));
                }
                let (Some(draft_id), Some(message_id)) = (
                    draft.get("id").and_then(Value::as_str),
                    draft.pointer("/message/id").and_then(Value::as_str),
                ) else {
                    continue;
                };
                output.push((draft_id.to_owned(), message_id.to_owned()));
            }
            page = gmail_page_token(&value)?;
            match page.as_ref() {
                Some(token) if !seen_page_tokens.insert(token.clone()) => {
                    return Err(CoreError::Network(
                        "Gmail repeated a draft page token".into(),
                    ));
                }
                Some(_) => {}
                None => return Ok(output),
            }
        }
        Err(CoreError::Network(format!(
            "Gmail exceeded the {MAX_DRAFT_PAGES}-page draft safety limit"
        )))
    }

    async fn upsert_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<Value> {
        let body = json!({
            "message": {
                "raw": URL_SAFE_NO_PAD.encode(raw),
                "threadId": thread_id,
            }
        });
        match draft_id.filter(|value| !value.is_empty()) {
            Some(id) => {
                self.put_json(&format!("{GMAIL_API}/drafts/{}", urlencode(id)), &body)
                    .await
            }
            None => self.post_json(&format!("{GMAIL_API}/drafts"), &body).await,
        }
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<()> {
        ignore_not_found(
            self.delete_json(&format!("{GMAIL_API}/drafts/{}", urlencode(draft_id)))
                .await,
        )
    }

    async fn send_draft(&self, draft_id: &str) -> Result<Value> {
        self.post_json(
            &format!("{GMAIL_API}/drafts/send"),
            &json!({ "id": draft_id }),
        )
        .await
    }
}

fn messages_list_url(page_token: Option<&str>, cutoff: Option<chrono::NaiveDate>) -> String {
    let mut url = format!("{GMAIL_API}/messages?maxResults={SYNC_PAGE_SIZE}&includeSpamTrash=true");
    if let Some(cutoff) = cutoff {
        // Gmail interprets date-only search terms in Pacific Time. Use an
        // epoch one second before our inclusive UTC cleanup boundary because
        // Gmail's `after:` operator itself is exclusive.
        let seconds = cutoff
            .and_time(chrono::NaiveTime::MIN)
            .and_utc()
            .timestamp()
            .saturating_sub(1);
        let query = format!("after:{seconds}");
        url.push_str("&q=");
        url.push_str(&urlencode(&query));
    }
    if let Some(token) = page_token.filter(|value| !value.is_empty()) {
        url.push_str("&pageToken=");
        url.push_str(&urlencode(token));
    }
    url
}

/// Provider mutations are replayed from a durable offline queue. If the
/// resource disappeared independently (another client, or a retried request
/// whose response was lost), the requested end state has already been reached.
fn ignore_not_found(result: Result<Value>) -> Result<()> {
    match result {
        Ok(_) | Err(CoreError::NotFound(_)) => Ok(()),
        Err(error) => Err(error),
    }
}

fn network_error(error: reqwest::Error) -> CoreError {
    if error.is_timeout() || error.is_connect() {
        CoreError::Offline
    } else {
        CoreError::Network(error.to_string())
    }
}

async fn google_json(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let body = http_body::text(response, MAX_GMAIL_JSON_BODY_BYTES, "Gmail JSON response").await?;
    if status.is_success() {
        if status == StatusCode::NO_CONTENT || body.trim().is_empty() {
            return Ok(Value::Null);
        }
        return serde_json::from_str(&body)
            .map_err(|error| CoreError::Network(format!("invalid Gmail JSON: {error}")));
    }
    let value = serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);
    let reason = value
        .pointer("/error/errors/0/reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| status.canonical_reason().unwrap_or("Gmail request failed"));
    if status == StatusCode::TOO_MANY_REQUESTS || google_quota_reason(reason) {
        return Err(CoreError::Network(format!("Gmail rate limited: {message}")));
    }
    if status == StatusCode::UNAUTHORIZED
        || (status == StatusCode::FORBIDDEN
            && matches!(
                reason,
                "authError" | "forbidden" | "insufficientPermissions"
            ))
    {
        return Err(CoreError::NeedsReauth);
    }
    if status == StatusCode::NOT_FOUND {
        return Err(CoreError::NotFound(format!("Gmail resource: {message}")));
    }
    if status.is_server_error() {
        return Err(CoreError::Network(format!("Gmail {status}: {message}")));
    }
    Err(CoreError::Other(format!("Gmail {status}: {message}")))
}

fn google_quota_reason(reason: &str) -> bool {
    matches!(
        reason,
        "rateLimitExceeded"
            | "userRateLimitExceeded"
            | "quotaExceeded"
            | "dailyLimitExceeded"
            | "backendError"
    )
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CoreError::Other(format!("Gmail response omitted {field}")))
}

fn urlencode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn build_batch_body(ids: &[String], boundary: &str) -> String {
    let mut output = String::new();
    for (index, id) in ids.iter().enumerate() {
        output.push_str("--");
        output.push_str(boundary);
        output.push_str("\r\nContent-Type: application/http\r\nContent-ID: <flectar-");
        output.push_str(&index.to_string());
        output.push_str(">\r\n\r\nGET /gmail/v1/users/me/messages/");
        output.push_str(&urlencode(id));
        output.push_str("?format=metadata");
        for header in METADATA_HEADERS {
            output.push_str("&metadataHeaders=");
            output.push_str(&urlencode(header));
        }
        output.push_str(" HTTP/1.1\r\nAccept: application/json\r\n\r\n");
    }
    output.push_str("--");
    output.push_str(boundary);
    output.push_str("--\r\n");
    output
}

fn multipart_boundary(content_type: &str) -> Option<&str> {
    content_type.split(';').find_map(|part| {
        part.trim()
            .strip_prefix("boundary=")
            .map(|value| value.trim_matches('"'))
            .filter(|value| !value.is_empty())
    })
}

fn parse_batch_response(body: &str, boundary: &str, expected_parts: usize) -> Result<Vec<Value>> {
    let marker = format!("--{boundary}");
    let mut seen = 0usize;
    let mut messages = Vec::with_capacity(expected_parts);
    for raw in body.split(&marker).skip(1) {
        let part = raw.trim();
        if part.is_empty() || part == "--" || part.starts_with("--\n") || part.starts_with("--\r\n")
        {
            continue;
        }
        seen += 1;
        let (_, nested) = split_header_block(part)
            .ok_or_else(|| CoreError::Network("malformed Gmail batch part".into()))?;
        let (headers, response_body) = split_header_block(nested)
            .ok_or_else(|| CoreError::Network("malformed Gmail batch response".into()))?;
        let status = headers
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u16>().ok())
            .and_then(|value| StatusCode::from_u16(value).ok())
            .ok_or_else(|| CoreError::Network("invalid Gmail batch status".into()))?;
        if status == StatusCode::NOT_FOUND {
            continue;
        }
        if !status.is_success() {
            let error = serde_json::from_str::<Value>(response_body.trim()).unwrap_or(Value::Null);
            let reason = error
                .pointer("/error/errors/0/reason")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let message = error
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| status.to_string());
            return Err(
                if status == StatusCode::TOO_MANY_REQUESTS || google_quota_reason(reason) {
                    CoreError::Network(format!("Gmail batch rate limited: {message}"))
                } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                    CoreError::NeedsReauth
                } else if status.is_server_error() {
                    CoreError::Network(format!("Gmail batch {status}: {message}"))
                } else {
                    CoreError::Other(format!("Gmail batch {status}: {message}"))
                },
            );
        }
        messages.push(
            serde_json::from_str(response_body.trim()).map_err(|error| {
                CoreError::Network(format!("invalid Gmail batch JSON: {error}"))
            })?,
        );
    }
    if seen != expected_parts {
        return Err(CoreError::Network(format!(
            "Gmail batch returned {seen} of {expected_parts} parts"
        )));
    }
    Ok(messages)
}

fn split_header_block(value: &str) -> Option<(&str, &str)> {
    value
        .find("\r\n\r\n")
        .map(|position| (&value[..position], &value[position + 4..]))
        .or_else(|| {
            value
                .find("\n\n")
                .map(|position| (&value[..position], &value[position + 2..]))
        })
}

fn parse_history_page(value: &Value, base_history_id: &str) -> Result<HistoryPage> {
    let mut affected_threads = BTreeSet::new();
    for history in value
        .get("history")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for key in [
            "messages",
            "messagesAdded",
            "messagesDeleted",
            "labelsAdded",
            "labelsRemoved",
        ] {
            for item in history
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let message = item.get("message").unwrap_or(item);
                if let Some(thread_id) = message.get("threadId").and_then(Value::as_str) {
                    if affected_threads.len() >= MAX_HISTORY_THREADS_PER_PAGE
                        && !affected_threads.contains(thread_id)
                    {
                        return Err(CoreError::Network(format!(
                            "Gmail history returned more than {MAX_HISTORY_THREADS_PER_PAGE} threads"
                        )));
                    }
                    affected_threads.insert(thread_id.to_owned());
                }
            }
        }
    }
    let next_page_token = gmail_page_token(value)?;
    let completed_history_id = if next_page_token.is_none() {
        let history_id = value
            .get("historyId")
            .and_then(Value::as_str)
            .unwrap_or(base_history_id);
        if history_id.len() > MAX_GMAIL_PAGE_TOKEN_BYTES {
            return Err(CoreError::Network(
                "Gmail returned an oversized history cursor".into(),
            ));
        }
        Some(history_id.to_owned())
    } else {
        None
    };
    Ok(HistoryPage {
        affected_threads,
        next_page_token,
        completed_history_id,
    })
}

fn gmail_page_token(value: &Value) -> Result<Option<String>> {
    let token = value.get("nextPageToken").and_then(Value::as_str);
    if token.is_some_and(|token| token.len() > MAX_GMAIL_PAGE_TOKEN_BYTES) {
        return Err(CoreError::Network(
            "Gmail returned an oversized page token".into(),
        ));
    }
    Ok(token.filter(|token| !token.is_empty()).map(str::to_owned))
}

fn decode_gmail_data(value: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(value.trim_end_matches('='))
        .map_err(|error| CoreError::Mime(format!("invalid Gmail base64url data: {error}")))
}

fn contrasting_text(background: &str) -> &'static str {
    let value = background.trim_start_matches('#');
    let rgb = u32::from_str_radix(value, 16).unwrap_or(0x6b7280);
    let r = (rgb >> 16) & 0xff;
    let g = (rgb >> 8) & 0xff;
    let b = rgb & 0xff;
    if r * 299 + g * 587 + b * 114 > 150_000 {
        "#000000"
    } else {
        "#ffffff"
    }
}

/// Gmail accepts only its documented label palette, not arbitrary CSS hex.
/// Keep arbitrary local colors local rather than making label creation fail.
fn gmail_palette_color(value: &str) -> Option<&str> {
    const COLORS: &[&str] = &[
        "#000000", "#434343", "#666666", "#999999", "#cccccc", "#efefef", "#f3f3f3", "#ffffff",
        "#fb4c2f", "#ffad46", "#fad165", "#16a765", "#43d692", "#4a86e8", "#a479e2", "#f691b3",
        "#f6c5be", "#ffe6c7", "#fef1d1", "#b9e4d0", "#c6f3de", "#c9daf8", "#e4d7f5", "#fcdee8",
        "#efa093", "#ffd6a2", "#fce8b3", "#89d3b2", "#a0eac9", "#a4c2f4", "#d0bcf1", "#fbc8d9",
        "#e66550", "#ffbc6b", "#fcda83", "#44b984", "#68dfa9", "#6d9eeb", "#b694e8", "#f7a7c0",
        "#cc3a21", "#eaa041", "#f2c960", "#149e60", "#3dc789", "#3c78d8", "#8e63ce", "#e07798",
        "#ac2b16", "#cf8933", "#d5ae49", "#0b804b", "#2a9c68", "#285bac", "#653e9b", "#b65775",
        "#822111", "#a46a21", "#aa8831", "#076239", "#1a764d", "#1c4587", "#41236d", "#83334c",
    ];
    COLORS
        .iter()
        .copied()
        .find(|allowed| allowed.eq_ignore_ascii_case(value))
}

fn system_role(label_id: &str) -> Option<&'static str> {
    match label_id {
        "INBOX" => Some(roles::INBOX),
        "SENT" => Some(roles::SENT),
        "DRAFT" => Some(roles::DRAFTS),
        "TRASH" => Some(roles::TRASH),
        "SPAM" => Some(roles::SPAM),
        _ => None,
    }
}

async fn sync_labels(ctx: &SyncCtx, config: &AccountConfig, api: &GmailApi) -> Result<()> {
    let labels = api.labels().await?;
    let account_id = config.id;
    ctx.db
        .write(move |conn| {
            let tx = conn.transaction()?;

            // Archive is a Gmail state (absence of INBOX), not a provider
            // label. All Mail is similarly synthetic for the existing folder
            // UI; every Gmail message receives that membership.
            repo::folders::upsert(
                &tx,
                account_id,
                "[Gmail]/Archive",
                Some("/"),
                Some(roles::ARCHIVE),
            )?;
            repo::folders::upsert(
                &tx,
                account_id,
                "[Gmail]/All Mail",
                Some("/"),
                Some(roles::ALL),
            )?;

            for (position, label) in labels.iter().enumerate() {
                let role = system_role(&label.id);
                let folder_id = if role.is_some() || label.kind == "user" {
                    let existing: Option<i64> = tx
                        .query_row(
                            "SELECT folder_id FROM gmail_labels
                             WHERE account_id = ?1 AND provider_id = ?2",
                            params![account_id, label.id],
                            |row| row.get(0),
                        )
                        .optional()?
                        .flatten();
                    let folder_name = if label.kind == "user" {
                        label.name.as_str()
                    } else {
                        label.id.as_str()
                    };
                    Some(match existing {
                        Some(folder_id) => {
                            tx.execute(
                                "UPDATE folders SET imap_name = ?2, delimiter = '/', role = ?3
                                 WHERE id = ?1",
                                params![folder_id, folder_name, role],
                            )?;
                            folder_id
                        }
                        None => {
                            repo::folders::upsert(&tx, account_id, folder_name, Some("/"), role)?
                        }
                    })
                } else {
                    None
                };

                let previous_local_label_id: Option<i64> = tx
                    .query_row(
                        "SELECT local_label_id FROM gmail_labels
                         WHERE account_id = ?1 AND provider_id = ?2",
                        params![account_id, label.id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .flatten();
                let local_label_id = if label.kind == "user" {
                    let existing: Option<i64> = tx
                        .query_row(
                            "SELECT id FROM labels WHERE name = ?1 AND COALESCE(is_auto, 0) = 0",
                            params![label.name],
                            |row| row.get(0),
                        )
                        .optional()?;
                    Some(match existing {
                        Some(id) => {
                            tx.execute(
                                "UPDATE labels SET color = ?2 WHERE id = ?1",
                                params![id, label.background_color.as_deref().unwrap_or("#6b7280")],
                            )?;
                            id
                        }
                        None => {
                            // An auto-category may own the plain display name.
                            // Keep the provider label distinct rather than
                            // converting a local-only classifier into a remote
                            // Gmail label.
                            let name_exists: bool = tx.query_row(
                                "SELECT EXISTS(SELECT 1 FROM labels WHERE name = ?1)",
                                params![label.name],
                                |row| row.get(0),
                            )?;
                            let display_name = if name_exists {
                                format!("{} (Gmail)", label.name)
                            } else {
                                label.name.clone()
                            };
                            tx.execute(
                                "INSERT OR IGNORE INTO labels (name, color, keyword, position)
                                 VALUES (?1, ?2, ?3, ?4)",
                                params![
                                    display_name,
                                    label.background_color.as_deref().unwrap_or("#6b7280"),
                                    repo::labels::keyword_for(&display_name),
                                    position as i64,
                                ],
                            )?;
                            tx.query_row(
                                "SELECT id FROM labels WHERE name = ?1",
                                params![display_name],
                                |row| row.get(0),
                            )?
                        }
                    })
                } else {
                    None
                };

                if let (Some(previous), Some(current)) = (previous_local_label_id, local_label_id)
                    && previous != current
                {
                    // An external rename can resolve to a different global
                    // local label (for example because an auto-category owns
                    // the new display name). Move only this Gmail account's
                    // memberships so old and new chips are not both shown.
                    tx.execute(
                        "INSERT OR IGNORE INTO message_labels (message_id, label_id)
                             SELECT ml.message_id, ?3
                             FROM message_labels ml
                             JOIN messages m ON m.id = ml.message_id
                             WHERE m.account_id = ?1 AND ml.label_id = ?2",
                        params![account_id, previous, current],
                    )?;
                    tx.execute(
                        "DELETE FROM message_labels
                             WHERE label_id = ?2 AND message_id IN (
                               SELECT id FROM messages WHERE account_id = ?1
                             )",
                        params![account_id, previous],
                    )?;
                }

                tx.execute(
                    "INSERT INTO gmail_labels (
                         account_id, provider_id, name, kind, folder_id,
                         local_label_id, background_color, text_color
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(account_id, provider_id) DO UPDATE SET
                         name = excluded.name,
                         kind = excluded.kind,
                         folder_id = excluded.folder_id,
                         local_label_id = excluded.local_label_id,
                         background_color = excluded.background_color,
                         text_color = excluded.text_color",
                    params![
                        account_id,
                        label.id,
                        label.name,
                        label.kind,
                        folder_id,
                        local_label_id,
                        label.background_color,
                        label.text_color,
                    ],
                )?;
            }

            // labels.list is authoritative. Remove provider mappings and their
            // account-scoped folder when a label was deleted in another Gmail
            // client. Keep the global local label itself: routing rules or
            // another account may still use it, and applying it again can
            // recreate the provider label lazily.
            let seen = labels
                .iter()
                .map(|label| label.id.as_str())
                .collect::<HashSet<_>>();
            let stale = {
                let mut stmt = tx.prepare(
                    "SELECT provider_id, folder_id FROM gmail_labels
                     WHERE account_id = ?1",
                )?;
                stmt.query_map(params![account_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter(|(provider_id, _)| !seen.contains(provider_id.as_str()))
                .collect::<Vec<_>>()
            };
            for (provider_id, folder_id) in stale {
                tx.execute(
                    "DELETE FROM gmail_labels
                     WHERE account_id = ?1 AND provider_id = ?2",
                    params![account_id, provider_id],
                )?;
                if let Some(folder_id) = folder_id {
                    // Only user-label folders are removed here. A missing
                    // system label must never erase a canonical mailbox.
                    tx.execute(
                        "DELETE FROM folders WHERE id = ?1 AND role IS NULL",
                        params![folder_id],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
}

pub(crate) async fn create_user_folder(
    ctx: &SyncCtx,
    config: &AccountConfig,
    name: &str,
) -> Result<()> {
    let api = GmailApi::new(ctx.clone(), config.clone())?;
    api.create_label(name, None).await?;
    sync_labels(ctx, config, &api).await
}

pub(crate) async fn rename_user_folder(
    ctx: &SyncCtx,
    config: &AccountConfig,
    folder_id: i64,
    name: &str,
) -> Result<()> {
    let account_id = config.id;
    let provider_id = ctx
        .db
        .read(move |conn| repo::gmail::provider_label_for_folder(conn, account_id, folder_id))
        .await?
        .ok_or_else(|| CoreError::NotFound(format!("Gmail folder {folder_id}")))?;
    let api = GmailApi::new(ctx.clone(), config.clone())?;
    api.update_label(&provider_id, name, None).await?;
    sync_labels(ctx, config, &api).await
}

pub(crate) async fn delete_user_folder(
    ctx: &SyncCtx,
    config: &AccountConfig,
    folder_id: i64,
) -> Result<()> {
    let account_id = config.id;
    let provider_id = ctx
        .db
        .read(move |conn| repo::gmail::provider_label_for_folder(conn, account_id, folder_id))
        .await?
        .ok_or_else(|| CoreError::NotFound(format!("Gmail folder {folder_id}")))?;
    let api = GmailApi::new(ctx.clone(), config.clone())?;
    api.delete_label(&provider_id).await?;
    sync_labels(ctx, config, &api).await
}

#[derive(Debug, Clone)]
struct ParsedResource {
    provider_id: String,
    thread_id: String,
    labels: Vec<String>,
    headers: crate::mime::ParsedHeaders,
    internal_date: i64,
    size: Option<i64>,
    snippet: String,
    has_attachments: bool,
}

fn parse_resource(value: &Value) -> Result<ParsedResource> {
    let provider_id = required_string(value, "id")?.to_owned();
    let thread_id = required_string(value, "threadId")?.to_owned();
    let payload = value.get("payload").unwrap_or(&Value::Null);
    let mut raw_headers = Vec::new();
    for header in payload
        .get("headers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = header.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(header_value) = header.get("value").and_then(Value::as_str) else {
            continue;
        };
        let additional = name
            .len()
            .saturating_add(header_value.len())
            .saturating_add(4);
        if additional > MAX_GMAIL_HEADERS_BYTES.saturating_sub(raw_headers.len()) {
            return Err(CoreError::Mime(format!(
                "Gmail message headers exceed the {} KiB safety limit",
                MAX_GMAIL_HEADERS_BYTES / 1024
            )));
        }
        raw_headers.extend_from_slice(name.as_bytes());
        raw_headers.extend_from_slice(b": ");
        raw_headers.extend_from_slice(header_value.as_bytes());
        raw_headers.extend_from_slice(b"\r\n");
    }
    raw_headers.extend_from_slice(b"\r\n");
    let headers = crate::mime::parse_header_block(&raw_headers)?;
    let internal_date = value
        .get("internalDate")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or_else(now_ms);
    Ok(ParsedResource {
        provider_id,
        thread_id,
        labels: value
            .get("labelIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        headers,
        internal_date,
        size: value.get("sizeEstimate").and_then(Value::as_i64),
        snippet: value
            .get("snippet")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        has_attachments: payload_has_file_attachments(payload)?,
    })
}

/// A bounded account still accepts an entire Gmail thread when at least one
/// message is recent or the thread is already represented locally. This keeps
/// new replies readable in context and keeps previously downloaded old mail
/// consistent, without importing unrelated old threads after label changes.
async fn retain_history_threads(
    ctx: &SyncCtx,
    account_id: i64,
    resources: Vec<Value>,
    mail_history: MailHistory,
) -> Result<Vec<Value>> {
    let Some(cutoff) = mail_history.cutoff_ms_at(now_ms()) else {
        return Ok(resources);
    };

    let mut accepted_threads = HashSet::<String>::new();
    let mut old_resources = Vec::<(String, String)>::new();
    for resource in &resources {
        let Some(provider_id) = resource.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(thread_id) = resource.get("threadId").and_then(Value::as_str) else {
            continue;
        };
        let internal_date = resource
            .get("internalDate")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or_default();
        if internal_date >= cutoff {
            accepted_threads.insert(thread_id.to_owned());
        } else {
            old_resources.push((provider_id.to_owned(), thread_id.to_owned()));
        }
    }

    let existing_threads = ctx
        .db
        .read(move |conn| {
            let mut existing = HashSet::new();
            for (provider_id, thread_id) in &old_resources {
                if repo::messages::by_gm_msgid(conn, account_id, provider_id)?.is_some() {
                    existing.insert(thread_id.clone());
                }
            }
            Ok(existing)
        })
        .await?;
    accepted_threads.extend(existing_threads);

    Ok(resources
        .into_iter()
        .filter(|resource| {
            resource
                .get("threadId")
                .and_then(Value::as_str)
                .is_some_and(|thread_id| accepted_threads.contains(thread_id))
        })
        .collect())
}

fn payload_has_file_attachments(root: &Value) -> Result<bool> {
    let mut stack = vec![root];
    let mut visited = 0_usize;
    while let Some(part) = stack.pop() {
        visited += 1;
        if visited > MAX_GMAIL_MIME_PARTS {
            return Err(CoreError::Mime(format!(
                "Gmail MIME tree exceeds the {MAX_GMAIL_MIME_PARTS}-part safety limit"
            )));
        }
        let filename = part
            .get("filename")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let disposition = part_headers(part)
            .get("content-disposition")
            .map(String::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if disposition.starts_with("attachment")
            || (!filename.is_empty() && !disposition.starts_with("inline"))
        {
            return Ok(true);
        }
        if let Some(children) = part.get("parts").and_then(Value::as_array) {
            stack.extend(children);
        }
    }
    Ok(false)
}

#[derive(Debug, Clone)]
struct LabelMapping {
    folder_id: Option<i64>,
    local_label_id: Option<i64>,
    role: Option<String>,
}

#[derive(Default)]
struct StoreOutcome {
    touched_threads: Vec<i64>,
    fresh_threads: Vec<i64>,
}

async fn store_resources(
    ctx: &SyncCtx,
    config: &AccountConfig,
    resources: &[Value],
    generation: Option<i64>,
    complete_threads: bool,
    notify_new: bool,
    notification_cutoff: Option<i64>,
) -> Result<StoreOutcome> {
    let parsed = resources
        .iter()
        .map(parse_resource)
        .collect::<Result<Vec<_>>>()?;
    if parsed.is_empty() {
        return Ok(StoreOutcome::default());
    }
    let account_id = config.id;
    let account_email = config.email.to_ascii_lowercase();
    ctx.db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let settings = repo::settings::get(&tx)?;
            let mut mappings = HashMap::<String, LabelMapping>::new();
            {
                let mut stmt = tx.prepare(
                    "SELECT gl.provider_id, gl.folder_id, gl.local_label_id, f.role
                     FROM gmail_labels gl
                     LEFT JOIN folders f ON f.id = gl.folder_id
                     WHERE gl.account_id = ?1",
                )?;
                let rows = stmt.query_map(params![account_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        LabelMapping {
                            folder_id: row.get(1)?,
                            local_label_id: row.get(2)?,
                            role: row.get(3)?,
                        },
                    ))
                })?;
                for row in rows {
                    let (id, mapping) = row?;
                    mappings.insert(id, mapping);
                }
            }
            let archive_id = repo::folders::by_role(&tx, account_id, roles::ARCHIVE)?
                .map(|folder| folder.id)
                .ok_or_else(|| CoreError::NotFound("Gmail archive folder".into()))?;
            let all_id = repo::folders::by_role(&tx, account_id, roles::ALL)?
                .map(|folder| folder.id)
                .ok_or_else(|| CoreError::NotFound("Gmail all-mail folder".into()))?;

            let provider_local_labels = mappings
                .values()
                .filter_map(|mapping| mapping.local_label_id)
                .collect::<HashSet<_>>();
            let mut outcome = StoreOutcome::default();
            let mut complete_by_thread = HashMap::<String, HashSet<String>>::new();
            let mut old_threads = Vec::<i64>::new();

            for resource in parsed {
                let thread_id =
                    match repo::threads::by_gm_thrid(&tx, account_id, &resource.thread_id)? {
                        Some(id) => id,
                        None => repo::threads::create(
                            &tx,
                            account_id,
                            Some(&resource.thread_id),
                            &crate::mime::normalize_subject(&resource.headers.subject),
                        )?,
                    };
                complete_by_thread
                    .entry(resource.thread_id.clone())
                    .or_default()
                    .insert(resource.provider_id.clone());

                let label_set = resource.labels.iter().cloned().collect::<HashSet<_>>();
                let mut folder_ids = Vec::<i64>::new();
                let mut role_folders = HashMap::<String, i64>::new();
                let mut present_local_labels = Vec::<i64>::new();
                for label in &resource.labels {
                    if let Some(mapping) = mappings.get(label) {
                        if let Some(folder_id) = mapping.folder_id {
                            folder_ids.push(folder_id);
                            if let Some(role) = &mapping.role {
                                role_folders.insert(role.clone(), folder_id);
                            }
                        }
                        if let Some(local_id) = mapping.local_label_id {
                            present_local_labels.push(local_id);
                        }
                    }
                }
                let archived = !label_set.contains("INBOX")
                    && !label_set.contains("SENT")
                    && !label_set.contains("DRAFT")
                    && !label_set.contains("TRASH")
                    && !label_set.contains("SPAM");
                if archived {
                    folder_ids.push(archive_id);
                    role_folders.insert(roles::ARCHIVE.into(), archive_id);
                }
                folder_ids.push(all_id);
                role_folders.insert(roles::ALL.into(), all_id);
                folder_ids.sort_unstable();
                folder_ids.dedup();

                let canonical_folder = [
                    roles::TRASH,
                    roles::SPAM,
                    roles::DRAFTS,
                    roles::INBOX,
                    roles::SENT,
                    roles::ARCHIVE,
                    roles::ALL,
                ]
                .into_iter()
                .find_map(|role| role_folders.get(role).copied())
                .unwrap_or(all_id);

                let date = resource.headers.date_ms.unwrap_or(resource.internal_date);
                let from_email = resource
                    .headers
                    .from
                    .as_ref()
                    .map(|address| address.email.to_ascii_lowercase())
                    .unwrap_or_default();
                let is_outgoing = label_set.contains("SENT") || from_email == account_email;
                let is_read = !label_set.contains("UNREAD");
                let is_starred = label_set.contains("STARRED");
                let is_draft = label_set.contains("DRAFT");

                let existing = repo::messages::by_gm_msgid(&tx, account_id, &resource.provider_id)?;
                let inserted = existing.is_none();
                let local_message_id = if let Some(existing) = existing {
                    if let Some(old_thread) = existing.thread_id.filter(|id| *id != thread_id) {
                        old_threads.push(old_thread);
                    }
                    tx.execute(
                        "UPDATE messages SET
                             thread_id = ?2, folder_id = ?3, uid = NULL,
                             gm_thrid = ?4, subject = ?5, from_name = ?6,
                             from_addr = ?7, to_json = ?8, cc_json = ?9,
                             bcc_json = ?10, date = ?11, internal_date = ?12,
                             is_read = ?13, is_starred = ?14, is_draft = ?15,
                             is_outgoing = ?16, is_automated = ?17,
                             has_attachments = ?18, size = ?19, snippet = ?20,
                             list_unsubscribe = ?21, list_unsubscribe_post = ?22,
                             sender_addr = ?23, sender_verification = ?24,
                             gmail_sync_generation = COALESCE(?25, gmail_sync_generation)
                         WHERE id = ?1",
                        params![
                            existing.id,
                            thread_id,
                            canonical_folder,
                            resource.thread_id,
                            resource.headers.subject,
                            resource
                                .headers
                                .from
                                .as_ref()
                                .and_then(|a| a.name.as_deref()),
                            resource.headers.from.as_ref().map(|a| a.email.as_str()),
                            serde_json::to_string(&resource.headers.to)?,
                            serde_json::to_string(&resource.headers.cc)?,
                            serde_json::to_string(&resource.headers.bcc)?,
                            date,
                            resource.internal_date,
                            is_read as i64,
                            is_starred as i64,
                            is_draft as i64,
                            is_outgoing as i64,
                            resource.headers.is_automated as i64,
                            resource.has_attachments as i64,
                            resource.size,
                            resource.snippet,
                            resource.headers.list_unsubscribe,
                            resource.headers.list_unsubscribe_post,
                            resource.headers.via,
                            resource.headers.sender_verification.as_str(),
                            generation,
                        ],
                    )?;
                    existing.id
                } else {
                    let message = NewMessage {
                        account_id,
                        folder_id: canonical_folder,
                        uid: None,
                        message_id: resource.headers.message_id.clone(),
                        gm_msgid: Some(resource.provider_id.clone()),
                        gm_thrid: Some(resource.thread_id.clone()),
                        subject: resource.headers.subject.clone(),
                        from: resource.headers.from.clone(),
                        to: resource.headers.to.clone(),
                        cc: resource.headers.cc.clone(),
                        bcc: resource.headers.bcc.clone(),
                        date,
                        internal_date: Some(resource.internal_date),
                        is_read,
                        is_starred,
                        is_draft,
                        is_outgoing,
                        is_automated: resource.headers.is_automated,
                        has_attachments: resource.has_attachments,
                        size: resource.size,
                        snippet: resource.snippet.clone(),
                        references: resource.headers.references.clone(),
                        list_unsubscribe: resource.headers.list_unsubscribe.clone(),
                        list_unsubscribe_post: resource.headers.list_unsubscribe_post.clone(),
                        sender_addr: resource.headers.via.clone(),
                        sender_verification: resource.headers.sender_verification,
                    };
                    let id = repo::messages::insert(&tx, &message, thread_id)?;
                    tx.execute(
                        "UPDATE messages SET gmail_sync_generation = ?2 WHERE id = ?1",
                        params![id, generation],
                    )?;
                    id
                };

                repo::gmail::set_message_folders(&tx, local_message_id, &folder_ids)?;
                for local_id in &provider_local_labels {
                    if !present_local_labels.contains(local_id) {
                        repo::labels::remove_from_message(&tx, local_message_id, *local_id)?;
                    }
                }
                for local_id in present_local_labels {
                    repo::labels::add_to_message(&tx, local_message_id, local_id)?;
                }
                repo::search::index_message(&tx, local_message_id)?;

                if is_outgoing {
                    for address in resource.headers.to.iter().chain(resource.headers.cc.iter()) {
                        repo::contacts::harvest(&tx, account_id, address, true, date)?;
                    }
                } else if let Some(from) = &resource.headers.from {
                    repo::contacts::harvest(&tx, account_id, from, false, date)?;
                }

                if !outcome.touched_threads.contains(&thread_id) {
                    outcome.touched_threads.push(thread_id);
                }
                if notify_new
                    && inserted
                    && !is_read
                    && !is_outgoing
                    && notification_cutoff.is_none_or(|cutoff| resource.internal_date >= cutoff)
                    && now_ms().saturating_sub(resource.internal_date) < 24 * 60 * 60 * 1000
                {
                    repo::notifications::enqueue(&tx, local_message_id)?;
                    if !resource.headers.is_automated
                        && !crate::mime::robot_sender(&from_email)
                        && !outcome.fresh_threads.contains(&thread_id)
                    {
                        outcome.fresh_threads.push(thread_id);
                    }
                }
            }

            if complete_threads {
                for (provider_thread_id, ids) in &complete_by_thread {
                    let local_thread =
                        repo::threads::by_gm_thrid(&tx, account_id, provider_thread_id)?;
                    let Some(local_thread) = local_thread else {
                        continue;
                    };
                    let stale = {
                        let mut stmt = tx.prepare(
                            "SELECT id, gm_msgid FROM messages
                             WHERE account_id = ?1 AND thread_id = ?2
                               AND gm_msgid IS NOT NULL",
                        )?;
                        stmt.query_map(params![account_id, local_thread], |row| {
                            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                        .into_iter()
                        .filter_map(|(id, gm)| (!ids.contains(&gm)).then_some(id))
                        .collect::<Vec<_>>()
                    };
                    for id in stale {
                        repo::messages::delete(&tx, id)?;
                    }
                }
            }

            outcome.touched_threads.extend(old_threads);
            outcome.touched_threads.sort_unstable();
            outcome.touched_threads.dedup();
            for &thread_id in &outcome.touched_threads {
                repo::threads::recompute(&tx, thread_id)?;
            }
            if settings.auto_labels_enabled {
                let splits = repo::splits::list(&tx)?;
                for &thread_id in &outcome.touched_threads {
                    if repo::threads::get_summary(&tx, thread_id)?.is_some() {
                        crate::route::route_thread_deterministic(
                            &tx,
                            &splits,
                            settings.ai_categorize,
                            thread_id,
                        )?;
                    }
                }
            }
            tx.commit()?;
            Ok(outcome)
        })
        .await
}

async fn remove_provider_thread(
    ctx: &SyncCtx,
    account_id: i64,
    provider_thread_id: String,
) -> Result<Vec<i64>> {
    ctx.db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let Some(thread_id) = repo::threads::by_gm_thrid(&tx, account_id, &provider_thread_id)?
            else {
                tx.commit()?;
                return Ok(Vec::new());
            };
            let ids = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM messages
                     WHERE account_id = ?1 AND thread_id = ?2 AND gm_msgid IS NOT NULL",
                )?;
                stmt.query_map(params![account_id, thread_id], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<i64>>>()?
            };
            for id in ids {
                repo::messages::delete(&tx, id)?;
            }
            repo::threads::recompute(&tx, thread_id)?;
            tx.commit()?;
            Ok(vec![thread_id])
        })
        .await
}

async fn cleanup_full_generation(
    ctx: &SyncCtx,
    account_id: i64,
    generation: i64,
    started_at: i64,
    cutoff: Option<i64>,
) -> Result<Vec<i64>> {
    ctx.db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let stale = {
                let mut stmt = tx.prepare(
                    "SELECT id, thread_id FROM messages
                     WHERE account_id = ?1 AND gm_msgid IS NOT NULL
                       AND COALESCE(gmail_sync_generation, 0) <> ?2
                       AND COALESCE(internal_date, date) < ?3
                       AND (?4 IS NULL OR COALESCE(internal_date, date) >= ?4)
                       AND NOT EXISTS (
                         SELECT 1 FROM pending_actions pa
                         WHERE pa.message_id = messages.id
                           AND pa.state IN ('pending','inflight')
                       )",
                )?;
                stmt.query_map(params![account_id, generation, started_at, cutoff], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
            };
            let mut threads = Vec::new();
            for (id, thread_id) in stale {
                repo::messages::delete(&tx, id)?;
                if let Some(thread_id) = thread_id {
                    threads.push(thread_id);
                }
            }
            threads.sort_unstable();
            threads.dedup();
            for thread_id in &threads {
                repo::threads::recompute(&tx, *thread_id)?;
            }
            tx.commit()?;
            Ok(threads)
        })
        .await
}

async fn sync_draft_map(ctx: &SyncCtx, config: &AccountConfig, api: &GmailApi) -> Result<()> {
    let drafts = api.list_drafts().await?;
    let account_id = config.id;
    ctx.db
        .write(move |conn| {
            conn.execute(
                "UPDATE messages SET gmail_draft_id = NULL
                 WHERE account_id = ?1 AND is_draft = 1 AND gm_msgid IS NOT NULL",
                params![account_id],
            )?;
            for (draft_id, message_id) in drafts {
                repo::gmail::set_draft_ids(conn, account_id, &message_id, &draft_id)?;
            }
            Ok(())
        })
        .await
}

fn part_headers(part: &Value) -> BTreeMap<String, String> {
    part.get("headers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|header| {
            Some((
                header.get("name")?.as_str()?.to_ascii_lowercase(),
                header.get("value")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

#[derive(Debug, Clone)]
struct PartSpec {
    part_id: String,
    mime_type: String,
    filename: Option<String>,
    content_id: Option<String>,
    is_inline: bool,
    is_attachment: bool,
    charset: Option<String>,
    provider_attachment_id: Option<String>,
    encoded_data: Option<String>,
    size: i64,
}

fn collect_part_specs(part: &Value, output: &mut Vec<PartSpec>) -> Result<()> {
    let mut visited = 0_usize;
    collect_part_specs_inner(part, output, &mut visited)
}

fn collect_part_specs_inner(
    part: &Value,
    output: &mut Vec<PartSpec>,
    visited: &mut usize,
) -> Result<()> {
    *visited += 1;
    if *visited > MAX_GMAIL_MIME_PARTS {
        return Err(CoreError::Mime(format!(
            "Gmail MIME tree exceeds the {MAX_GMAIL_MIME_PARTS}-part safety limit"
        )));
    }
    let mime_type = part
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream")
        .to_ascii_lowercase();
    let filename = part
        .get("filename")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let headers = part_headers(part);
    let disposition = headers
        .get("content-disposition")
        .map(String::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let charset = headers.get("content-type").and_then(|value| {
        value.split(';').skip(1).find_map(|parameter| {
            let (name, value) = parameter.trim().split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| value.trim().trim_matches(['\'', '"']).to_owned())
        })
    });
    let content_id = headers
        .get("content-id")
        .map(|value| value.trim().trim_matches(['<', '>']).to_owned())
        .filter(|value| !value.is_empty());
    let body = part.get("body").unwrap_or(&Value::Null);
    let has_body = body.get("data").and_then(Value::as_str).is_some()
        || body.get("attachmentId").and_then(Value::as_str).is_some();
    if has_body {
        let is_attachment = filename.is_some() || disposition.starts_with("attachment");
        output.push(PartSpec {
            part_id: part
                .get("partId")
                .and_then(Value::as_str)
                .unwrap_or("0")
                .to_owned(),
            mime_type,
            filename,
            content_id: content_id.clone(),
            is_inline: disposition.starts_with("inline")
                || (content_id.is_some() && !disposition.starts_with("attachment")),
            is_attachment,
            charset,
            provider_attachment_id: body
                .get("attachmentId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            encoded_data: body.get("data").and_then(Value::as_str).map(str::to_owned),
            size: body.get("size").and_then(Value::as_i64).unwrap_or(0),
        });
    }
    for child in part
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        collect_part_specs_inner(child, output, visited)?;
    }
    Ok(())
}

#[derive(Debug)]
struct HydratedAttachment {
    part_id: String,
    provider_attachment_id: Option<String>,
    filename: Option<String>,
    mime_type: String,
    size: i64,
    content_id: Option<String>,
    is_inline: bool,
    inline_bytes: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
struct HydratedBody {
    text: Option<String>,
    html: Option<String>,
    calendar_parts: Vec<String>,
    attachments: Vec<HydratedAttachment>,
}

async fn hydrate_parts(api: &GmailApi, resource: &Value) -> Result<HydratedBody> {
    let message_id = required_string(resource, "id")?;
    let mut specs = Vec::new();
    collect_part_specs(resource.get("payload").unwrap_or(&Value::Null), &mut specs)?;
    let mut hydrated = HydratedBody::default();
    for spec in specs {
        let bytes = match (&spec.encoded_data, &spec.provider_attachment_id) {
            (Some(data), _) => decode_gmail_data(data)?,
            (None, Some(attachment_id)) => api.attachment(message_id, attachment_id).await?,
            (None, None) => Vec::new(),
        };
        let is_text_body = !spec.is_attachment
            && !spec.mime_type.eq_ignore_ascii_case("message/rfc822")
            && (spec.mime_type == "text/plain"
                || spec.mime_type == "text/html"
                || spec.mime_type == "text/calendar");
        if is_text_body {
            let decoded = decode_text_part(&bytes, spec.charset.as_deref());
            match spec.mime_type.as_str() {
                "text/plain" if hydrated.text.is_none() => hydrated.text = Some(decoded),
                "text/html" if hydrated.html.is_none() => {
                    hydrated.html = Some(crate::mime::sanitize_html(&decoded))
                }
                "text/calendar" => hydrated.calendar_parts.push(decoded),
                _ => {}
            }
            continue;
        }
        let has_provider_attachment = spec.provider_attachment_id.is_some();
        hydrated.attachments.push(HydratedAttachment {
            part_id: spec.part_id,
            provider_attachment_id: spec.provider_attachment_id,
            filename: spec.filename,
            mime_type: spec.mime_type,
            size: if spec.size > 0 {
                spec.size
            } else {
                bytes.len() as i64
            },
            content_id: spec.content_id,
            is_inline: spec.is_inline,
            // Attachment endpoints can always be fetched again. Preserve bytes
            // only when Gmail embedded them directly and supplied no opaque id.
            inline_bytes: (!has_provider_attachment).then_some(bytes),
        });
    }
    if hydrated.text.is_none() {
        hydrated.text = hydrated.html.as_deref().map(html_to_text);
    }
    if hydrated.html.is_none() {
        hydrated.html = hydrated
            .text
            .as_deref()
            .map(|text| format!("<pre>{}</pre>", escape_html(text)));
    }
    Ok(hydrated)
}

fn decode_text_part(bytes: &[u8], charset: Option<&str>) -> String {
    let encoding = charset
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    let (decoded, _, _) = encoding.decode(bytes);
    decoded.into_owned()
}

fn html_to_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut in_tag = false;
    for character in value.chars() {
        match character {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                output.push(' ');
            }
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    crate::mime::collapse_whitespace(&output, None)
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn store_hydrated_body(
    ctx: &SyncCtx,
    config: &AccountConfig,
    provider_message_id: String,
    body: HydratedBody,
) -> Result<Option<i64>> {
    let account_id = config.id;
    let text = body.text.clone();
    let html = body.html.clone();
    let snippet = crate::mime::make_body_snippet(text.as_deref(), html.as_deref());
    let has_files = body
        .attachments
        .iter()
        .any(|attachment| !attachment.is_inline);
    let attachments = body.attachments;
    let calendar_parts = body.calendar_parts;
    let parsed_calendar_events: Vec<_> = calendar_parts
        .iter()
        .flat_map(|ics| crate::calendar::parse_ics(ics))
        .collect();
    let (local_message_id, thread_id, cached_inline) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let local_message_id: i64 = tx
                .query_row(
                    "SELECT id FROM messages WHERE account_id = ?1 AND gm_msgid = ?2",
                    params![account_id, provider_message_id],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| CoreError::NotFound("Gmail message".into()))?;
            repo::messages::store_body(
                &tx,
                local_message_id,
                text.as_deref(),
                html.as_deref(),
                None,
                has_files,
                Some(&snippet),
            )?;

            let mut seen_ids = Vec::<i64>::new();
            let mut cached_inline = Vec::<(i64, Vec<u8>)>::new();
            for attachment in attachments {
                let existing: Option<i64> =
                    if let Some(provider_id) = attachment.provider_attachment_id.as_deref() {
                        tx.query_row(
                            "SELECT id FROM attachments
                         WHERE message_id = ?1 AND provider_attachment_id = ?2",
                            params![local_message_id, provider_id],
                            |row| row.get(0),
                        )
                        .optional()?
                    } else {
                        tx.query_row(
                            "SELECT id FROM attachments
                         WHERE message_id = ?1 AND part_id = ?2
                           AND provider_attachment_id IS NULL",
                            params![local_message_id, attachment.part_id],
                            |row| row.get(0),
                        )
                        .optional()?
                    };
                let id = match existing {
                    Some(id) => {
                        tx.execute(
                            "UPDATE attachments SET
                                 part_id = ?2, provider_attachment_id = ?3,
                                 filename = ?4, mime_type = ?5, size = ?6,
                                 content_id = ?7, is_inline = ?8
                             WHERE id = ?1",
                            params![
                                id,
                                attachment.part_id,
                                attachment.provider_attachment_id,
                                attachment.filename,
                                attachment.mime_type,
                                attachment.size,
                                attachment.content_id,
                                attachment.is_inline as i64,
                            ],
                        )?;
                        id
                    }
                    None => {
                        tx.execute(
                            "INSERT INTO attachments (
                                 message_id, part_id, provider_attachment_id,
                                 filename, mime_type, size, content_id, is_inline
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            params![
                                local_message_id,
                                attachment.part_id,
                                attachment.provider_attachment_id,
                                attachment.filename,
                                attachment.mime_type,
                                attachment.size,
                                attachment.content_id,
                                attachment.is_inline as i64,
                            ],
                        )?;
                        tx.last_insert_rowid()
                    }
                };
                seen_ids.push(id);
                if let Some(bytes) = attachment.inline_bytes {
                    cached_inline.push((id, bytes));
                }
            }

            // The full Gmail payload is authoritative. A changed draft can
            // replace an attachment id while the older part is cached; keeping
            // that descriptor would show a removed file and could add it back
            // on the next save.
            let stale = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM attachments
                     WHERE message_id = ?1 AND imap_section IS NULL",
                )?;
                stmt.query_map(params![local_message_id], |row| row.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            for id in stale {
                if !seen_ids.contains(&id) {
                    tx.execute("DELETE FROM attachments WHERE id = ?1", params![id])?;
                }
            }

            repo::search::index_message(&tx, local_message_id)?;
            let thread_id =
                repo::messages::get_row(&tx, local_message_id)?.and_then(|row| row.thread_id);
            if let Some(thread_id) = thread_id {
                repo::threads::recompute(&tx, thread_id)?;
            }
            tx.commit()?;
            Ok((local_message_id, thread_id, cached_inline))
        })
        .await?;

    if !parsed_calendar_events.is_empty() {
        ctx.calendar_db
            .write(move |conn| {
                let tx = conn.transaction()?;
                for event in &parsed_calendar_events {
                    repo::calendar::upsert(&tx, account_id, local_message_id, event)?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        ctx.bus.emit(CoreEvent::CalendarUpdated { account_id });
    }

    for (attachment_id, bytes) in cached_inline {
        let dir = ctx
            .paths
            .attachments_dir(config.id)
            .join(attachment_id.to_string());
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join("gmail-inline-part");
        crate::file_io::write_atomic(&path, &bytes, "Gmail inline attachment cache").await?;
        let path = path.to_string_lossy().to_string();
        ctx.db
            .write(move |conn| {
                conn.execute(
                    "UPDATE attachments SET file_path = ?2 WHERE id = ?1",
                    params![attachment_id, path],
                )?;
                Ok(())
            })
            .await?;
    }
    Ok(thread_id)
}

async fn hydrate_thread(
    ctx: &SyncCtx,
    config: &AccountConfig,
    api: &GmailApi,
    provider_thread_id: &str,
) -> Result<Vec<i64>> {
    let mut thread = api.thread(provider_thread_id, true).await?;
    let resources = thread
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .map(std::mem::take)
        .unwrap_or_default();
    if resources.len() > MAX_MESSAGES_PER_THREAD {
        return Err(CoreError::Network(format!(
            "Gmail thread returned more than {MAX_MESSAGES_PER_THREAD} messages"
        )));
    }
    let outcome = store_resources(ctx, config, &resources, None, true, false, None).await?;
    for resource in resources {
        let provider_message_id = required_string(&resource, "id")?.to_owned();
        let body = hydrate_parts(api, &resource).await?;
        store_hydrated_body(ctx, config, provider_message_id, body).await?;
    }
    if !outcome.touched_threads.is_empty() {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: outcome.touched_threads.clone(),
        });
    }
    Ok(outcome.touched_threads)
}

async fn sync_once(
    ctx: &SyncCtx,
    config: &AccountConfig,
    api: &GmailApi,
    refresh_labels: bool,
    mail_history: MailHistory,
) -> Result<bool> {
    let account_id = config.id;
    let mut state = ctx
        .db
        .read(move |conn| repo::gmail::state(conn, account_id))
        .await?;

    if refresh_labels {
        sync_labels(ctx, config, api).await?;
    }

    if !state.backfill_done {
        if state.history_id.is_none() || state.generation == 0 {
            let history_id = api.profile_history_id().await?;
            let generation = ctx
                .db
                .write(move |conn| repo::gmail::begin_full_sync(conn, account_id, &history_id))
                .await?;
            state = ctx
                .db
                .read(move |conn| repo::gmail::state(conn, account_id))
                .await?;
            debug_assert_eq!(state.generation, generation);
        }
        let cutoff_date = state
            .full_started_at
            .and_then(|started_at| mail_history.cutoff_date_at(started_at));
        let (resources, next_cursor) = api
            .list_messages_page(state.sync_cursor.as_deref(), cutoff_date)
            .await?;
        if next_cursor.is_some() && next_cursor == state.sync_cursor {
            return Err(CoreError::Network(
                "Gmail full-sync pagination cursor did not advance".into(),
            ));
        }
        let total = resources.len() as u64;
        let outcome = store_resources(
            ctx,
            config,
            &resources,
            Some(state.generation),
            false,
            true,
            state.full_started_at,
        )
        .await?;
        if !outcome.touched_threads.is_empty() {
            ctx.bus.emit(CoreEvent::MailUpdated {
                thread_ids: outcome.touched_threads,
            });
        }
        ctx.bus.emit(CoreEvent::SyncProgress(SyncProgress {
            account_id,
            folder: "Gmail".into(),
            phase: if next_cursor.is_some() {
                "headers".into()
            } else {
                "history".into()
            },
            done: total,
            total,
        }));
        if next_cursor.is_none()
            && let Some(started_at) = state.full_started_at
        {
            let cutoff = mail_history.cutoff_ms_at(started_at);
            let changed =
                cleanup_full_generation(ctx, account_id, state.generation, started_at, cutoff)
                    .await?;
            if !changed.is_empty() {
                ctx.bus.emit(CoreEvent::MailUpdated {
                    thread_ids: changed,
                });
            }
        }
        ctx.db
            .write({
                let next_cursor = next_cursor.clone();
                move |conn| {
                    repo::gmail::checkpoint_full_page(conn, account_id, next_cursor.as_deref())
                }
            })
            .await?;
        if next_cursor.is_none() {
            sync_draft_map(ctx, config, api).await?;
            emit_sync_status(ctx, account_id).await;
            return Ok(true);
        }
        emit_sync_status(ctx, account_id).await;
        return Ok(false);
    }

    let Some(history_id) = state.history_id.as_deref() else {
        ctx.db
            .write(move |conn| repo::gmail::expire_history(conn, account_id))
            .await?;
        return Ok(false);
    };
    let page = match api
        .history(history_id, state.history_page_token.as_deref())
        .await
    {
        Ok(page) => page,
        Err(CoreError::NotFound(_)) => {
            tracing::info!(
                account_id,
                "Gmail history cursor expired; starting authoritative rescan"
            );
            ctx.db
                .write(move |conn| repo::gmail::expire_history(conn, account_id))
                .await?;
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    if page.next_page_token.is_some() && page.next_page_token == state.history_page_token {
        return Err(CoreError::Network(
            "Gmail history pagination cursor did not advance".into(),
        ));
    }

    let affected_ids = page.affected_threads.iter().cloned().collect::<Vec<_>>();
    let mut touched = Vec::new();
    let mut fresh_threads = Vec::new();
    let mut removed = Vec::new();
    for batch in affected_ids.chunks(HISTORY_THREAD_BATCH) {
        let api_for_fetch = api.clone();
        let results = stream::iter(batch.iter().cloned().map(|thread_id| {
            let api = api_for_fetch.clone();
            async move {
                let result = api.thread(&thread_id, false).await;
                (thread_id, result)
            }
        }))
        .buffer_unordered(HISTORY_FETCH_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
        let mut resources = Vec::new();
        for (thread_id, result) in results {
            match result {
                Ok(mut thread) => {
                    let messages = thread
                        .get_mut("messages")
                        .and_then(Value::as_array_mut)
                        .map(std::mem::take)
                        .unwrap_or_default();
                    if messages.len() > MAX_MESSAGES_PER_THREAD
                        || messages.len()
                            > MAX_HISTORY_MESSAGES_PER_BATCH.saturating_sub(resources.len())
                    {
                        return Err(CoreError::Network(
                            "Gmail history batch exceeded its message safety limit".into(),
                        ));
                    }
                    resources.extend(messages);
                }
                Err(CoreError::NotFound(_)) => removed.push(thread_id),
                Err(error) => return Err(error),
            }
        }
        let resources = retain_history_threads(ctx, account_id, resources, mail_history).await?;
        let outcome = store_resources(ctx, config, &resources, None, true, true, None).await?;
        touched.extend(outcome.touched_threads);
        fresh_threads.extend(outcome.fresh_threads);
    }
    for thread_id in removed {
        touched.extend(remove_provider_thread(ctx, account_id, thread_id).await?);
    }
    touched.sort_unstable();
    touched.dedup();
    if !touched.is_empty() {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: touched,
        });
    }
    fresh_threads.sort_unstable();
    fresh_threads.dedup();
    if !fresh_threads.is_empty() {
        ctx.bus.emit(CoreEvent::MailNew {
            account_id,
            thread_ids: fresh_threads,
        });
    }
    ctx.db
        .write({
            let next = page.next_page_token.clone();
            let completed = page.completed_history_id.clone();
            move |conn| {
                repo::gmail::checkpoint_history_page(
                    conn,
                    account_id,
                    next.as_deref(),
                    completed.as_deref(),
                )
            }
        })
        .await?;
    if page.next_page_token.is_none() && !page.affected_threads.is_empty() {
        sync_draft_map(ctx, config, api).await?;
    }
    Ok(true)
}

struct PreparedDraft {
    raw: Vec<u8>,
    message_id: String,
    provider_thread_id: Option<String>,
    provider_draft_id: Option<String>,
}

async fn prepare_draft(
    ctx: &SyncCtx,
    config: &AccountConfig,
    api: &GmailApi,
    draft_id: i64,
) -> Result<PreparedDraft> {
    let (
        detail,
        bcc,
        refs,
        in_reply_to,
        provider_thread_id,
        provider_draft_id,
        gm_msgid,
        stored_message_id,
    ) = ctx
        .db
        .read(move |conn| {
            let detail = repo::messages::detail(conn, draft_id)?;
            if !detail.is_draft {
                return Err(CoreError::NotFound(format!("draft {draft_id}")));
            }
            let bcc_json: String = conn.query_row(
                "SELECT bcc_json FROM messages WHERE id = ?1",
                params![draft_id],
                |row| row.get(0),
            )?;
            let bcc = serde_json::from_str::<Vec<Address>>(&bcc_json)?;
            let parent_id: Option<i64> = conn
                .query_row(
                    "SELECT in_reply_to_message_id FROM drafts_meta WHERE message_id = ?1",
                    params![draft_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            let mut refs = Vec::new();
            let mut in_reply_to = None;
            if let Some(parent_id) = parent_id {
                let mut stmt =
                    conn.prepare("SELECT ref_message_id FROM message_refs WHERE message_id = ?1")?;
                refs = stmt
                    .query_map(params![parent_id], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if let Some(parent) = repo::messages::get_row(conn, parent_id)?
                    && let Some(message_id) = parent.message_id
                {
                    refs.push(message_id.clone());
                    in_reply_to = Some(message_id);
                }
            }
            let provider: (
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            ) = conn.query_row(
                "SELECT t.gm_thrid, m.gmail_draft_id, m.gm_msgid, m.message_id
                 FROM messages m LEFT JOIN threads t ON t.id = m.thread_id
                 WHERE m.id = ?1",
                params![draft_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            Ok((
                detail,
                bcc,
                refs,
                in_reply_to,
                provider.0,
                provider.1,
                provider.2,
                provider.3,
            ))
        })
        .await?;

    let staged: Vec<(String, String, Option<String>)> = ctx
        .db
        .read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT file_path, filename, mime_type
                 FROM draft_attachments WHERE draft_id = ?1 ORDER BY id",
            )?;
            Ok(stmt
                .query_map(params![draft_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await?;
    let staging_root = tokio::fs::canonicalize(ctx.paths.draft_attachments_dir())
        .await
        .ok();
    let mut attachments = Vec::new();
    let mut attachment_bytes = 0usize;
    for (path, filename, mime_type) in staged {
        let canonical = tokio::fs::canonicalize(&path)
            .await
            .map_err(|error| CoreError::Other(format!("attachment {filename}: {error}")))?;
        if !staging_root
            .as_ref()
            .is_some_and(|root| canonical.starts_with(root))
        {
            return Err(CoreError::Other(format!(
                "attachment {filename}: path is outside the staging area"
            )));
        }
        let remaining = crate::MAX_DRAFT_ATTACHMENT_BYTES.saturating_sub(attachment_bytes);
        let bytes = crate::file_io::read(canonical, remaining, "draft attachment").await?;
        attachment_bytes += bytes.len();
        attachments.push(crate::mime::OutgoingAttachment {
            filename: filename.clone(),
            mime_type: mime_type.unwrap_or_else(|| mime_guess(&filename)),
            bytes,
        });
    }

    // A draft created in another Gmail client has descriptors rather than
    // staged composer files. Preserve those attachments when it is edited or
    // sent from Flectar Mail.
    if attachments.is_empty()
        && let Some(gm_msgid) = gm_msgid.as_deref()
    {
        let remote: Vec<RemoteDraftAttachment> = ctx
            .db
            .read(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT provider_attachment_id, file_path,
                                COALESCE(filename, 'attachment'), mime_type
                         FROM attachments WHERE message_id = ?1 AND is_inline = 0",
                )?;
                Ok(stmt
                    .query_map(params![draft_id], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await?;
        for (provider_id, file_path, filename, mime_type) in remote {
            let remaining = crate::MAX_DRAFT_ATTACHMENT_BYTES.saturating_sub(attachment_bytes);
            let bytes = if let Some(path) = file_path {
                crate::file_io::read(path, remaining, "draft attachment").await?
            } else if let Some(provider_id) = provider_id {
                let bytes = api.attachment(gm_msgid, &provider_id).await?;
                if bytes.len() > remaining {
                    return Err(CoreError::Other(format!(
                        "draft attachments exceed the {} MiB safety limit",
                        crate::MAX_DRAFT_ATTACHMENT_BYTES / (1024 * 1024)
                    )));
                }
                bytes
            } else {
                continue;
            };
            attachment_bytes += bytes.len();
            attachments.push(crate::mime::OutgoingAttachment {
                filename: filename.clone(),
                mime_type: mime_type.unwrap_or_else(|| mime_guess(&filename)),
                bytes,
            });
        }
    }

    let from = Address {
        name: config.display_name.clone(),
        email: config.email.clone(),
    };
    let domain = config.email.split('@').nth(1).unwrap_or("localhost");
    let outgoing = crate::mime::OutgoingMessage {
        from,
        to: &detail.to,
        cc: &detail.cc,
        bcc: &bcc,
        subject: &detail.subject,
        body_text: detail.text_body.as_deref().unwrap_or(""),
        body_html: detail.html_body.as_deref(),
        in_reply_to: in_reply_to.as_deref(),
        references: &refs,
        message_id: stored_message_id.as_deref(),
        message_id_domain: domain,
        attachments,
    };
    let (message_id, raw) = crate::mime::build_message(&outgoing)?;
    let raw = crate::mail_security::protect_draft(
        &ctx.db,
        config.id,
        draft_id,
        raw,
        outgoing
            .to
            .iter()
            .chain(outgoing.cc)
            .chain(outgoing.bcc)
            .cloned()
            .collect(),
    )
    .await?;
    let bare_message_id = message_id.trim_matches(['<', '>']).to_owned();
    if stored_message_id.as_deref() != Some(bare_message_id.as_str()) {
        let stable_id = bare_message_id.clone();
        ctx.db
            .write(move |conn| {
                conn.execute(
                    "UPDATE messages SET message_id = ?2 WHERE id = ?1",
                    params![draft_id, stable_id],
                )?;
                Ok(())
            })
            .await?;
    }
    Ok(PreparedDraft {
        raw,
        message_id,
        provider_thread_id,
        provider_draft_id,
    })
}

fn mime_guess(filename: &str) -> String {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" | "md" | "log" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        "ics" => "text/calendar",
        _ => "application/octet-stream",
    }
    .to_owned()
}

async fn save_remote_draft(
    ctx: &SyncCtx,
    config: &AccountConfig,
    api: &GmailApi,
    draft_id: i64,
) -> Result<()> {
    let prepared = prepare_draft(ctx, config, api, draft_id).await?;
    let value = match api
        .upsert_draft(
            prepared.provider_draft_id.as_deref(),
            &prepared.raw,
            prepared.provider_thread_id.as_deref(),
        )
        .await
    {
        Ok(value) => value,
        // A draft deleted in another client no longer has a resource to update;
        // the local edit is authoritative user intent, so recreate it.
        Err(CoreError::NotFound(_)) if prepared.provider_draft_id.is_some() => {
            api.upsert_draft(None, &prepared.raw, prepared.provider_thread_id.as_deref())
                .await?
        }
        Err(error) => return Err(error),
    };
    let provider_draft_id = required_string(&value, "id")?.to_owned();
    let provider_message_id = value
        .pointer("/message/id")
        .and_then(Value::as_str)
        .ok_or_else(|| CoreError::Other("Gmail draft omitted message id".into()))?
        .to_owned();
    let provider_thread_id = value
        .pointer("/message/threadId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(prepared.provider_thread_id);
    let cleanup_draft_id = provider_draft_id.clone();
    let persisted = persist_remote_draft_identity(
        ctx,
        draft_id,
        provider_message_id,
        provider_thread_id,
        provider_draft_id,
        prepared.message_id,
    )
    .await?;
    if !persisted {
        // The user deleted the local draft while the provider request was in
        // flight. Compensate immediately so the completed autosave cannot
        // resurrect an orphan draft in Gmail.
        api.delete_draft(&cleanup_draft_id).await?;
    }
    Ok(())
}

async fn persist_remote_draft_identity(
    ctx: &SyncCtx,
    draft_id: i64,
    provider_message_id: String,
    provider_thread_id: Option<String>,
    provider_draft_id: String,
    message_id: String,
) -> Result<bool> {
    let (thread_id, persisted) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let updated = tx.execute(
                "UPDATE messages SET gm_msgid = ?2, gm_thrid = ?3,
                        gmail_draft_id = ?4, message_id = ?5
                 WHERE id = ?1",
                params![
                    draft_id,
                    provider_message_id,
                    provider_thread_id,
                    provider_draft_id,
                    message_id.trim_matches(['<', '>']),
                ],
            )?;
            if updated == 0 {
                tx.commit()?;
                return Ok((None, false));
            }
            if let Some(provider_thread_id) = provider_thread_id.as_deref()
                && let Some(local_thread_id) =
                    repo::messages::get_row(&tx, draft_id)?.and_then(|row| row.thread_id)
            {
                tx.execute(
                    "UPDATE threads SET gm_thrid = COALESCE(gm_thrid, ?2)
                         WHERE id = ?1",
                    params![local_thread_id, provider_thread_id],
                )?;
            }
            let thread_id = repo::messages::get_row(&tx, draft_id)?.and_then(|row| row.thread_id);
            tx.commit()?;
            Ok((thread_id, true))
        })
        .await?;
    if let Some(thread_id) = thread_id {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: vec![thread_id],
        });
    }
    Ok(persisted)
}

async fn send_remote_draft(
    ctx: &SyncCtx,
    config: &AccountConfig,
    api: &GmailApi,
    draft_id: i64,
) -> Result<()> {
    let prepared = prepare_draft(ctx, config, api, draft_id).await?;
    if let Some(sent) = api.find_sent_by_message_id(&prepared.message_id).await? {
        return finalize_sent_draft(ctx, config, draft_id, sent, &prepared.message_id).await;
    }

    // Always update/create the Gmail draft immediately before sending so a
    // delayed-send action dispatches exactly the local snapshot visible to the
    // user, even if an earlier background draft-save was interrupted.
    let draft = match api
        .upsert_draft(
            prepared.provider_draft_id.as_deref(),
            &prepared.raw,
            prepared.provider_thread_id.as_deref(),
        )
        .await
    {
        Ok(draft) => draft,
        Err(CoreError::NotFound(_)) if prepared.provider_draft_id.is_some() => {
            // drafts.send removes the draft resource. A missing id on retry is
            // therefore ambiguous: reconcile by Message-ID and wait for Gmail's
            // search index rather than creating a second outgoing message.
            if let Some(sent) = api.find_sent_by_message_id(&prepared.message_id).await? {
                return finalize_sent_draft(ctx, config, draft_id, sent, &prepared.message_id)
                    .await;
            }
            return Err(CoreError::Network(
                "Gmail draft disappeared; waiting to reconcile the send".into(),
            ));
        }
        Err(error) => return Err(error),
    };
    let provider_draft_id = required_string(&draft, "id")?.to_owned();
    let provider_message_id = draft
        .pointer("/message/id")
        .and_then(Value::as_str)
        .ok_or_else(|| CoreError::Other("Gmail draft omitted message id".into()))?
        .to_owned();
    let provider_thread_id = draft
        .pointer("/message/threadId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(prepared.provider_thread_id.clone());
    let persisted = persist_remote_draft_identity(
        ctx,
        draft_id,
        provider_message_id,
        provider_thread_id,
        provider_draft_id.clone(),
        prepared.message_id.clone(),
    )
    .await?;
    if !persisted {
        api.delete_draft(&provider_draft_id).await?;
        return Err(CoreError::NotFound("local Gmail draft was deleted".into()));
    }

    let sent = match api.send_draft(&provider_draft_id).await {
        Ok(sent) => sent,
        Err(error) => {
            let ambiguous = matches!(
                &error,
                CoreError::Offline | CoreError::Network(_) | CoreError::NotFound(_)
            );
            if ambiguous {
                if let Some(sent) = api.find_sent_by_message_id(&prepared.message_id).await? {
                    sent
                } else {
                    return Err(error);
                }
            } else {
                return Err(error);
            }
        }
    };
    finalize_sent_draft(ctx, config, draft_id, sent, &prepared.message_id).await
}

async fn finalize_sent_draft(
    ctx: &SyncCtx,
    config: &AccountConfig,
    draft_id: i64,
    sent: Value,
    message_id: &str,
) -> Result<()> {
    let provider_message_id = required_string(&sent, "id")?.to_owned();
    let provider_thread_id = sent
        .get("threadId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let account_id = config.id;
    let message_id = message_id.trim_matches(['<', '>']).to_owned();
    let (thread_id, staged_paths) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let sent_folder = match repo::folders::by_role(&tx, account_id, roles::SENT)? {
                Some(folder) => folder.id,
                None => {
                    repo::folders::upsert(&tx, account_id, "SENT", Some("/"), Some(roles::SENT))?
                }
            };
            let all_folder = match repo::folders::by_role(&tx, account_id, roles::ALL)? {
                Some(folder) => folder.id,
                None => repo::folders::upsert(
                    &tx,
                    account_id,
                    "[Gmail]/All Mail",
                    Some("/"),
                    Some(roles::ALL),
                )?,
            };
            tx.execute(
                "UPDATE messages SET
                     is_draft = 0, is_outgoing = 1, is_read = 1,
                     folder_id = ?2, uid = NULL, gm_msgid = ?3,
                     gm_thrid = COALESCE(?4, gm_thrid),
                     gmail_draft_id = NULL, message_id = ?5, date = ?6
                 WHERE id = ?1",
                params![
                    draft_id,
                    sent_folder,
                    provider_message_id,
                    provider_thread_id,
                    message_id,
                    now_ms(),
                ],
            )?;
            repo::gmail::set_message_folders(&tx, draft_id, &[sent_folder, all_folder])?;
            tx.execute(
                "DELETE FROM drafts_meta WHERE message_id = ?1",
                params![draft_id],
            )?;
            let staged_paths = repo::messages::take_draft_attachment_paths(&tx, draft_id)?;
            let thread_id = repo::messages::get_row(&tx, draft_id)?.and_then(|row| row.thread_id);
            if let Some(thread_id) = thread_id {
                if let Some(provider_thread_id) = provider_thread_id.as_deref() {
                    tx.execute(
                        "UPDATE threads SET gm_thrid = COALESCE(gm_thrid, ?2)
                         WHERE id = ?1",
                        params![thread_id, provider_thread_id],
                    )?;
                }
                repo::threads::recompute(&tx, thread_id)?;
            }
            repo::search::index_message(&tx, draft_id)?;
            tx.commit()?;
            Ok((thread_id, staged_paths))
        })
        .await?;
    for path in staged_paths {
        crate::remove_staged_attachment(&ctx.paths.draft_attachments_dir(), &path).await;
    }
    if let Some(thread_id) = thread_id {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: vec![thread_id],
        });
    }
    Ok(())
}

async fn ensure_provider_label(
    ctx: &SyncCtx,
    config: &AccountConfig,
    api: &GmailApi,
    local_label_id: i64,
) -> Result<String> {
    let account_id = config.id;
    if let Some(provider_id) = ctx
        .db
        .read(move |conn| repo::gmail::provider_label_for_local(conn, account_id, local_label_id))
        .await?
    {
        return Ok(provider_id);
    }
    let label = ctx
        .db
        .read(move |conn| repo::labels::get(conn, local_label_id))
        .await?
        .ok_or_else(|| CoreError::NotFound(format!("label {local_label_id}")))?;
    if label.is_auto {
        return Err(CoreError::Other(
            "local auto-categories cannot be pushed to Gmail".into(),
        ));
    }
    let remote = api.create_label(&label.name, Some(&label.color)).await?;
    let provider_id = remote.id.clone();
    ctx.db
        .write(move |conn| {
            let folder_id = repo::folders::upsert(conn, account_id, &remote.name, Some("/"), None)?;
            conn.execute(
                "INSERT INTO gmail_labels (
                     account_id, provider_id, name, kind, folder_id,
                     local_label_id, background_color, text_color
                 ) VALUES (?1, ?2, ?3, 'user', ?4, ?5, ?6, ?7)
                 ON CONFLICT(account_id, provider_id) DO UPDATE SET
                     name = excluded.name, folder_id = excluded.folder_id,
                     local_label_id = excluded.local_label_id,
                     background_color = excluded.background_color,
                     text_color = excluded.text_color",
                params![
                    account_id,
                    remote.id,
                    remote.name,
                    folder_id,
                    local_label_id,
                    remote.background_color,
                    remote.text_color,
                ],
            )?;
            Ok(())
        })
        .await?;
    Ok(provider_id)
}

async fn apply_action(
    ctx: &SyncCtx,
    config: &AccountConfig,
    api: &GmailApi,
    action: &repo::actions::PendingAction,
) -> Result<()> {
    match action.kind.as_str() {
        "snooze" | "unsnooze" => return Ok(()),
        "save_draft" => {
            let draft_id = action.payload["draftId"]
                .as_i64()
                .or(action.message_id)
                .ok_or_else(|| CoreError::Other("draft save omitted draftId".into()))?;
            return save_remote_draft(ctx, config, api, draft_id).await;
        }
        "delete_draft" => {
            if let Some(draft_id) = action.payload["gmailDraftId"].as_str() {
                return api.delete_draft(draft_id).await;
            }
            if let Some(message_id) = action.payload["gmailMessageId"].as_str() {
                return api.trash_message(message_id).await;
            }
            return Ok(());
        }
        "send" => {
            let draft_id = action.payload["draftId"]
                .as_i64()
                .ok_or_else(|| CoreError::Other("send omitted draftId".into()))?;
            return send_remote_draft(ctx, config, api, draft_id).await;
        }
        "gmail_label_update" => {
            let provider_id = action.payload["providerId"]
                .as_str()
                .ok_or_else(|| CoreError::Other("label update omitted providerId".into()))?;
            let name = action.payload["name"]
                .as_str()
                .ok_or_else(|| CoreError::Other("label update omitted name".into()))?;
            let color = action.payload["color"].as_str();
            api.update_label(provider_id, name, color).await?;
            let account_id = config.id;
            let provider_id = provider_id.to_owned();
            let name = name.to_owned();
            ctx.db
                .write(move |conn| {
                    conn.execute(
                        "UPDATE gmail_labels SET name = ?3
                         WHERE account_id = ?1 AND provider_id = ?2",
                        params![account_id, provider_id, name],
                    )?;
                    conn.execute(
                        "UPDATE folders SET imap_name = ?3
                         WHERE account_id = ?1 AND id = (
                           SELECT folder_id FROM gmail_labels
                           WHERE account_id = ?1 AND provider_id = ?2
                         )",
                        params![account_id, provider_id, name],
                    )?;
                    Ok(())
                })
                .await?;
            return Ok(());
        }
        "gmail_label_delete" => {
            let provider_id = action.payload["providerId"]
                .as_str()
                .ok_or_else(|| CoreError::Other("label delete omitted providerId".into()))?;
            api.delete_label(provider_id).await?;
            let account_id = config.id;
            let provider_id = provider_id.to_owned();
            ctx.db
                .write(move |conn| {
                    let tx = conn.transaction()?;
                    let folder_id: Option<i64> = tx
                        .query_row(
                            "SELECT folder_id FROM gmail_labels
                             WHERE account_id = ?1 AND provider_id = ?2",
                            params![account_id, provider_id],
                            |row| row.get(0),
                        )
                        .optional()?
                        .flatten();
                    if let Some(folder_id) = folder_id {
                        let fallback = repo::folders::by_role(&tx, account_id, roles::ALL)?
                            .or(repo::folders::by_role(&tx, account_id, roles::ARCHIVE)?)
                            .map(|folder| folder.id);
                        if let Some(fallback) = fallback {
                            tx.execute(
                                "UPDATE messages SET folder_id = ?2 WHERE folder_id = ?1",
                                params![folder_id, fallback],
                            )?;
                        }
                    }
                    tx.execute(
                        "DELETE FROM gmail_labels WHERE account_id = ?1 AND provider_id = ?2",
                        params![account_id, provider_id],
                    )?;
                    if let Some(folder_id) = folder_id {
                        tx.execute("DELETE FROM folders WHERE id = ?1", params![folder_id])?;
                    }
                    tx.commit()?;
                    Ok(())
                })
                .await?;
            return Ok(());
        }
        _ => {}
    }

    let Some(local_message_id) = action.message_id else {
        return Ok(());
    };
    let account_id = config.id;
    let provider_target: Option<(Option<String>, Option<String>, bool)> = ctx
        .db
        .read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT m.gm_msgid, t.gm_thrid, m.is_draft
                     FROM messages m LEFT JOIN threads t ON t.id = m.thread_id
                     WHERE m.id = ?1 AND m.account_id = ?2",
                    params![local_message_id, account_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, i64>(2)? != 0,
                        ))
                    },
                )
                .optional()?)
        })
        .await?;
    let Some((Some(provider_message_id), provider_thread_id, is_draft)) = provider_target else {
        // A local draft-save action earlier in the ordered queue will assign
        // its id. Other local-only messages have no remote intent to replay.
        return Ok(());
    };

    let mut add = Vec::<String>::new();
    let mut remove = Vec::<String>::new();
    match action.kind.as_str() {
        "mark_read" => remove.push("UNREAD".into()),
        "mark_unread" => add.push("UNREAD".into()),
        "star" => add.push("STARRED".into()),
        "unstar" => remove.push("STARRED".into()),
        "archive" => remove.push("INBOX".into()),
        "unarchive" => add.push("INBOX".into()),
        "trash" => {
            return match provider_thread_id.as_deref().filter(|_| !is_draft) {
                Some(thread_id) => api.trash_thread(thread_id).await,
                None => api.trash_message(&provider_message_id).await,
            };
        }
        "spam" => {
            add.push("SPAM".into());
            remove.extend(["INBOX".into(), "TRASH".into()]);
        }
        "not_spam" => {
            add.push("INBOX".into());
            remove.push("SPAM".into());
        }
        "add_label" | "remove_label" => {
            let local_label_id = action.payload["labelId"]
                .as_i64()
                .ok_or_else(|| CoreError::Other("label action omitted labelId".into()))?;
            let provider_id = ensure_provider_label(ctx, config, api, local_label_id).await?;
            if action.kind == "add_label" {
                add.push(provider_id);
            } else {
                remove.push(provider_id);
            }
        }
        "move" => {
            let target_folder = action.payload["targetFolderId"]
                .as_i64()
                .ok_or_else(|| CoreError::Other("move omitted targetFolderId".into()))?;
            let source_folder = action.payload["srcFolderId"].as_i64();
            let (target_provider, target_role, source_provider): (
                Option<String>,
                Option<String>,
                Option<String>,
            ) = ctx
                .db
                .read(move |conn| {
                    let target_role: Option<String> = conn
                        .query_row(
                            "SELECT role FROM folders WHERE id = ?1",
                            params![target_folder],
                            |row| row.get(0),
                        )
                        .optional()?
                        .flatten();
                    let target_provider = conn
                        .query_row(
                            "SELECT provider_id FROM gmail_labels
                             WHERE account_id = ?1 AND folder_id = ?2",
                            params![account_id, target_folder],
                            |row| row.get(0),
                        )
                        .optional()?;
                    let source_provider = source_folder.and_then(|folder_id| {
                        conn.query_row(
                            "SELECT provider_id FROM gmail_labels
                                 WHERE account_id = ?1 AND folder_id = ?2",
                            params![account_id, folder_id],
                            |row| row.get(0),
                        )
                        .optional()
                        .ok()
                        .flatten()
                    });
                    Ok((target_provider, target_role, source_provider))
                })
                .await?;
            match target_role.as_deref() {
                Some(roles::ARCHIVE) => remove.push("INBOX".into()),
                Some(roles::INBOX) => add.push("INBOX".into()),
                Some(roles::TRASH) => {
                    return match provider_thread_id.as_deref().filter(|_| !is_draft) {
                        Some(thread_id) => api.trash_thread(thread_id).await,
                        None => api.trash_message(&provider_message_id).await,
                    };
                }
                Some(roles::SPAM) => {
                    add.push("SPAM".into());
                    remove.push("INBOX".into());
                }
                _ => {
                    if let Some(provider_id) = target_provider {
                        add.push(provider_id);
                    }
                    if let Some(provider_id) = source_provider {
                        remove.push(provider_id);
                    }
                }
            }
        }
        other => tracing::warn!(kind = other, "unknown Gmail action"),
    }
    if add.is_empty() && remove.is_empty() {
        return Ok(());
    }
    if !is_draft
        && !matches!(action.kind.as_str(), "star" | "unstar")
        && let Some(provider_thread_id) = provider_thread_id.as_deref()
    {
        api.modify_thread(provider_thread_id, &add, &remove).await
    } else {
        api.modify_message(&provider_message_id, &add, &remove)
            .await
    }
}

async fn execute_actions(ctx: &SyncCtx, config: &AccountConfig, api: &GmailApi) -> Result<bool> {
    const LIMIT: i64 = 20;
    const MAX_ATTEMPTS: i64 = 8;
    let account_id = config.id;
    let actions = ctx
        .db
        .read(move |conn| repo::actions::due(conn, account_id, now_ms(), LIMIT))
        .await?;
    for action in actions {
        let action_id = action.id;
        if action.kind == "save_draft" {
            let draft_id = action.payload["draftId"].as_i64().or(action.message_id);
            let has_newer = ctx
                .db
                .read(move |conn| {
                    Ok(draft_id.is_some_and(|draft_id| {
                        conn.query_row(
                            "SELECT EXISTS(
                                 SELECT 1 FROM pending_actions
                                 WHERE account_id = ?1 AND kind = 'save_draft'
                                   AND state = 'pending' AND id > ?2
                                   AND json_extract(payload, '$.draftId') = ?3
                             )",
                            params![account_id, action_id, draft_id],
                            |row| row.get::<_, bool>(0),
                        )
                        .unwrap_or(false)
                    }))
                })
                .await?;
            if has_newer {
                ctx.db
                    .write(move |conn| repo::actions::set_state(conn, action_id, "done", None))
                    .await?;
                continue;
            }
        }
        let claimed = ctx
            .db
            .write(move |conn| repo::actions::try_claim(conn, action_id))
            .await?;
        if !claimed {
            continue;
        }
        match apply_action(ctx, config, api, &action).await {
            Ok(()) => {
                ctx.db
                    .write(move |conn| repo::actions::set_state(conn, action_id, "done", None))
                    .await?;
                ctx.bus.emit(CoreEvent::ActionState {
                    action_id,
                    state: "done".into(),
                    error: None,
                });
            }
            Err(error @ (CoreError::NeedsReauth | CoreError::Auth(_))) => {
                let message = error.to_string();
                ctx.db
                    .write(move |conn| {
                        repo::actions::bump_attempt(conn, action_id, now_ms() + 60_000, &message)
                    })
                    .await?;
                if action.kind == "send" {
                    ctx.bus.emit(CoreEvent::ActionState {
                        action_id,
                        state: "paused".into(),
                        error: Some(error.to_string()),
                    });
                }
                return Err(CoreError::NeedsReauth);
            }
            Err(error) => {
                let message = error.to_string();
                let attempts = action.attempts + 1;
                let permanent = matches!(error, CoreError::NotFound(_))
                    || message.contains("400 Bad Request")
                    || message.contains("invalidArgument");
                if attempts >= MAX_ATTEMPTS || permanent {
                    let saved = message.clone();
                    ctx.db
                        .write(move |conn| {
                            repo::actions::set_state(conn, action_id, "failed", Some(&saved))
                        })
                        .await?;
                    ctx.bus.emit(CoreEvent::ActionState {
                        action_id,
                        state: "failed".into(),
                        error: Some(message),
                    });
                } else {
                    let retry_at =
                        now_ms() + (1_i64 << attempts.min(8)) * 1_000 + action_id.rem_euclid(997);
                    let saved = message.clone();
                    ctx.db
                        .write(move |conn| {
                            repo::actions::bump_attempt(conn, action_id, retry_at, &saved)
                        })
                        .await?;
                    if matches!(error, CoreError::Offline | CoreError::Network(_)) {
                        return Err(error);
                    }
                }
            }
        }
    }
    ctx.db
        .read(move |conn| repo::actions::has_due(conn, account_id, now_ms()))
        .await
}

pub(super) fn spawn(
    ctx: SyncCtx,
    config: AccountConfig,
    rx: mpsc::Receiver<SyncCmd>,
    body_rx: mpsc::Receiver<PriorityFetchCmd>,
    settings_rx: watch::Receiver<AccountSettings>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let actor_ctx = ctx.clone();
    let actor_config = config.clone();
    vec![
        tokio::spawn(async move { run_actor(actor_ctx, actor_config, rx, settings_rx).await }),
        tokio::spawn(async move { run_reader(ctx, config, body_rx).await }),
    ]
}

async fn run_actor(
    ctx: SyncCtx,
    config: AccountConfig,
    mut rx: mpsc::Receiver<SyncCmd>,
    mut settings_rx: watch::Receiver<AccountSettings>,
) {
    let account_id = config.id;
    debug_assert_eq!(config.provider, Provider::Gmail);
    debug_assert_eq!(config.auth_kind, AuthKind::Oauth2);
    let api = match GmailApi::new(ctx.clone(), config.clone()) {
        Ok(api) => api,
        Err(error) => {
            tracing::error!(account_id, %error, "could not create Gmail provider");
            set_state_error(&ctx, account_id, "offline", &error.to_string()).await;
            return;
        }
    };
    let mut waiters = Vec::<oneshot::Sender<std::result::Result<(), String>>>::new();
    let mut refresh_labels = true;
    let mut backoff = 1u64;

    loop {
        while let Ok(command) = rx.try_recv() {
            match command {
                SyncCmd::SyncNow { complete } => {
                    refresh_labels = true;
                    if let Some(complete) = complete {
                        waiters.push(complete);
                    }
                }
                SyncCmd::Shutdown => return,
                SyncCmd::RunActions | SyncCmd::FetchBody { .. } => {}
            }
        }

        set_state(&ctx, account_id, "syncing").await;
        let mail_history = settings_rx.borrow_and_update().mail_history;
        let result = async {
            // Provider definitions must exist before replaying actions. This is
            // important on a new account where the user can compose before the
            // first metadata page creates Sent, Drafts and All Mail locally.
            if refresh_labels {
                sync_labels(&ctx, &config, &api).await?;
            }
            let actions_remaining = execute_actions(&ctx, &config, &api).await?;
            let backfill_done = sync_once(&ctx, &config, &api, false, mail_history).await?;
            Ok::<_, CoreError>((actions_remaining, backfill_done))
        }
        .await;
        refresh_labels = false;

        let (delay, rerun) = match result {
            Ok((actions_remaining, backfill_done)) => {
                backoff = 1;
                set_state(&ctx, account_id, "idle").await;
                ctx.bus.emit(CoreEvent::NetworkState { online: true });
                let (done, total) = ctx
                    .db
                    .read(move |conn| {
                        repo::messages::body_progress_since(
                            conn,
                            account_id,
                            mail_history.cutoff_ms_at(now_ms()),
                        )
                    })
                    .await
                    .unwrap_or((0, 0));
                ctx.bus.emit(CoreEvent::SyncProgress(SyncProgress {
                    account_id,
                    folder: "Gmail".into(),
                    phase: "idle".into(),
                    done,
                    total,
                }));
                for waiter in waiters.drain(..) {
                    let _ = waiter.send(Ok(()));
                }
                let delay = if backfill_done {
                    configured_sync_interval(&ctx.db).await
                } else {
                    BACKFILL_INTERVAL
                };
                (delay, actions_remaining)
            }
            Err(CoreError::NeedsReauth) | Err(CoreError::Auth(_)) => {
                set_state(&ctx, account_id, "needs_reauth").await;
                for waiter in waiters.drain(..) {
                    let _ = waiter.send(Err("authentication required".into()));
                }
                // A reauth nudge invalidates the old access-token state and
                // immediately retries. There is no pointless timed auth loop.
                match rx.recv().await {
                    None | Some(SyncCmd::Shutdown) => return,
                    Some(SyncCmd::SyncNow { complete }) => {
                        refresh_labels = true;
                        if let Some(complete) = complete {
                            waiters.push(complete);
                        }
                    }
                    Some(_) => {}
                }
                continue;
            }
            Err(error) => {
                tracing::warn!(account_id, %error, "Gmail sync cycle failed");
                ctx.bus.emit(CoreEvent::NetworkState { online: false });
                let message = error.to_string();
                set_state_error(&ctx, account_id, "offline", &message).await;
                for waiter in waiters.drain(..) {
                    let _ = waiter.send(Err(message.clone()));
                }
                let delay = Duration::from_secs(backoff);
                backoff = (backoff * 2).min(300);
                (delay, false)
            }
        };

        if rerun {
            continue;
        }
        match tokio::time::timeout(delay, rx.recv()).await {
            Ok(None) | Ok(Some(SyncCmd::Shutdown)) => return,
            Ok(Some(SyncCmd::SyncNow { complete })) => {
                refresh_labels = true;
                if let Some(complete) = complete {
                    waiters.push(complete);
                }
            }
            Ok(Some(SyncCmd::RunActions)) | Err(_) => {}
            Ok(Some(SyncCmd::FetchBody { .. })) => {}
        }
    }
}

async fn run_reader(ctx: SyncCtx, config: AccountConfig, mut rx: mpsc::Receiver<PriorityFetchCmd>) {
    let account_id = config.id;
    let api = match GmailApi::new(ctx.clone(), config.clone()) {
        Ok(api) => api,
        Err(error) => {
            tracing::error!(account_id, %error, "could not create Gmail reader");
            return;
        }
    };
    while let Some(command) = rx.recv().await {
        match command {
            PriorityFetchCmd::Attachment {
                attachment_id,
                complete,
            } => {
                let result = fetch_attachment(&ctx, &api, attachment_id)
                    .await
                    .map_err(|error| error.to_string());
                let _ = complete.send(result);
            }
            PriorityFetchCmd::Body(first_message_id) => {
                let mut message_ids = vec![first_message_id];
                tokio::time::sleep(Duration::from_millis(40)).await;
                let mut deferred = Vec::new();
                while let Ok(next) = rx.try_recv() {
                    match next {
                        PriorityFetchCmd::Body(message_id) => message_ids.push(message_id),
                        other => deferred.push(other),
                    }
                }
                // Attachment responses must never be stranded behind hydration.
                for command in deferred {
                    if let PriorityFetchCmd::Attachment {
                        attachment_id,
                        complete,
                    } = command
                    {
                        let result = fetch_attachment(&ctx, &api, attachment_id)
                            .await
                            .map_err(|error| error.to_string());
                        let _ = complete.send(result);
                    }
                }
                message_ids.sort_unstable();
                message_ids.dedup();
                let thread_ids = ctx
                    .db
                    .read(move |conn| {
                        let mut output = Vec::new();
                        for message_id in &message_ids {
                            if let Some(thread_id) = conn
                                .query_row(
                                    "SELECT gm_thrid FROM messages WHERE id = ?1",
                                    params![message_id],
                                    |row| row.get::<_, Option<String>>(0),
                                )
                                .optional()?
                                .flatten()
                            {
                                output.push(thread_id);
                            }
                        }
                        output.sort();
                        output.dedup();
                        Ok(output)
                    })
                    .await;
                match thread_ids {
                    Ok(thread_ids) => {
                        for thread_id in thread_ids {
                            if let Err(error) =
                                hydrate_thread(&ctx, &config, &api, &thread_id).await
                            {
                                tracing::warn!(account_id, %thread_id, %error, "Gmail hydration failed");
                            }
                        }
                    }
                    Err(error) => tracing::warn!(account_id, %error, "Gmail body lookup failed"),
                }
            }
        }
    }
}

async fn fetch_attachment(ctx: &SyncCtx, api: &GmailApi, attachment_id: i64) -> Result<Vec<u8>> {
    let (message_id, provider_attachment_id): (String, Option<String>) = ctx
        .db
        .read(move |conn| {
            conn.query_row(
                "SELECT m.gm_msgid, a.provider_attachment_id
                 FROM attachments a JOIN messages m ON m.id = a.message_id
                 WHERE a.id = ?1",
                params![attachment_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(Into::into)
        })
        .await?;
    let provider_attachment_id = provider_attachment_id
        .ok_or_else(|| CoreError::NotFound("Gmail attachment resource".into()))?;
    api.attachment(&message_id, &provider_attachment_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_deduplicates_threads_and_advances_only_on_final_page() {
        let first = json!({
            "historyId": "200",
            "nextPageToken": "next",
            "history": [
                { "messagesAdded": [{ "message": { "threadId": "a" } }] },
                { "labelsRemoved": [
                    { "message": { "threadId": "a" } },
                    { "message": { "threadId": "b" } }
                ] }
            ]
        });
        let page = parse_history_page(&first, "100").unwrap();
        assert_eq!(
            page.affected_threads.into_iter().collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(page.next_page_token.as_deref(), Some("next"));
        assert!(page.completed_history_id.is_none());

        let final_page = parse_history_page(&json!({ "historyId": "200" }), "100").unwrap();
        assert_eq!(final_page.completed_history_id.as_deref(), Some("200"));
    }

    #[test]
    fn metadata_batch_is_bounded_and_uses_requested_headers() {
        let body = build_batch_body(&["a/b".into(), "two".into()], "boundary");
        assert!(body.contains("messages/a%2Fb?format=metadata"));
        assert!(body.contains("metadataHeaders=List-Unsubscribe-Post"));
        assert!(body.contains("Content-ID: <flectar-1>"));
        assert!(body.ends_with("--boundary--\r\n"));
    }

    #[test]
    fn bounded_message_list_keeps_query_stable_across_pages() {
        let cutoff = chrono::NaiveDate::from_ymd_opt(2026, 2, 28).unwrap();
        let first = messages_list_url(None, Some(cutoff));
        let next = messages_list_url(Some("opaque/page"), Some(cutoff));
        let expected = cutoff
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp()
            .saturating_sub(1);
        assert!(first.contains(&format!("q=after%3A{expected}")));
        assert!(next.contains(&format!("q=after%3A{expected}")));
        assert!(next.contains("pageToken=opaque%2Fpage"));
        assert!(!messages_list_url(None, None).contains("&q="));
    }

    #[test]
    fn per_user_request_fanout_leaves_concurrency_headroom() {
        // The independent interactive reader can contribute one request while
        // the actor is synchronizing. Keep both actor paths conservative so
        // the account remains below Gmail's shared per-user limit.
        const {
            assert!(BATCH_SIZE < 11);
            assert!(HISTORY_FETCH_CONCURRENCY < 3);
        }
    }

    #[test]
    fn batch_parser_accepts_a_raced_remote_deletion() {
        let body = concat!(
            "--response\r\nContent-Type: application/http\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n",
            "{\"id\":\"one\",\"threadId\":\"thread\"}\r\n",
            "--response\r\nContent-Type: application/http\r\n\r\n",
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\r\n",
            "{\"error\":{\"message\":\"gone\"}}\r\n",
            "--response--\r\n"
        );
        let resources = parse_batch_response(body, "response", 2).unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0]["id"], "one");
    }

    #[test]
    fn batch_quota_rejection_is_retryable_not_reauth() {
        let body = concat!(
            "--response\r\nContent-Type: application/http\r\n\r\n",
            "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\n\r\n",
            "{\"error\":{\"message\":\"slow down\",\"errors\":[{\"reason\":\"userRateLimitExceeded\"}]}}\r\n",
            "--response--\r\n"
        );
        assert!(matches!(
            parse_batch_response(body, "response", 1),
            Err(CoreError::Network(_))
        ));
    }

    #[test]
    fn replayed_delete_treats_missing_resource_as_success() {
        assert!(ignore_not_found(Err(CoreError::NotFound("gone".into()))).is_ok());
        assert!(matches!(
            ignore_not_found(Err(CoreError::Network("offline".into()))),
            Err(CoreError::Network(_))
        ));
    }

    #[test]
    fn metadata_parser_keeps_native_ids_labels_and_rfc_headers() {
        let resource = json!({
            "id": "msg-1",
            "threadId": "thread-1",
            "historyId": "7",
            "internalDate": "1700000000000",
            "sizeEstimate": 42,
            "labelIds": ["INBOX", "UNREAD", "Label_1"],
            "snippet": "hello",
            "payload": {
                "headers": [
                    { "name": "From", "value": "Alice <alice@example.com>" },
                    { "name": "To", "value": "Me <me@example.com>" },
                    { "name": "Subject", "value": "Native Gmail" },
                    { "name": "Message-ID", "value": "<m@example.com>" }
                ],
                "parts": [{
                    "partId": "1",
                    "mimeType": "application/pdf",
                    "filename": "report.pdf",
                    "body": { "attachmentId": "opaque", "size": 10 }
                }]
            }
        });
        let parsed = parse_resource(&resource).unwrap();
        assert_eq!(parsed.provider_id, "msg-1");
        assert_eq!(parsed.thread_id, "thread-1");
        assert_eq!(parsed.headers.subject, "Native Gmail");
        assert_eq!(parsed.headers.from.unwrap().email, "alice@example.com");
        assert!(parsed.labels.contains(&"UNREAD".into()));
        assert!(parsed.has_attachments);
    }

    #[test]
    fn part_plan_distinguishes_external_text_from_file_attachments() {
        let payload = json!({
            "mimeType": "multipart/mixed",
            "parts": [
                {
                    "partId": "1",
                    "mimeType": "text/plain",
                    "filename": "",
                    "body": { "attachmentId": "large-text", "size": 100 }
                },
                {
                    "partId": "2",
                    "mimeType": "image/png",
                    "filename": "pixel.png",
                    "headers": [{ "name": "Content-Disposition", "value": "attachment" }],
                    "body": { "attachmentId": "image", "size": 200 }
                }
            ]
        });
        let mut parts = Vec::new();
        collect_part_specs(&payload, &mut parts).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[0].provider_attachment_id.as_deref(),
            Some("large-text")
        );
        assert_eq!(parts[1].filename.as_deref(), Some("pixel.png"));
        assert!(!parts[1].is_inline);
    }

    #[test]
    fn arbitrary_local_colors_are_not_sent_to_gmail() {
        assert_eq!(gmail_palette_color("#4a86e8"), Some("#4a86e8"));
        assert_eq!(gmail_palette_color("#123456"), None);
    }
}
