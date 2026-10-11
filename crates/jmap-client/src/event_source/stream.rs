/*
 * Copyright Stalwart Labs LLC See the COPYING
 * file at the top-level directory of this distribution.
 *
 * Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
 * https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
 * <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
 * option. This file may not be copied, modified, or distributed
 * except according to those terms.
 */

use crate::{
    client::Client,
    core::session::URLPart,
    event_source::{parser::EventParser, PushNotification},
    DataType,
};
use futures_util::{Stream, StreamExt};
use reqwest::header::{HeaderValue, ACCEPT, CONTENT_TYPE};

impl Client {
    pub async fn event_source(
        &self,
        mut types: Option<impl IntoIterator<Item = DataType>>,
        close_after_state: bool,
        ping: Option<u32>,
        last_event_id: Option<&str>,
    ) -> crate::Result<impl Stream<Item = crate::Result<PushNotification>> + Unpin> {
        let mut event_source_url = String::with_capacity(self.session().event_source_url().len());

        for part in self.event_source_url() {
            match part {
                URLPart::Value(value) => {
                    event_source_url.push_str(value);
                }
                URLPart::Parameter(param) => match param {
                    super::URLParameter::Types => {
                        if let Some(types) = Option::take(&mut types) {
                            event_source_url.push_str(
                                &types
                                    .into_iter()
                                    .map(|state| state.to_string())
                                    .collect::<Vec<_>>()
                                    .join(","),
                            );
                        } else {
                            event_source_url.push('*');
                        }
                    }
                    super::URLParameter::CloseAfter => {
                        event_source_url.push_str(if close_after_state { "state" } else { "no" });
                    }
                    super::URLParameter::Ping => {
                        if let Some(ping) = ping {
                            event_source_url.push_str(&ping.to_string());
                        } else {
                            event_source_url.push('0');
                        }
                    }
                },
            }
        }

        // Add headers
        let mut headers = self.headers().clone();
        headers.remove(CONTENT_TYPE);
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        if let Some(last_event_id) = last_event_id {
            headers.insert(
                "Last-Event-ID",
                HeaderValue::from_str(last_event_id).unwrap(),
            );
        }

        let mut stream = Client::handle_error(
            reqwest::Client::builder()
                .connect_timeout(self.timeout())
                .danger_accept_invalid_certs(self.accept_invalid_certs)
                .redirect(self.redirect_policy())
                .default_headers(headers)
                .build()?
                .get(event_source_url)
                .send()
                .await?,
        )
        .await?
        .bytes_stream();
        let mut parser = EventParser::default();

        Ok(Box::pin(async_stream::stream! {
            loop {
                if let Some(notification) = parser.filter_notification() {
                    yield notification;
                    continue;
                }
                if let Some(result) = stream.next().await {
                    match result {
                        Ok(bytes) => {
                            parser.push_bytes(bytes.to_vec());
                            continue;
                        }
                        Err(err) => {
                            yield Err(err.into());
                            break;
                        }
                    }
                } else {
                    break;
                }
            }
        }))
    }
}
