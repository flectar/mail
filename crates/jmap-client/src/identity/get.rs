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

use crate::{core::get::GetObject, email::EmailAddress, Get, Set};

use super::Identity;

impl Identity<Get> {
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn take_id(&mut self) -> String {
        self.id.take().unwrap_or_default()
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }

    pub fn reply_to(&self) -> Option<&[EmailAddress]> {
        self.reply_to.as_deref()
    }

    pub fn bcc(&self) -> Option<&[EmailAddress]> {
        self.bcc.as_deref()
    }

    pub fn text_signature(&self) -> Option<&str> {
        self.text_signature.as_deref()
    }

    pub fn html_signature(&self) -> Option<&str> {
        self.html_signature.as_deref()
    }

    pub fn may_delete(&self) -> bool {
        self.may_delete.unwrap_or(false)
    }
}

impl GetObject for Identity<Set> {
    type GetArguments = ();
}

impl GetObject for Identity<Get> {
    type GetArguments = ();
}
