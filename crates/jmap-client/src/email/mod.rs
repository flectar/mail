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

pub mod get;
pub mod helpers;
pub mod import;
pub mod parse;
pub mod query;
pub mod search_snippet;
pub mod set;
use ahash::AHashMap;
use chrono::{DateTime, Utc};
use serde::{de::Visitor, Deserialize, Serialize};
use std::fmt::{self, Display, Formatter};

use crate::{
    core::{changes::ChangesObject, request::ResultReference, Object},
    Get, Set,
};

impl Object for Email<Set> {
    type Property = Property;

    fn requires_account_id() -> bool {
        true
    }
}

impl Object for Email<Get> {
    type Property = Property;

    fn requires_account_id() -> bool {
        true
    }
}

impl ChangesObject for Email<Set> {
    type ChangesResponse = ();
}

impl ChangesObject for Email<Get> {
    type ChangesResponse = ();
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Email<State = Get> {
    #[serde(skip)]
    _create_id: Option<usize>,

    #[serde(skip)]
    _state: std::marker::PhantomData<State>,

    #[serde(rename = "id")]
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,

    #[serde(rename = "blobId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    blob_id: Option<String>,

    #[serde(rename = "threadId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,

    #[serde(rename = "mailboxIds")]
    #[serde(skip_serializing_if = "Option::is_none")]
    mailbox_ids: Option<AHashMap<String, bool>>,

    #[serde(rename = "#mailboxIds")]
    #[serde(skip_deserializing)]
    #[serde(skip_serializing_if = "Option::is_none")]
    mailbox_ids_ref: Option<ResultReference>,

    #[serde(rename = "keywords")]
    #[serde(skip_serializing_if = "Option::is_none")]
    keywords: Option<AHashMap<String, bool>>,

    #[serde(rename = "size")]
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<usize>,

    #[serde(rename = "receivedAt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    received_at: Option<DateTime<Utc>>,

    #[cfg_attr(
        not(feature = "debug"),
        serde(alias = "header:Message-ID:asMessageIds")
    )]
    #[serde(rename = "messageId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<Vec<String>>,

    #[serde(rename = "inReplyTo")]
    #[cfg_attr(
        not(feature = "debug"),
        serde(alias = "header:In-Reply-To:asMessageIds")
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    in_reply_to: Option<Vec<String>>,

    #[serde(rename = "references")]
    #[cfg_attr(
        not(feature = "debug"),
        serde(alias = "header:References:asMessageIds")
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    references: Option<Vec<String>>,

    #[serde(rename = "sender")]
    #[cfg_attr(not(feature = "debug"), serde(alias = "header:Sender:asAddresses"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    sender: Option<Vec<EmailAddress>>,

    #[serde(rename = "from")]
    #[cfg_attr(not(feature = "debug"), serde(alias = "header:From:asAddresses"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<Vec<EmailAddress>>,

    #[serde(rename = "to")]
    #[cfg_attr(not(feature = "debug"), serde(alias = "header:To:asAddresses"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<Vec<EmailAddress>>,

    #[serde(rename = "cc")]
    #[cfg_attr(not(feature = "debug"), serde(alias = "header:Cc:asAddresses"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    cc: Option<Vec<EmailAddress>>,

    #[serde(rename = "bcc")]
    #[cfg_attr(not(feature = "debug"), serde(alias = "header:Bcc:asAddresses"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    bcc: Option<Vec<EmailAddress>>,

    #[serde(rename = "replyTo")]
    #[cfg_attr(not(feature = "debug"), serde(alias = "header:Reply-To:asAddresses"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to: Option<Vec<EmailAddress>>,

    #[serde(rename = "subject")]
    #[cfg_attr(not(feature = "debug"), serde(alias = "header:Subject:asText"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    subject: Option<String>,

    #[serde(rename = "sentAt")]
    #[cfg_attr(not(feature = "debug"), serde(alias = "header:Date:asDate"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    sent_at: Option<DateTime<Utc>>,

    #[serde(rename = "bodyStructure")]
    #[serde(skip_serializing_if = "Option::is_none")]
    body_structure: Option<Box<EmailBodyPart>>,

    #[serde(rename = "bodyValues")]
    #[serde(skip_serializing_if = "Option::is_none")]
    body_values: Option<AHashMap<String, EmailBodyValue>>,

    #[serde(rename = "textBody")]
    #[serde(skip_serializing_if = "Option::is_none")]
    text_body: Option<Vec<EmailBodyPart>>,

    #[serde(rename = "htmlBody")]
    #[serde(skip_serializing_if = "Option::is_none")]
    html_body: Option<Vec<EmailBodyPart>>,

    #[serde(rename = "attachments")]
    #[serde(skip_serializing_if = "Option::is_none")]
    attachments: Option<Vec<EmailBodyPart>>,

    #[serde(rename = "hasAttachment")]
    #[serde(skip_serializing_if = "Option::is_none")]
    has_attachment: Option<bool>,

    #[serde(rename = "preview")]
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<String>,

    #[serde(flatten)]
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    headers: AHashMap<Header, Option<HeaderValue>>,

    #[serde(flatten)]
    #[serde(skip_deserializing)]
    #[serde(skip_serializing_if = "Option::is_none")]
    patch: Option<AHashMap<String, bool>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailBodyPart<State = Get> {
    #[serde(skip)]
    _state: std::marker::PhantomData<State>,

    #[serde(rename = "partId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    part_id: Option<String>,

    #[serde(rename = "blobId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    blob_id: Option<String>,

    #[serde(rename = "size")]
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<usize>,

    #[serde(rename = "headers")]
    #[serde(skip_serializing_if = "Option::is_none")]
    headers: Option<Vec<EmailHeader>>,

    #[serde(rename = "name")]
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    #[serde(rename = "type")]
    #[serde(skip_serializing_if = "Option::is_none")]
    type_: Option<String>,

    #[serde(rename = "charset")]
    #[serde(skip_serializing_if = "Option::is_none")]
    charset: Option<String>,

    #[serde(rename = "disposition")]
    #[serde(skip_serializing_if = "Option::is_none")]
    disposition: Option<String>,

    #[serde(rename = "cid")]
    #[serde(skip_serializing_if = "Option::is_none")]
    cid: Option<String>,

    #[serde(rename = "language")]
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<Vec<String>>,

    #[serde(rename = "location")]
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<String>,

    #[serde(rename = "subParts")]
    #[serde(skip_serializing_if = "Option::is_none")]
    sub_parts: Option<Vec<EmailBodyPart>>,

    #[serde(flatten)]
    #[serde(skip_serializing_if = "Option::is_none")]
    header: Option<AHashMap<Header, HeaderValue>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailBodyValue<State = Get> {
    #[serde(skip)]
    _state: std::marker::PhantomData<State>,

    #[serde(rename = "value")]
    value: String,

    #[serde(rename = "isEncodingProblem")]
    #[serde(skip_serializing_if = "Option::is_none")]
    is_encoding_problem: Option<bool>,

    #[serde(rename = "isTruncated")]
    #[serde(skip_serializing_if = "Option::is_none")]
    is_truncated: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailAddress<State = Get> {
    #[serde(skip)]
    _state: std::marker::PhantomData<State>,

    name: Option<String>,
    email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailAddressGroup<State = Get> {
    #[serde(skip)]
    _state: std::marker::PhantomData<State>,

    name: Option<String>,
    addresses: Vec<EmailAddress>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailHeader<State = Get> {
    #[serde(skip)]
    _state: std::marker::PhantomData<State>,

    name: String,
    value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Property {
    Id,
    BlobId,
    ThreadId,
    MailboxIds,
    Keywords,
    Size,
    ReceivedAt,
    MessageId,
    InReplyTo,
    References,
    Sender,
    From,
    To,
    Cc,
    Bcc,
    ReplyTo,
    Subject,
    SentAt,
    BodyStructure,
    BodyValues,
    TextBody,
    HtmlBody,
    Attachments,
    HasAttachment,
    Preview,
    Header(Header),
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HeaderValue {
    AsDate(DateTime<Utc>),
    AsDateAll(Vec<DateTime<Utc>>),
    AsText(String),
    AsTextAll(Vec<String>),
    AsTextListAll(Vec<Vec<String>>),
    AsAddressesAll(Vec<Vec<EmailAddress>>),
    AsAddresses(Vec<EmailAddress>),
    AsGroupedAddressesAll(Vec<Vec<EmailAddressGroup>>),
    AsGroupedAddresses(Vec<EmailAddressGroup>),
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Clone)]
pub struct Header {
    pub name: String,
    pub form: HeaderForm,
    pub all: bool,
}

#[derive(PartialEq, Eq, Hash, Debug, Clone, PartialOrd, Ord)]
pub enum HeaderForm {
    Raw,
    Text,
    Addresses,
    GroupedAddresses,
    MessageIds,
    Date,
    URLs,
}

impl Property {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "id" => Some(Property::Id),
            "blobId" => Some(Property::BlobId),
            "threadId" => Some(Property::ThreadId),
            "mailboxIds" => Some(Property::MailboxIds),
            "keywords" => Some(Property::Keywords),
            "size" => Some(Property::Size),
            "receivedAt" => Some(Property::ReceivedAt),
            "messageId" => Some(Property::MessageId),
            "inReplyTo" => Some(Property::InReplyTo),
            "references" => Some(Property::References),
            "sender" => Some(Property::Sender),
            "from" => Some(Property::From),
            "to" => Some(Property::To),
            "cc" => Some(Property::Cc),
            "bcc" => Some(Property::Bcc),
            "replyTo" => Some(Property::ReplyTo),
            "subject" => Some(Property::Subject),
            "sentAt" => Some(Property::SentAt),
            "hasAttachment" => Some(Property::HasAttachment),
            "preview" => Some(Property::Preview),
            "bodyValues" => Some(Property::BodyValues),
            "textBody" => Some(Property::TextBody),
            "htmlBody" => Some(Property::HtmlBody),
            "attachments" => Some(Property::Attachments),
            "bodyStructure" => Some(Property::BodyStructure),
            _ if value.starts_with("header:") => Some(Property::Header(Header::parse(value)?)),
            _ => Some(Property::Other(value.to_string())),
        }
    }
}

impl Display for Property {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Property::Id => write!(f, "id"),
            Property::BlobId => write!(f, "blobId"),
            Property::ThreadId => write!(f, "threadId"),
            Property::MailboxIds => write!(f, "mailboxIds"),
            Property::Keywords => write!(f, "keywords"),
            Property::Size => write!(f, "size"),
            Property::ReceivedAt => write!(f, "receivedAt"),
            Property::MessageId => write!(f, "messageId"),
            Property::InReplyTo => write!(f, "inReplyTo"),
            Property::References => write!(f, "references"),
            Property::Sender => write!(f, "sender"),
            Property::From => write!(f, "from"),
            Property::To => write!(f, "to"),
            Property::Cc => write!(f, "cc"),
            Property::Bcc => write!(f, "bcc"),
            Property::ReplyTo => write!(f, "replyTo"),
            Property::Subject => write!(f, "subject"),
            Property::SentAt => write!(f, "sentAt"),
            Property::BodyStructure => write!(f, "bodyStructure"),
            Property::BodyValues => write!(f, "bodyValues"),
            Property::TextBody => write!(f, "textBody"),
            Property::HtmlBody => write!(f, "htmlBody"),
            Property::Attachments => write!(f, "attachments"),
            Property::HasAttachment => write!(f, "hasAttachment"),
            Property::Preview => write!(f, "preview"),
            Property::Header(header) => header.fmt(f),
            Property::Other(other) => write!(f, "{}", other),
        }
    }
}

impl Serialize for Property {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

struct PropertyVisitor;

impl<'de> Visitor<'de> for PropertyVisitor {
    type Value = Property;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a valid JMAP e-mail property")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Property::parse(v).ok_or_else(|| {
            serde::de::Error::custom(format!("Failed to parse JMAP property '{}'", v))
        })
    }
}

