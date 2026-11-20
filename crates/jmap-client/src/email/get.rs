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

use crate::{core::get::GetObject, Get, Set};

use super::{
    Email, EmailAddress, EmailAddressGroup, EmailBodyPart, EmailBodyValue, EmailHeader,
    GetArguments, Header, HeaderValue,
};

impl Email<Get> {
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn take_id(&mut self) -> String {
        self.id.take().unwrap_or_default()
    }

    pub fn blob_id(&self) -> Option<&str> {
        self.blob_id.as_deref()
    }

    pub fn take_blob_id(&mut self) -> String {
        self.blob_id.take().unwrap_or_default()
    }

    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    pub fn take_thread_id(&mut self) -> Option<String> {
        self.thread_id.take()
    }

    pub fn mailbox_ids(&self) -> Vec<&str> {
        self.mailbox_ids
            .as_ref()
            .map(|m| {
                m.iter()
                    .filter(|(_, v)| **v)
                    .map(|(k, _)| k.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn keywords(&self) -> Vec<&str> {
        self.keywords
            .as_ref()
            .map(|k| {
                k.iter()
                    .filter(|(_, v)| **v)
                    .map(|(k, _)| k.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn size(&self) -> usize {
        self.size.unwrap_or(0)
    }

    pub fn received_at(&self) -> Option<i64> {
        self.received_at.as_ref().map(|r| r.timestamp())
    }

    pub fn message_id(&self) -> Option<&[String]> {
        self.message_id.as_deref()
    }

    pub fn in_reply_to(&self) -> Option<&[String]> {
        self.in_reply_to.as_deref()
    }

    pub fn references(&self) -> Option<&[String]> {
        self.references.as_deref()
    }

    pub fn sender(&self) -> Option<&[EmailAddress]> {
        self.sender.as_deref()
    }

    pub fn take_sender(&mut self) -> Option<Vec<EmailAddress>> {
        self.sender.take()
    }

    pub fn from(&self) -> Option<&[EmailAddress]> {
        self.from.as_deref()
    }

    pub fn take_from(&mut self) -> Option<Vec<EmailAddress>> {
        self.from.take()
    }

    pub fn reply_to(&self) -> Option<&[EmailAddress]> {
        self.reply_to.as_deref()
    }

    pub fn take_reply_to(&mut self) -> Option<Vec<EmailAddress>> {
        self.reply_to.take()
    }

    pub fn to(&self) -> Option<&[EmailAddress]> {
        self.to.as_deref()
    }

    pub fn take_to(&mut self) -> Option<Vec<EmailAddress>> {
        self.to.take()
    }

    pub fn cc(&self) -> Option<&[EmailAddress]> {
        self.cc.as_deref()
    }

    pub fn take_cc(&mut self) -> Option<Vec<EmailAddress>> {
        self.cc.take()
    }

    pub fn bcc(&self) -> Option<&[EmailAddress]> {
        self.bcc.as_deref()
    }

    pub fn take_bcc(&mut self) -> Option<Vec<EmailAddress>> {
        self.bcc.take()
    }

    pub fn subject(&self) -> Option<&str> {
        self.subject.as_deref()
    }

    pub fn take_subject(&mut self) -> Option<String> {
        self.subject.take()
    }

    pub fn sent_at(&self) -> Option<i64> {
        self.sent_at.as_ref().map(|v| v.timestamp())
    }

    pub fn body_structure(&self) -> Option<&EmailBodyPart> {
        self.body_structure.as_deref()
    }

    pub fn body_value(&self, id: &str) -> Option<&EmailBodyValue> {
        self.body_values.as_ref().and_then(|v| v.get(id))
    }

    pub fn text_body(&self) -> Option<&[EmailBodyPart]> {
        self.text_body.as_deref()
    }

    pub fn html_body(&self) -> Option<&[EmailBodyPart]> {
        self.html_body.as_deref()
    }

    pub fn attachments(&self) -> Option<&[EmailBodyPart]> {
        self.attachments.as_deref()
    }

    pub fn has_attachment(&self) -> bool {
        *self.has_attachment.as_ref().unwrap_or(&false)
    }

    pub fn header(&self, id: &Header) -> Option<&HeaderValue> {
        self.headers.get(id).and_then(|v| v.as_ref())
    }

    pub fn has_header(&self, id: &Header) -> bool {
        self.headers.contains_key(id)
    }

    pub fn preview(&self) -> Option<&str> {
        self.preview.as_deref()
    }

    pub fn take_preview(&mut self) -> Option<String> {
        self.preview.take()
    }

    #[cfg(feature = "debug")]
    pub fn into_test(self) -> super::TestEmail {
        self.into()
    }
}

impl EmailBodyPart<Get> {
    pub fn part_id(&self) -> Option<&str> {
        self.part_id.as_deref()
    }

    pub fn blob_id(&self) -> Option<&str> {
        self.blob_id.as_deref()
    }

    pub fn size(&self) -> usize {
        *self.size.as_ref().unwrap_or(&0)
    }

    pub fn headers(&self) -> Option<&[EmailHeader]> {
        self.headers.as_deref()
    }

    pub fn header(&self, id: &Header) -> Option<&HeaderValue> {
        self.header.as_ref().and_then(|v| v.get(id))
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn charset(&self) -> Option<&str> {
        self.charset.as_deref()
    }

    pub fn content_type(&self) -> Option<&str> {
        self.type_.as_deref()
    }

    pub fn content_disposition(&self) -> Option<&str> {
        self.disposition.as_deref()
    }

    pub fn content_id(&self) -> Option<&str> {
        self.cid.as_deref()
    }

    pub fn content_language(&self) -> Option<&[String]> {
        self.language.as_deref()
    }

    pub fn content_location(&self) -> Option<&str> {
        self.location.as_deref()
    }

    pub fn sub_parts(&self) -> Option<&[EmailBodyPart]> {
        self.sub_parts.as_deref()
    }
}

impl EmailBodyValue<Get> {
    pub fn value(&self) -> &str {
        self.value.as_str()
    }

    pub fn is_encoding_problem(&self) -> bool {
        self.is_encoding_problem.unwrap_or(false)
    }

    pub fn is_truncated(&self) -> bool {
        self.is_truncated.unwrap_or(false)
    }
}

impl EmailAddress<Get> {
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn email(&self) -> &str {
        self.email.as_str()
    }

    pub fn unwrap(self) -> (String, Option<String>) {
        (self.email, self.name)
    }
}

impl EmailAddressGroup<Get> {
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn addresses(&self) -> &[EmailAddress] {
        self.addresses.as_ref()
    }
}

impl EmailHeader<Get> {
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub fn value(&self) -> &str {
        self.value.as_str()
    }
}

impl GetObject for Email<Set> {
    type GetArguments = GetArguments;
}

impl GetObject for Email<Get> {
    type GetArguments = GetArguments;
}
