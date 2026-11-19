//! Microsoft accounts' automatic replies, kept in the mailbox settings that
//! Microsoft Graph reaches (#239). Exchange answers each sender once while
//! they're on. It has no subject of their own, and an organization's own
//! senders can get them while others don't.

use crate::error::{CoreError, Result};
use crate::models::{AutoReply, now_ms};
use crate::net::proxy::Owner;
use chrono::{Local, NaiveDateTime, TimeZone, Utc};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const MAILBOX_SETTINGS: &str = "https://graph.microsoft.com/v1.0/me/mailboxSettings";

/// The part of the mailbox settings automatic replies live in.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MailboxSettings {
    #[serde(default)]
    automatic_replies_setting: AutomaticReplies,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AutomaticReplies {
    /// "disabled", "alwaysEnabled" or "scheduled".
    #[serde(default)]
    status: String,
    /// Who outside the organization gets them: "none", "contactsOnly" or
    /// "all".
    #[serde(default)]
    external_audience: String,
    #[serde(default)]
    internal_reply_message: String,
    #[serde(default)]
    external_reply_message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheduled_start_date_time: Option<DateTimeZone>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheduled_end_date_time: Option<DateTimeZone>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DateTimeZone {
    date_time: String,
    time_zone: String,
}

impl DateTimeZone {
    fn utc(at: i64) -> Option<Self> {
        Some(Self {
            date_time: Utc
                .timestamp_millis_opt(at)
                .single()?
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string(),
            time_zone: "UTC".into(),
        })
    }

    /// Milliseconds since the epoch. A time in another zone than UTC, which
    /// Outlook can set, is read as this device's time, which is usually the
    /// mailbox's own.
    fn millis(&self) -> Option<i64> {
        let time = NaiveDateTime::parse_from_str(&self.date_time, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
        if self.time_zone.eq_ignore_ascii_case("UTC") {
            Some(Utc.from_utc_datetime(&time).timestamp_millis())
        } else {
            Local
                .from_local_datetime(&time)
                .earliest()
                .map(|time| time.timestamp_millis())
        }
    }
}

impl From<AutomaticReplies> for AutoReply {
    fn from(replies: AutomaticReplies) -> Self {
        let scheduled = replies.status == "scheduled";
        let message = if replies.internal_reply_message.trim().is_empty() {
            &replies.external_reply_message
        } else {
            &replies.internal_reply_message
        };
        Self {
            enabled: !replies.status.is_empty() && replies.status != "disabled",
            subject: String::new(),
            text: crate::mime::body_text(None, Some(message))
                .map(|text| text.trim().to_owned())
                .unwrap_or_default(),
            starts_at: replies
                .scheduled_start_date_time
                .filter(|_| scheduled)
                .and_then(|start| start.millis()),
            ends_at: replies
                .scheduled_end_date_time
                .filter(|_| scheduled)
                .and_then(|end| end.millis()),
            contacts_only: replies.external_audience == "contactsOnly",
            domain_only: replies.external_audience == "none",
        }
    }
}

impl AutomaticReplies {
    /// Graph's settings for `reply`. Days need an end: replies that start
    /// later without one can't be scheduled.
    fn from_reply(reply: &AutoReply, now: i64) -> Result<Self> {
        let status = match (reply.enabled, reply.starts_at, reply.ends_at) {
            (false, ..) => "disabled",
            (true, None, None) => "alwaysEnabled",
            (true, _, Some(_)) => "scheduled",
            (true, Some(_), None) => {
                return Err(CoreError::Other(
                    "Outlook needs a last day for replies that start on a first day".into(),
                ));
            }
        };
        let message = crate::signatures::text_html(&reply.text);
        Ok(Self {
            status: status.into(),
            external_audience: if reply.domain_only {
                "none"
            } else if reply.contacts_only {
                "contactsOnly"
            } else {
                "all"
            }
            .into(),
            internal_reply_message: message.clone(),
            external_reply_message: message,
            scheduled_start_date_time: reply
                .ends_at
                .and_then(|_| DateTimeZone::utc(reply.starts_at.unwrap_or(now))),
            scheduled_end_date_time: reply.ends_at.and_then(DateTimeZone::utc),
        })
    }
}

/// The mailbox settings of the account a Graph token is for.
pub(crate) struct GraphReplies {
    http: Client,
    url: String,
}

impl GraphReplies {
    /// Requests go through the proxy of `owner`, the account.
    pub(crate) fn new(owner: Owner) -> Result<Self> {
        Self::with_url(owner, MAILBOX_SETTINGS)
    }

    pub(crate) fn with_url(owner: Owner, url: &str) -> Result<Self> {
        let http = crate::net::proxy::http_builder(owner)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("Flectar-Mail/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| CoreError::Network(error.to_string()))?;
        Ok(Self {
            http,
            url: url.to_owned(),
        })
    }

    /// The account's automatic replies.
    pub(crate) async fn read(&self, token: &str) -> Result<AutoReply> {
        let response = self
            .http
            .get(format!("{}?$select=automaticRepliesSetting", self.url))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|error| CoreError::Network(error.to_string()))?;
        let settings: MailboxSettings = answer(response).await?;
        Ok(settings.automatic_replies_setting.into())
    }

    /// Saves `reply`, and gives the replies as the mailbox keeps them now.
    pub(crate) async fn save(&self, token: &str, reply: &AutoReply) -> Result<AutoReply> {
        let body = MailboxSettings {
            automatic_replies_setting: AutomaticReplies::from_reply(reply, now_ms())?,
        };
        let response = self
            .http
            .patch(&self.url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|error| CoreError::Network(error.to_string()))?;
        let saved: MailboxSettings = answer(response).await?;
        Ok(saved.automatic_replies_setting.into())
    }
}

/// Graph's answer, with a refused token as a sign-in to give again.
async fn answer<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return Err(CoreError::NeedsReauth);
    }
    let body = response
        .text()
        .await
        .map_err(|error| CoreError::Network(error.to_string()))?;
    if !status.is_success() {
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|error| error["error"]["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| status.to_string());
        return Err(CoreError::Network(format!("Microsoft Graph: {message}")));
    }
    Ok(serde_json::from_str(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn outlook_s_settings_read_as_the_app_s_automatic_replies() {
        let settings: MailboxSettings = serde_json::from_value(json!({
            "automaticRepliesSetting": {
                "status": "scheduled",
                "externalAudience": "contactsOnly",
                "internalReplyMessage": "<div>Back on Monday.<br>Alex</div>",
                "externalReplyMessage": "<div>Away.</div>",
                "scheduledStartDateTime": { "dateTime": "2026-11-16T00:00:00.0000000", "timeZone": "UTC" },
                "scheduledEndDateTime": { "dateTime": "2026-11-20T23:59:59.0000000", "timeZone": "UTC" },
            }
        }))
        .unwrap();
        let reply = AutoReply::from(settings.automatic_replies_setting);
        assert!(reply.enabled && reply.contacts_only && !reply.domain_only);
        assert!(reply.text.contains("Back on Monday.") && !reply.text.contains('<'));
        assert_eq!(reply.starts_at, Some(1_794_787_200_000));
        assert_eq!(reply.ends_at, Some(1_795_219_199_000));
        assert_eq!(reply.subject, "");

        // Always on, the days don't count.
        let always: AutomaticReplies = serde_json::from_value(json!({
            "status": "alwaysEnabled",
            "externalAudience": "none",
            "internalReplyMessage": "Away",
            "scheduledStartDateTime": { "dateTime": "2026-11-16T00:00:00", "timeZone": "UTC" },
        }))
        .unwrap();
        let reply = AutoReply::from(always);
        assert!(reply.enabled && reply.domain_only);
        assert_eq!((reply.starts_at, reply.ends_at), (None, None));
        assert!(!AutoReply::from(AutomaticReplies::default()).enabled);
    }

    #[tokio::test]
    async fn replies_are_saved_to_and_read_from_the_mailbox_settings() {
        let (origin, requests) =
            super::super::tests::server(|(method, _, body)| match method.as_str() {
                // Graph answers a change with the settings as they are now.
                "PATCH" => (200, serde_json::from_str(body).unwrap()),
                _ => (
                    200,
                    json!({
                        "automaticRepliesSetting": {
                            "status": "alwaysEnabled",
                            "externalAudience": "all",
                            "internalReplyMessage": "<p>Set in Outlook.</p>",
                            "externalReplyMessage": "<p>Set in Outlook.</p>",
                        }
                    }),
                ),
            })
            .await;
        let graph =
            GraphReplies::with_url(Owner::App, &format!("{origin}/v1.0/me/mailboxSettings"))
                .unwrap();
        let reply = AutoReply {
            enabled: true,
            text: "Away".into(),
            ..AutoReply::default()
        };
        let saved = graph.save("token", &reply).await.unwrap();
        assert!(saved.enabled && saved.text == "Away");
        let read = graph.read("token").await.unwrap();
        assert_eq!(read.text, "Set in Outlook.");

        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests[0].0, "PATCH");
        assert_eq!(requests[0].1, "/v1.0/me/mailboxSettings");
        let sent: serde_json::Value = serde_json::from_str(&requests[0].2).unwrap();
        assert_eq!(sent["automaticRepliesSetting"]["status"], "alwaysEnabled");
        assert_eq!(requests[1].0, "GET");
        assert_eq!(
            requests[1].1,
            "/v1.0/me/mailboxSettings?$select=automaticRepliesSetting"
        );
    }

    #[tokio::test]
    async fn a_refused_token_asks_for_a_sign_in() {
        let (origin, _) = super::super::tests::server(|_| {
            (
                403,
                json!({ "error": { "code": "ErrorAccessDenied", "message": "Access is denied." } }),
            )
        })
        .await;
        let graph =
            GraphReplies::with_url(Owner::App, &format!("{origin}/v1.0/me/mailboxSettings"))
                .unwrap();
        assert!(matches!(
            graph.save("token", &AutoReply::default()).await,
            Err(CoreError::NeedsReauth)
        ));
    }

    #[test]
    fn the_app_s_replies_go_to_outlook_as_its_settings() {
        let reply = AutoReply {
            enabled: true,
            subject: "Ignored".into(),
            text: "Back <soon>\nAlex".into(),
            starts_at: None,
            ends_at: Some(1_795_219_199_999),
            contacts_only: true,
            domain_only: false,
        };
        let settings = AutomaticReplies::from_reply(&reply, 1_794_787_200_000).unwrap();
        assert_eq!(settings.status, "scheduled");
        assert_eq!(settings.external_audience, "contactsOnly");
        assert!(
            settings
                .internal_reply_message
                .contains("Back &lt;soon&gt;")
        );
        assert_eq!(
            settings.internal_reply_message,
            settings.external_reply_message
        );
        // Without a first day, they start now.
        assert_eq!(
            settings.scheduled_start_date_time,
            Some(DateTimeZone {
                date_time: "2026-11-16T00:00:00".into(),
                time_zone: "UTC".into(),
            })
        );
        // Graph takes whole seconds.
        assert_eq!(
            settings.scheduled_end_date_time.unwrap().millis(),
            Some(1_795_219_199_000)
        );

        let off = AutoReply {
            enabled: false,
            ..reply.clone()
        };
        assert_eq!(
            AutomaticReplies::from_reply(&off, 0).unwrap().status,
            "disabled"
        );
        let always = AutoReply {
            ends_at: None,
            domain_only: true,
            ..reply.clone()
        };
        let settings = AutomaticReplies::from_reply(&always, 0).unwrap();
        assert_eq!(settings.status, "alwaysEnabled");
        assert_eq!(settings.external_audience, "none");
        assert!(settings.scheduled_start_date_time.is_none());
        // A first day needs a last.
        let open = AutoReply {
            starts_at: Some(1),
            ends_at: None,
            ..reply
        };
        assert!(AutomaticReplies::from_reply(&open, 0).is_err());
    }
}