impl<'de> Deserialize<'de> for Property {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(PropertyVisitor)
    }
}

impl Serialize for Header {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

struct HeaderVisitor;

impl<'de> Visitor<'de> for HeaderVisitor {
    type Value = Header;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a valid JMAP header")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Header::parse(v)
            .ok_or_else(|| serde::de::Error::custom(format!("Failed to parse JMAP header '{}'", v)))
    }
}

impl<'de> Deserialize<'de> for Header {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(HeaderVisitor)
    }
}

impl HeaderForm {
    pub fn parse(value: &str) -> Option<HeaderForm> {
        match value {
            "asText" => Some(HeaderForm::Text),
            "asAddresses" => Some(HeaderForm::Addresses),
            "asGroupedAddresses" => Some(HeaderForm::GroupedAddresses),
            "asMessageIds" => Some(HeaderForm::MessageIds),
            "asDate" => Some(HeaderForm::Date),
            "asURLs" => Some(HeaderForm::URLs),
            _ => None,
        }
    }
}

impl Header {
    pub fn as_raw(name: impl Into<String>, all: bool) -> Header {
        Header {
            name: name.into(),
            form: HeaderForm::Raw,
            all,
        }
    }

    pub fn as_text(name: impl Into<String>, all: bool) -> Header {
        Header {
            name: name.into(),
            form: HeaderForm::Text,
            all,
        }
    }

