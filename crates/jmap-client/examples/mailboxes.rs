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
    mailbox::{query::Filter, Role},
};

#[cfg(feature = "async")]
async fn mailboxes() {
    // Connect to the JMAP server using Basic authentication
    let client = Client::new()
        .credentials(("john@example.org", "secret"))
        .connect("https://jmap.example.org")
        .await
        .unwrap();

    // Create a mailbox
    let mailbox_id = client
        .mailbox_create("My Mailbox", None::<String>, Role::None)
        .await
        .unwrap()
        .take_id();

    // Rename a mailbox
    client
        .mailbox_rename(&mailbox_id, "My Renamed Mailbox")
        .await
        .unwrap();

    // Query mailboxes to obtain Inbox's id
    let inbox_id = client
        .mailbox_query(Filter::role(Role::Inbox).into(), None::<Vec<_>>)
        .await
        .unwrap()
        .take_ids()
        .pop()
        .unwrap();

    // Print Inbox's details
    println!(
        "{:?}",
        client.mailbox_get(&inbox_id, None::<Vec<_>>).await.unwrap()
    );

    // Move the newly created mailbox under Inbox
    client
        .mailbox_move(&mailbox_id, inbox_id.into())
        .await
        .unwrap();

    // Delete the mailbox including any messages
    client.mailbox_destroy(&mailbox_id, true).await.unwrap();
}

fn main() {
    #[cfg(feature = "async")]
    let _c = mailboxes();
}
