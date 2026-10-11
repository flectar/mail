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
use jmap_client::{
    client::Client,
    core::query::Filter,
    email::{self, Property},
    mailbox::{self, Role},
};

#[cfg(feature = "async")]
const TEST_MESSAGE: &[u8; 90] = br#"From: john@example.org
To: jane@example.org
Subject: Testing JMAP client

This is a test.
"#;

#[cfg(feature = "async")]
async fn messages() {
    // Connect to the JMAP server using Basic authentication
    let client = Client::new()
        .credentials(("john@example.org", "secret"))
        .connect("https://jmap.example.org")
        .await
        .unwrap();

    // Query mailboxes to obtain Inbox and Trash folder id
    let inbox_id = client
        .mailbox_query(
            mailbox::query::Filter::role(Role::Inbox).into(),
            None::<Vec<_>>,
        )
        .await
        .unwrap()
        .take_ids()
        .pop()
        .unwrap();
    let trash_id = client
        .mailbox_query(
            mailbox::query::Filter::role(Role::Trash).into(),
            None::<Vec<_>>,
        )
        .await
        .unwrap()
        .take_ids()
        .pop()
        .unwrap();

    // Import message into inbox
    client
        .email_import(TEST_MESSAGE.to_vec(), [&inbox_id], ["$draft"].into(), None)
        .await
        .unwrap();

    // Query mailbox
    let email_id = client
        .email_query(
            Filter::and([
                email::query::Filter::subject("test"),
                email::query::Filter::in_mailbox(&inbox_id),
                email::query::Filter::has_keyword("$draft"),
            ])
            .into(),
            [email::query::Comparator::from()].into(),
        )
        .await
        .unwrap()
        .take_ids()
        .pop()
        .unwrap();

    // Fetch message
    let email = client
        .email_get(
            &email_id,
            [Property::Subject, Property::Preview, Property::Keywords].into(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(email.preview().unwrap(), "This is a test.");
    assert_eq!(email.subject().unwrap(), "Testing JMAP client");
    assert_eq!(email.keywords(), ["$draft"]);

    // Remove the $draft keyword
    client
        .email_set_keyword(&email_id, "$draft", false)
        .await
        .unwrap();

    // Replace all keywords
    client
        .email_set_keywords(&email_id, ["$seen", "$important"])
        .await
        .unwrap();

    // Move the message to the Trash folder
    client
        .email_set_mailboxes(&email_id, [&trash_id])
        .await
        .unwrap();

    // Destroy the e-mail
    client.email_destroy(&email_id).await.unwrap();
}

fn main() {
    #[cfg(feature = "async")]
    let _c = messages();
}