    pub fn as_addresses(name: impl Into<String>, all: bool) -> Header {
        Header {
            name: name.into(),
            form: HeaderForm::Addresses,
            all,
        }
    }

    pub fn as_grouped_addresses(name: impl Into<String>, all: bool) -> Header {
        Header {
            name: name.into(),
            form: HeaderForm::GroupedAddresses,
            all,
        }
    }

    pub fn as_message_ids(name: impl Into<String>, all: bool) -> Header {
        Header {
            name: name.into(),
            form: HeaderForm::MessageIds,
            all,
        }
    }

    pub fn as_date(name: impl Into<String>, all: bool) -> Header {
        Header {
            name: name.into(),
            form: HeaderForm::Date,
            all,
        }
    }

    pub fn as_urls(name: impl Into<String>, all: bool) -> Header {
        Header {
            name: name.into(),
            form: HeaderForm::URLs,
            all,
        }
    }

    pub fn parse(value: &str) -> Option<Header> {
        let mut all = false;
        let mut form = HeaderForm::Raw;
        let mut header = None;
        for (pos, part) in value.split(':').enumerate() {
            match pos {
                0 if part == "header" => (),
                1 => {
                    header = part.into();
                }
                2 | 3 if part == "all" => all = true,
                2 => {
                    form = HeaderForm::parse(part)?;
                }
                _ => return None,
            }
        }
        Header {
            name: header?.to_string(),
            form,
            all,
        }
        .into()
    }
}

