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

use crate::{core::set::SetObject, email::EmailAddress, Get, Set};

use super::Identity;

impl Identity<Set> {
    pub fn name(&mut self, name: impl Into<String>) -> &mut Self {
        self.name = Some(name.into());
        self
    }

    pub fn email(&mut self, email: impl Into<String>) -> &mut Self {
        self.email = Some(email.into());
        self
    }

    pub fn bcc<T, U>(&mut self, bcc: Option<T>) -> &mut Self
    where
        T: Iterator<Item = U>,
        U: Into<EmailAddress>,
    {
        self.bcc = bcc.map(|s| s.map(|s| s.into()).collect());
        self
    }

    pub fn reply_to<T, U>(&mut self, reply_to: Option<T>) -> &mut Self
    where
        T: Iterator<Item = U>,
        U: Into<EmailAddress>,
    {
        self.reply_to = reply_to.map(|s| s.map(|s| s.into()).collect());
        self
    }

    pub fn text_signature(&mut self, text_signature: impl Into<String>) -> &mut Self {
        self.text_signature = Some(text_signature.into());
        self
    }

    pub fn html_signature(&mut self, html_signature: impl Into<String>) -> &mut Self {
        self.html_signature = Some(html_signature.into());
        self
    }
}

impl SetObject for Identity<Set> {
    type SetArguments = ();

    fn new(_create_id: Option<usize>) -> Self {
        Identity {
            _create_id,
            _state: Default::default(),
            id: None,
            name: None,
            email: None,
            reply_to: Vec::with_capacity(0).into(),
            bcc: Vec::with_capacity(0).into(),
            text_signature: None,
            html_signature: None,
            may_delete: None,
        }
    }

    fn create_id(&self) -> Option<String> {
        self._create_id.map(|id| format!("c{}", id))
    }
}

impl SetObject for Identity<Get> {
    type SetArguments = ();

    fn new(_create_id: Option<usize>) -> Self {
        unimplemented!()
    }

    fn create_id(&self) -> Option<String> {
        None
    }
}
