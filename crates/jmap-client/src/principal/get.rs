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

use super::{Principal, Type, ACL, DKIM};
use crate::{core::get::GetObject, Get, Set};
use ahash::AHashMap;

impl Principal<Get> {
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn take_id(&mut self) -> String {
        self.id.take().unwrap_or_default()
    }

    pub fn ptype(&self) -> Option<&Type> {
        self.ptype.as_ref()
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn timezone(&self) -> Option<&str> {
        self.timezone.as_deref()
    }

    pub fn secret(&self) -> Option<&str> {
        self.secret.as_deref()
    }

    pub fn picture(&self) -> Option<&str> {
        self.picture.as_deref()
    }

    pub fn quota(&self) -> Option<u32> {
        self.quota
    }

    pub fn capabilities(&self) -> Option<&[String]> {
        self.capabilities.as_deref()
    }

    pub fn aliases(&self) -> Option<&[String]> {
        self.aliases.as_deref()
    }

    pub fn members(&self) -> Option<&[String]> {
        self.members.as_deref()
    }

    pub fn dkim(&self) -> Option<&DKIM> {
        self.dkim.as_ref()
    }

    pub fn acl(&self) -> Option<&AHashMap<String, Vec<ACL>>> {
        self.acl.as_ref()
    }
}

impl GetObject for Principal<Set> {
    type GetArguments = ();
}

impl GetObject for Principal<Get> {
    type GetArguments = ();
}