impl Display for Header {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "header:")?;
        self.name.fmt(f)?;
        self.form.fmt(f)?;
        if self.all {
            write!(f, ":all")
        } else {
            Ok(())
        }
    }
}

impl Display for HeaderForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeaderForm::Raw => Ok(()),
            HeaderForm::Text => write!(f, ":asText"),
            HeaderForm::Addresses => write!(f, ":asAddresses"),
            HeaderForm::GroupedAddresses => write!(f, ":asGroupedAddresses"),
            HeaderForm::MessageIds => write!(f, ":asMessageIds"),
            HeaderForm::Date => write!(f, ":asDate"),
            HeaderForm::URLs => write!(f, ":asURLs"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BodyProperty {
    PartId,
    BlobId,
    Size,
    Headers,
    Name,
    Type,
    Charset,
    Disposition,
    Cid,
    Language,
    Location,
    SubParts,
    Header(Header),
}

impl BodyProperty {
    fn parse(value: &str) -> Option<BodyProperty> {
        match value {
            "partId" => Some(BodyProperty::PartId),
            "blobId" => Some(BodyProperty::BlobId),
            "size" => Some(BodyProperty::Size),
            "name" => Some(BodyProperty::Name),
            "type" => Some(BodyProperty::Type),
            "charset" => Some(BodyProperty::Charset),
            "headers" => Some(BodyProperty::Headers),
            "disposition" => Some(BodyProperty::Disposition),
            "cid" => Some(BodyProperty::Cid),
            "language" => Some(BodyProperty::Language),
            "location" => Some(BodyProperty::Location),
            "subParts" => Some(BodyProperty::SubParts),
            _ if value.starts_with("header:") => Some(BodyProperty::Header(Header::parse(value)?)),
            _ => None,
        }
    }
}

impl Display for BodyProperty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BodyProperty::PartId => write!(f, "partId"),
            BodyProperty::BlobId => write!(f, "blobId"),
            BodyProperty::Size => write!(f, "size"),
            BodyProperty::Name => write!(f, "name"),
            BodyProperty::Type => write!(f, "type"),
            BodyProperty::Charset => write!(f, "charset"),
            BodyProperty::Header(header) => header.fmt(f),
            BodyProperty::Headers => write!(f, "headers"),
            BodyProperty::Disposition => write!(f, "disposition"),
            BodyProperty::Cid => write!(f, "cid"),
            BodyProperty::Language => write!(f, "language"),
            BodyProperty::Location => write!(f, "location"),
            BodyProperty::SubParts => write!(f, "subParts"),
        }
    }
}

