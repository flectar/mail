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

//! # jmap-client
//!
//! [![crates.io](https://img.shields.io/crates/v/jmap-client)](https://crates.io/crates/jmap-client)
//! [![build](https://github.com/stalwartlabs/jmap-client/actions/workflows/rust.yml/badge.svg)](https://github.com/stalwartlabs/jmap-client/actions/workflows/rust.yml)
//! [![docs.rs](https://img.shields.io/docsrs/jmap-client)](https://docs.rs/jmap-client)
//! [![crates.io](https://img.shields.io/crates/l/jmap-client)](http://www.apache.org/licenses/LICENSE-2.0)
//!
//! _jmap-client_ is a **JSON Meta Application Protocol (JMAP) library** written in Rust. The library is a full implementation of the JMAP RFCs including:
//!
//! - JMAP Core ([RFC 8620](https://datatracker.ietf.org/doc/html/rfc8620))
//! - JMAP for Mail ([RFC 8621](https://datatracker.ietf.org/doc/html/rfc8621))
//! - JMAP over WebSocket ([RFC 8887](https://datatracker.ietf.org/doc/html/rfc8887)).
//! - JMAP for Sieve Scripts ([DRAFT-SIEVE-12](https://www.ietf.org/archive/id/draft-ietf-jmap-sieve-12.html)).
//!
//! Features:
//!
//! - Async and blocking support (use the cargo feature ``blocking`` to enable blocking).
//! - WebSocket async streams (use the cargo feature ``websockets`` to enable JMAP over WebSocket).
//! - EventSource async streams.
//! - Helper functions to reduce boilerplate code and quickly build JMAP requests.
//! - Fast parsing and encoding of JMAP requests.
//!
//! ## Usage Example
//!
//! ```rust
//!     // Connect to the JMAP server using Basic authentication.
//!     // (just for demonstration purposes, Bearer tokens should be used instead)
//!     let client = Client::new()
//!         .credentials(("john@example.org", "secret"))
//!         .connect("https://jmap.example.org")
//!         .await
//!         .unwrap();
//!
//!     // Create a mailbox.
//!     let mailbox_id = client
//!         .mailbox_create("My Mailbox", None::<String>, Role::None)
//!         .await
//!         .unwrap()
//!         .take_id();
//!
//!     // Import a message into the mailbox.
//!     client
//!         .email_import(
//!             b"From: john@example.org\nSubject: test\n\n test".to_vec(),
//!             [&mailbox_id],
//!             ["$draft"].into(),
//!             None,
//!         )
//!         .await
//!         .unwrap();
//!
//!     // Obtain all e-mail ids matching a filter.
//!     let email_id = client
//!         .email_query(
//!             Filter::and([
//!                 email::query::Filter::subject("test"),
//!                 email::query::Filter::in_mailbox(&mailbox_id),
//!                 email::query::Filter::has_keyword("$draft"),
//!             ])
//!             .into(),
//!             [email::query::Comparator::from()].into(),
//!         )
//!         .await
//!         .unwrap()
//!         .take_ids()
//!         .pop()
//!         .unwrap();
//!
//!     // Fetch an e-mail message.
//!     let email = client
//!         .email_get(
//!             &email_id,
//!             [Property::Subject, Property::Preview, Property::Keywords].into(),
//!         )
//!         .await
//!         .unwrap()
//!         .unwrap();
//!     assert_eq!(email.preview().unwrap(), "test");
//!     assert_eq!(email.subject().unwrap(), "test");
//!     assert_eq!(email.keywords(), ["$draft"]);
//!
//!     // Fetch only the updated properties of all mailboxes that changed
//!     // since a state.
//!     let mut request = client.build();
//!     let changes_request = request.changes_mailbox("n").max_changes(0);
//!     let properties_ref = changes_request.updated_properties_reference();
//!     let updated_ref = changes_request.updated_reference();
//!     request
//!         .get_mailbox()
//!         .ids_ref(updated_ref)
//!         .properties_ref(properties_ref);
//!     for mailbox in request
//!         .send()
//!         .await
//!         .unwrap()
//!         .unwrap_method_responses()
//!         .pop()
//!         .unwrap()
//!         .unwrap_get_mailbox()
//!         .unwrap()
//!         .take_list()
//!     {
//!         println!("Changed mailbox: {:#?}", mailbox);
//!     }
//!
//!     // Delete the mailbox including any messages
//!     client.mailbox_destroy(&mailbox_id, true).await.unwrap();
//!
//!     // Open an EventSource connection with the JMAP server.
//!     let mut stream = client
//!         .event_source(
//!             [
//!                 DataType::Email,
//!                 DataType::EmailDelivery,
//!                 DataType::Mailbox,
//!                 DataType::EmailSubmission,
//!                 DataType::Identity,
//!             ]
//!             .into(),
//!             false,
//!             60.into(),
//!             None,
//!         )
//!         .await
//!         .unwrap();
//!
//!     // Consume events received over EventSource.
//!     while let Some(event) = stream.next().await {
//!         let changes = event.unwrap();
//!         println!("-> Change id: {:?}", changes.id());
//!         for account_id in changes.changed_accounts() {
//!             println!(" Account {} has changes:", account_id);
//!             if let Some(account_changes) = changes.changes(account_id) {
//!                 for (type_state, state_id) in account_changes {
//!                     println!("   Type {:?} has a new state {}.", type_state, state_id);
//!                 }
//!             }
//!         }
//!     }
//! ```
//!
//! More examples available under the [examples](examples) directory.
//!
//! ## Testing
//!
//! To run the testsuite:
//!
//! ```bash
//!  $ cargo test --all-features
//! ```
//!
//! ## Conformed RFCs
//!
//! - [RFC 8620 - The JSON Meta Application Protocol (JMAP)](https://datatracker.ietf.org/doc/html/rfc8620)
//! - [RFC 8621 - The JSON Meta Application Protocol (JMAP) for Mail](https://datatracker.ietf.org/doc/html/rfc8621)
//! - [RFC 8887 - A JSON Meta Application Protocol (JMAP) Subprotocol for WebSocket](https://datatracker.ietf.org/doc/html/rfc8887)
//!
//! ## License
//!
//! Licensed under either of
//!
//!  * Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
//!  * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
//!
//! at your option.
//!
//! ## Copyright
//!
//! Copyright (C) 2022, Stalwart Labs LLC
//!

