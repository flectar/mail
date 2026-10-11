/*
 * Copyright Stalwart Labs LLC See the COPYING
 * file at the top-level directory of this distribution.
 *
 * Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
 * https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
 * file at the top-level directory of this distribution.
 *
 * Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
 * https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
 * <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
 * option. This file may not be copied, modified, or distributed
 * except according to those terms.
 */

#[cfg(feature = "async")]
use futures_util::StreamExt;
#[cfg(feature = "async")]
use jmap_client::{client::Client, DataType};

#[cfg(feature = "async")]
async fn event_source() {
    // Connect to the JMAP server using Basic authentication
    let client = Client::new()
        .credentials(("john@example.org", "secret"))
        .connect("https://jmap.example.org")
        .await
        .unwrap();

    // Open EventSource connection
    let mut stream = client
        .event_source(
            [
                DataType::Email,
                DataType::EmailDelivery,
                DataType::Mailbox,
                DataType::EmailSubmission,
                DataType::Identity,
            ]
            .into(),
            false,
            60.into(),
            None,
        )
        .await
        .unwrap();

    // Consume events
    while let Some(event) = stream.next().await {
        use jmap_client::event_source::PushNotification;

        match event.unwrap() {
            PushNotification::StateChange(changes) => {
                println!("-> Change id: {:?}", changes.id());
                for account_id in changes.changed_accounts() {
                    println!(" Account {} has changes:", account_id);
                    if let Some(account_changes) = changes.changes(account_id) {
                        for (type_state, state_id) in account_changes {
                            println!("   Type {:?} has a new state {}.", type_state, state_id);
                        }
                    }
                }
            }
            PushNotification::CalendarAlert(calendar_alert) => {
                println!(
                    "-> Calendar alert received for event {} (alert id {}).",
                    calendar_alert.calendar_event_id, calendar_alert.alert_id
                );
            }
        }
    }
}

fn main() {
    #[cfg(feature = "async")]
    let _c = event_source();
}
