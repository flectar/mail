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
use jmap_client::{client::Client, core::query, email, mailbox};

#[cfg(feature = "async")]
async fn result_reference() {
    // Connect to the JMAP server using Basic authentication
    let client = Client::new()
        .credentials(("john@example.org", "secret"))
        .connect("https://jmap.example.org")
        .await
        .unwrap();

    // Delete e-mails matching a filter
    let mut request = client.build();
    let result_ref = request
        .query_email()
        .filter(query::Filter::and([
            email::query::Filter::has_keyword("$draft"),
            email::query::Filter::from("bill"),
        ]))
        .result_reference();
    request.set_email().destroy_ref(result_ref);
    let _destroyed_ids = request
        .send()
        .await
        .unwrap()
        .unwrap_method_responses()
        .pop()
        .unwrap()
        .unwrap_set_email()
        .unwrap()
        .take_destroyed_ids();

    // Fetch mailboxes matching a filter
    let mut request = client.build();
    let query_result = request
        .query_mailbox()
        .filter(query::Filter::and([
            mailbox::query::Filter::has_any_role(false),
            mailbox::query::Filter::is_subscribed(true),
        ]))
        .result_reference();
    request.get_mailbox().ids_ref(query_result).properties([
        mailbox::Property::Id,
        mailbox::Property::Name,
        mailbox::Property::ParentId,
        mailbox::Property::TotalEmails,
        mailbox::Property::UnreadEmails,
    ]);
    let _mailboxes = request
        .send()
        .await
        .unwrap()
        .unwrap_method_responses()
        .pop()
        .unwrap()
        .unwrap_get_mailbox()
        .unwrap()
        .take_list();

    // Fetch only the updated properties of all mailboxes that changed
    // since a state.
    let mut request = client.build();
    let changes_request = request.changes_mailbox("n").max_changes(0);
    let properties_ref = changes_request.updated_properties_reference();
    let updated_ref = changes_request.updated_reference();
    request
        .get_mailbox()
        .ids_ref(updated_ref)
        .properties_ref(properties_ref);
    for mailbox in request
        .send()
        .await
        .unwrap()
        .unwrap_method_responses()
        .pop()
        .unwrap()
        .unwrap_get_mailbox()
        .unwrap()
        .take_list()
    {
        println!("Changed mailbox: {:#?}", mailbox);
    }
}

fn main() {
    #[cfg(feature = "async")]
    let _c = result_reference();
}
