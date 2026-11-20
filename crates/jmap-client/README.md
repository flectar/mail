# jmap-client

[![crates.io](https://img.shields.io/crates/v/jmap-client)](https://crates.io/crates/jmap-client)
[![build](https://github.com/stalwartlabs/jmap-client/actions/workflows/rust.yml/badge.svg)](https://github.com/stalwartlabs/jmap-client/actions/workflows/rust.yml)
[![docs.rs](https://img.shields.io/docsrs/jmap-client)](https://docs.rs/jmap-client)
[![crates.io](https://img.shields.io/crates/l/jmap-client)](http://www.apache.org/licenses/LICENSE-2.0)

_jmap-client_ is a **JSON Meta Application Protocol (JMAP) library** written in Rust. The library is a full implementation of the JMAP RFCs including:

- JMAP Core ([RFC 8620](https://datatracker.ietf.org/doc/html/rfc8620))
- JMAP for Mail ([RFC 8621](https://datatracker.ietf.org/doc/html/rfc8621)) 
- JMAP over WebSocket ([RFC 8887](https://datatracker.ietf.org/doc/html/rfc8887)).
- JMAP for Sieve Scripts ([DRAFT-SIEVE-14](https://www.ietf.org/archive/id/draft-ietf-jmap-sieve-14.html)).

Features:

- Async and blocking support (use the cargo feature ``blocking`` to enable blocking).
- WebSocket async streams (use the cargo feature ``websockets`` to enable JMAP over WebSocket).
- EventSource async streams.
- Helper functions to reduce boilerplate code and quickly build JMAP requests.
- Fast parsing and encoding of JMAP requests.

## Usage Example

```rust
// Connect to the JMAP server using Basic authentication.
// (just for demonstration purposes, Bearer tokens should be used instead)
let client = Client::new()
    .credentials(("john@example.org", "secret"))
    .connect("https://jmap.example.org")
    .await
    .unwrap();

// Create a mailbox.
let mailbox_id = client
    .mailbox_create("My Mailbox", None::<String>, Role::None)
    .await
    .unwrap()
    .take_id();

// Import a message into the mailbox.
client
    .email_import(
        b"From: john@example.org\nSubject: test\n\n test".to_vec(),
        [&mailbox_id],
        ["$draft"].into(),
        None,
    )
    .await
    .unwrap();

// Obtain all e-mail ids matching a filter.
let email_id = client
    .email_query(
        Filter::and([
            email::query::Filter::subject("test"),
            email::query::Filter::in_mailbox(&mailbox_id),
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

// Fetch an e-mail message.
let email = client
    .email_get(
        &email_id,
        [Property::Subject, Property::Preview, Property::Keywords].into(),
    )
    .await
    .unwrap()
    .unwrap();
assert_eq!(email.preview().unwrap(), "test");
assert_eq!(email.subject().unwrap(), "test");
assert_eq!(email.keywords(), ["$draft"]);

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

// Delete the mailbox including any messages
client.mailbox_destroy(&mailbox_id, true).await.unwrap();

// Open an EventSource connection with the JMAP server.
let mut stream = client
    .event_source(
        [
            TypeState::Email,
            TypeState::EmailDelivery,
            TypeState::Mailbox,
            TypeState::EmailSubmission,
            TypeState::Identity,
        ]
        .into(),
        false,
        60.into(),
        None,
    )
    .await
    .unwrap();

// Consume events received over EventSource.
while let Some(event) = stream.next().await {
    let changes = event.unwrap();
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
```

More examples available under the [examples](examples) directory. 

## Testing

To run the testsuite:

```bash
 $ cargo test --all-features
```

## Conformed RFCs

- [RFC 8620 - The JSON Meta Application Protocol (JMAP)](https://datatracker.ietf.org/doc/html/rfc8620)
- [RFC 8621 - The JSON Meta Application Protocol (JMAP) for Mail](https://datatracker.ietf.org/doc/html/rfc8621)
- [RFC 8887 - A JSON Meta Application Protocol (JMAP) Subprotocol for WebSocket](https://datatracker.ietf.org/doc/html/rfc8887)

## License

Licensed under either of

 * Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Copyright

Copyright (C) 2022, Stalwart Labs LLC