impl Serialize for BodyProperty {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

struct BodyPropertyVisitor;

impl<'de> Visitor<'de> for BodyPropertyVisitor {
    type Value = BodyProperty;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a valid JMAP body property")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        BodyProperty::parse(v).ok_or_else(|| {
            serde::de::Error::custom(format!("Failed to parse JMAP body property '{}'", v))
        })
    }
}

impl<'de> Deserialize<'de> for BodyProperty {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(BodyPropertyVisitor)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailCapabilities {
    #[serde(rename = "maxMailboxesPerEmail")]
    max_mailboxes_per_email: Option<usize>,

    #[serde(rename = "maxMailboxDepth")]
    max_mailbox_depth: usize,

    #[serde(rename = "maxSizeMailboxName")]
    max_size_mailbox_name: usize,

    #[serde(rename = "maxSizeAttachmentsPerEmail")]
    max_size_attachments_per_email: usize,

    #[serde(rename = "emailQuerySortOptions")]
    email_query_sort_options: Vec<String>,

    #[serde(rename = "mayCreateTopLevelMailbox")]
    may_create_top_level_mailbox: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionCapabilities {
    #[serde(rename = "maxDelayedSend")]
    max_delayed_send: usize,

    #[serde(rename = "submissionExtensions")]
    submission_extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct QueryArguments {
    #[serde(rename = "collapseThreads")]
    #[serde(skip_serializing_if = "Option::is_none")]
    collapse_threads: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct GetArguments {
    #[serde(rename = "bodyProperties")]
    #[serde(skip_serializing_if = "Option::is_none")]
    body_properties: Option<Vec<BodyProperty>>,

    #[serde(rename = "fetchTextBodyValues")]
    #[serde(skip_serializing_if = "Option::is_none")]
    fetch_text_body_values: Option<bool>,

    #[serde(rename = "fetchHTMLBodyValues")]
    #[serde(skip_serializing_if = "Option::is_none")]
    fetch_html_body_values: Option<bool>,

    #[serde(rename = "fetchAllBodyValues")]
    #[serde(skip_serializing_if = "Option::is_none")]
    fetch_all_body_values: Option<bool>,

    #[serde(rename = "maxBodyValueBytes")]
    #[serde(skip_serializing_if = "Option::is_none")]
    max_body_value_bytes: Option<usize>,
}

impl QueryArguments {
    pub fn collapse_threads(&mut self, collapse_threads: bool) {
        self.collapse_threads = collapse_threads.into();
    }
}

impl GetArguments {
    pub fn body_properties(
        &mut self,
        body_properties: impl IntoIterator<Item = BodyProperty>,
    ) -> &mut Self {
        self.body_properties = Some(body_properties.into_iter().collect());
        self
    }

    pub fn fetch_text_body_values(&mut self, fetch_text_body_values: bool) -> &mut Self {
        self.fetch_text_body_values = fetch_text_body_values.into();
        self
    }

    pub fn fetch_html_body_values(&mut self, fetch_html_body_values: bool) -> &mut Self {
        self.fetch_html_body_values = fetch_html_body_values.into();
        self
    }

    pub fn fetch_all_body_values(&mut self, fetch_all_body_values: bool) -> &mut Self {
        self.fetch_all_body_values = fetch_all_body_values.into();
        self
    }

    pub fn max_body_value_bytes(&mut self, max_body_value_bytes: usize) -> &mut Self {
        self.max_body_value_bytes = max_body_value_bytes.into();
        self
    }
}

impl MailCapabilities {
    pub fn max_mailboxes_per_email(&self) -> Option<usize> {
        self.max_mailboxes_per_email
    }

    pub fn max_mailbox_depth(&self) -> usize {
        self.max_mailbox_depth
    }

    pub fn max_size_mailbox_name(&self) -> usize {
        self.max_size_mailbox_name
    }

    pub fn max_size_attachments_per_email(&self) -> usize {
        self.max_size_attachments_per_email
    }

    pub fn email_query_sort_options(&self) -> &[String] {
        &self.email_query_sort_options
    }

    pub fn may_create_top_level_mailbox(&self) -> bool {
        self.may_create_top_level_mailbox
    }
}

impl SubmissionCapabilities {
    pub fn max_delayed_send(&self) -> usize {
        self.max_delayed_send
    }

    pub fn submission_extensions(&self) -> &[String] {
        &self.submission_extensions
    }
}

#[cfg(feature = "debug")]
use std::collections::BTreeMap;

#[cfg(feature = "debug")]
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TestEmail {
    #[serde(rename = "mailboxIds")]
    pub mailbox_ids: Option<BTreeMap<String, bool>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<BTreeMap<String, bool>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<usize>,

    #[serde(rename = "receivedAt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received_at: Option<DateTime<Utc>>,

    #[serde(rename = "messageId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<Vec<String>>,

    #[serde(rename = "inReplyTo")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub references: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender: Option<Vec<EmailAddress>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<Vec<EmailAddress>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Vec<EmailAddress>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cc: Option<Vec<EmailAddress>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub bcc: Option<Vec<EmailAddress>>,

    #[serde(rename = "replyTo")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Vec<EmailAddress>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,

    #[serde(rename = "sentAt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<DateTime<Utc>>,

    #[serde(rename = "bodyStructure")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_structure: Option<Box<EmailBodyPart>>,

    #[serde(rename = "bodyValues")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_values: Option<BTreeMap<String, EmailBodyValue>>,

    #[serde(rename = "textBody")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_body: Option<Vec<EmailBodyPart>>,

    #[serde(rename = "htmlBody")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html_body: Option<Vec<EmailBodyPart>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<EmailBodyPart>>,

    #[serde(rename = "hasAttachment")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_attachment: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,

    #[serde(flatten)]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<Header, Option<HeaderValue>>,
}

#[cfg(feature = "debug")]
impl From<Email> for TestEmail {
    fn from(email: Email) -> Self {
        TestEmail {
            mailbox_ids: email.mailbox_ids.map(|ids| ids.into_iter().collect()),
            keywords: email
                .keywords
                .map(|keywords| keywords.into_iter().collect()),
            size: email.size,
            received_at: email.received_at,
            message_id: email.message_id,
            in_reply_to: email.in_reply_to,
            references: email.references,
            sender: email.sender,
            from: email.from,
            to: email.to,
            cc: email.cc,
            bcc: email.bcc,
            reply_to: email.reply_to,
            subject: email.subject,
            sent_at: email.sent_at,
            body_structure: email.body_structure,
            body_values: email
                .body_values
                .map(|body_values| body_values.into_iter().collect()),
            text_body: email.text_body,
            html_body: email.html_body,
            attachments: email.attachments,
            has_attachment: email.has_attachment,
            preview: email.preview,
            headers: email.headers.into_iter().collect(),
        }
    }
}