#[forbid(unsafe_code)]
pub mod blob;
pub mod client;
pub mod core;
pub mod email;
pub mod email_submission;
#[cfg(feature = "async")]
pub mod event_source;
pub mod identity;
pub mod mailbox;
pub mod principal;
pub mod push_subscription;
pub mod sieve;
pub mod thread;
pub mod vacation_response;

use crate::core::error::MethodError;
use crate::core::error::ProblemDetails;
use crate::core::set::SetError;
use ahash::AHashMap;
use serde::{Deserialize, Serialize};
use std::fmt::Display;

#[cfg(feature = "websockets")]
pub mod client_ws;

#[derive(Debug, Clone, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub enum URI {
    #[serde(rename = "urn:ietf:params:jmap:core")]
    Core,
    #[serde(rename = "urn:ietf:params:jmap:mail")]
    Mail,
    #[serde(rename = "urn:ietf:params:jmap:submission")]
    Submission,
    #[serde(rename = "urn:ietf:params:jmap:vacationresponse")]
    VacationResponse,
    #[serde(rename = "urn:ietf:params:jmap:contacts")]
    Contacts,
    #[serde(rename = "urn:ietf:params:jmap:calendars")]
    Calendars,
    #[serde(rename = "urn:ietf:params:jmap:websocket")]
    WebSocket,
    #[serde(rename = "urn:ietf:params:jmap:sieve")]
    Sieve,
    #[serde(rename = "urn:ietf:params:jmap:blob")]
    Blob,
    #[serde(rename = "urn:ietf:params:jmap:quota")]
    Quota,
    #[serde(rename = "urn:ietf:params:jmap:principals")]
    Principals,
    #[serde(rename = "urn:ietf:params:jmap:principals:owner")]
    PrincipalsOwner,
}

impl AsRef<str> for URI {
    fn as_ref(&self) -> &str {
        match self {
            URI::Core => "urn:ietf:params:jmap:core",
            URI::Mail => "urn:ietf:params:jmap:mail",
            URI::Submission => "urn:ietf:params:jmap:submission",
            URI::VacationResponse => "urn:ietf:params:jmap:vacationresponse",
            URI::Contacts => "urn:ietf:params:jmap:contacts",
            URI::Calendars => "urn:ietf:params:jmap:calendars",
            URI::WebSocket => "urn:ietf:params:jmap:websocket",
            URI::Sieve => "urn:ietf:params:jmap:sieve",
            URI::Blob => "urn:ietf:params:jmap:blob",
            URI::Quota => "urn:ietf:params:jmap:quota",
            URI::Principals => "urn:ietf:params:jmap:principals",
            URI::PrincipalsOwner => "urn:ietf:params:jmap:principals:owner",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Method {
    #[serde(rename = "Core/echo")]
    Echo,
    #[serde(rename = "Blob/copy")]
    CopyBlob,
    #[serde(rename = "PushSubscription/get")]
    GetPushSubscription,
    #[serde(rename = "PushSubscription/set")]
    SetPushSubscription,
    #[serde(rename = "Mailbox/get")]
    GetMailbox,
    #[serde(rename = "Mailbox/changes")]
    ChangesMailbox,
    #[serde(rename = "Mailbox/query")]
    QueryMailbox,
    #[serde(rename = "Mailbox/queryChanges")]
    QueryChangesMailbox,
    #[serde(rename = "Mailbox/set")]
    SetMailbox,
    #[serde(rename = "Thread/get")]
    GetThread,
    #[serde(rename = "Thread/changes")]
    ChangesThread,
    #[serde(rename = "Email/get")]
    GetEmail,
    #[serde(rename = "Email/changes")]
    ChangesEmail,
    #[serde(rename = "Email/query")]
    QueryEmail,
    #[serde(rename = "Email/queryChanges")]
    QueryChangesEmail,
    #[serde(rename = "Email/set")]
    SetEmail,
    #[serde(rename = "Email/copy")]
    CopyEmail,
    #[serde(rename = "Email/import")]
    ImportEmail,
    #[serde(rename = "Email/parse")]
    ParseEmail,
    #[serde(rename = "SearchSnippet/get")]
    GetSearchSnippet,
    #[serde(rename = "Identity/get")]
    GetIdentity,
    #[serde(rename = "Identity/changes")]
    ChangesIdentity,
    #[serde(rename = "Identity/set")]
    SetIdentity,
    #[serde(rename = "EmailSubmission/get")]
    GetEmailSubmission,
    #[serde(rename = "EmailSubmission/changes")]
    ChangesEmailSubmission,
    #[serde(rename = "EmailSubmission/query")]
    QueryEmailSubmission,
    #[serde(rename = "EmailSubmission/queryChanges")]
    QueryChangesEmailSubmission,
    #[serde(rename = "EmailSubmission/set")]
    SetEmailSubmission,
    #[serde(rename = "VacationResponse/get")]
    GetVacationResponse,
    #[serde(rename = "VacationResponse/set")]
    SetVacationResponse,
    #[serde(rename = "SieveScript/get")]
    GetSieveScript,
    #[serde(rename = "SieveScript/set")]
    SetSieveScript,
    #[serde(rename = "SieveScript/query")]
    QuerySieveScript,
    #[serde(rename = "SieveScript/validate")]
    ValidateSieveScript,
    #[serde(rename = "Principal/get")]
    GetPrincipal,
    #[serde(rename = "Principal/changes")]
    ChangesPrincipal,
    #[serde(rename = "Principal/query")]
    QueryPrincipal,
    #[serde(rename = "Principal/queryChanges")]
    QueryChangesPrincipal,
    #[serde(rename = "Principal/set")]
    SetPrincipal,
    #[serde(rename = "error")]
    Error,
}

#[derive(Debug, Serialize, Deserialize, Eq, PartialEq, Hash, Clone)]
pub enum DataType {
    #[serde(rename = "Email")]
    Email = 0,
    #[serde(rename = "EmailDelivery")]
    EmailDelivery = 1,
    #[serde(rename = "EmailSubmission")]
    EmailSubmission = 2,
    #[serde(rename = "Mailbox")]
    Mailbox = 3,
    #[serde(rename = "Thread")]
    Thread = 4,
    #[serde(rename = "Identity")]
    Identity = 5,
    #[serde(rename = "Core")]
    Core = 6,
    #[serde(rename = "PushSubscription")]
    PushSubscription = 7,
    #[serde(rename = "SearchSnippet")]
    SearchSnippet = 8,
    #[serde(rename = "VacationResponse")]
    VacationResponse = 9,
    #[serde(rename = "MDN")]
    Mdn = 10,
    #[serde(rename = "Quota")]
    Quota = 11,
    #[serde(rename = "SieveScript")]
    SieveScript = 12,
    #[serde(rename = "Calendar")]
    Calendar = 13,
    #[serde(rename = "CalendarEvent")]
    CalendarEvent = 14,
    #[serde(rename = "CalendarEventNotification")]
    CalendarEventNotification = 15,
    #[serde(rename = "AddressBook")]
    AddressBook = 16,
    #[serde(rename = "ContactCard")]
    ContactCard = 17,
    #[serde(rename = "FileNode")]
    FileNode = 18,
    #[serde(rename = "Principal")]
    Principal = 19,
    #[serde(rename = "ShareNotification")]
    ShareNotification = 20,
    #[serde(rename = "ParticipantIdentity")]
    ParticipantIdentity = 21,
    #[serde(rename = "CalendarAlert")]
    CalendarAlert = 22,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "@type")]
pub enum PushObject {
    StateChange {
        changed: AHashMap<String, AHashMap<DataType, String>>,
    },
    EmailPush {
        #[serde(rename = "accountId")]
        account_id: String,
        email: serde_json::Value,
    },
    CalendarAlert(CalendarAlert),
    Group {
        entries: Vec<PushObject>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarAlert {
    #[serde(rename = "accountId")]
    pub account_id: String,
    #[serde(rename = "calendarEventId")]
    pub calendar_event_id: String,
    pub uid: String,
    #[serde(rename = "recurrenceId")]
    pub recurrence_id: Option<String>,
    #[serde(rename = "alertId")]
    pub alert_id: String,
}

#[derive(Debug, Clone)]
pub struct Get;
#[derive(Debug, Clone)]
pub struct Set;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Transport(reqwest::Error),
    Parse(serde_json::Error),
    Internal(String),
    Problem(Box<ProblemDetails>),
    Server(String),
    Method(MethodError),
    Set(SetError<String>),
    #[cfg(feature = "websockets")]
    WebSocket(tokio_tungstenite::tungstenite::error::Error),
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Transport(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Parse(e)
    }
}

impl From<MethodError> for Error {
    fn from(e: MethodError) -> Self {
        Error::Method(e)
    }
}

impl From<ProblemDetails> for Error {
    fn from(e: ProblemDetails) -> Self {
        Error::Problem(Box::new(e))
    }
}

impl From<SetError<String>> for Error {
    fn from(e: SetError<String>) -> Self {
        Error::Set(e)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::Internal(s.to_string())
    }
}

#[cfg(feature = "websockets")]
impl From<tokio_tungstenite::tungstenite::error::Error> for Error {
    fn from(e: tokio_tungstenite::tungstenite::error::Error) -> Self {
        Error::WebSocket(e)
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Transport(e) => write!(f, "Transport error: {}", e),
            Error::Parse(e) => write!(f, "Parse error: {}", e),
            Error::Internal(e) => write!(f, "Internal error: {}", e),
            Error::Problem(e) => write!(f, "Request failed: {}", e),
            Error::Server(e) => write!(f, "Server failed: {}", e),
            Error::Method(e) => write!(f, "Request failed: {}", e),
            Error::Set(e) => write!(f, "Set failed: {}", e),
            #[cfg(feature = "websockets")]
            Error::WebSocket(e) => write!(f, "WebSockets error: {}", e),
        }
    }
}

impl Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataType::Mailbox => write!(f, "Mailbox"),
            DataType::Thread => write!(f, "Thread"),
            DataType::Email => write!(f, "Email"),
            DataType::EmailDelivery => write!(f, "EmailDelivery"),
            DataType::Identity => write!(f, "Identity"),
            DataType::EmailSubmission => write!(f, "EmailSubmission"),
            DataType::CalendarAlert => write!(f, "CalendarAlert"),
            DataType::Core => write!(f, "Core"),
            DataType::PushSubscription => write!(f, "PushSubscription"),
            DataType::SearchSnippet => write!(f, "SearchSnippet"),
            DataType::VacationResponse => write!(f, "VacationResponse"),
            DataType::Mdn => write!(f, "MDN"),
            DataType::Quota => write!(f, "Quota"),
            DataType::SieveScript => write!(f, "SieveScript"),
            DataType::Calendar => write!(f, "Calendar"),
            DataType::CalendarEvent => write!(f, "CalendarEvent"),
            DataType::CalendarEventNotification => write!(f, "CalendarEventNotification"),
            DataType::AddressBook => write!(f, "AddressBook"),
            DataType::ContactCard => write!(f, "ContactCard"),
            DataType::FileNode => write!(f, "FileNode"),
            DataType::Principal => write!(f, "Principal"),
            DataType::ShareNotification => write!(f, "ShareNotification"),
            DataType::ParticipantIdentity => write!(f, "ParticipantIdentity"),
        }
    }
}
