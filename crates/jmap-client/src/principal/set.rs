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

use super::{Principal, Property, Type, ACL, DKIM};
use crate::{core::set::SetObject, Get, Set};
use ahash::AHashMap;

impl Principal<Set> {
    pub fn name(&mut self, name: impl Into<String>) -> &mut Self {
        self.name = name.into().into();
        self
    }

    pub fn description(&mut self, description: Option<impl Into<String>>) -> &mut Self {
        self.description = description.map(|s| s.into());
        self
    }

    pub fn email(&mut self, email: impl Into<String>) -> &mut Self {
        self.email = email.into().into();
        self
    }

    pub fn secret(&mut self, secret: impl Into<String>) -> &mut Self {
        self.secret = secret.into().into();
        self
    }

    pub fn timezone(&mut self, timezone: Option<impl Into<String>>) -> &mut Self {
        self.timezone = timezone.map(|s| s.into());
        self
    }

    pub fn picture(&mut self, picture: Option<impl Into<String>>) -> &mut Self {
        self.picture = picture.map(|s| s.into());
        self
    }

    pub fn quota(&mut self, quota: Option<u32>) -> &mut Self {
        self.quota = quota;
        self
    }

    pub fn ptype(&mut self, ptype: Type) -> &mut Self {
        self.ptype = ptype.into();
        self
    }

    pub fn dkim(&mut self, dkim: DKIM) -> &mut Self {
        self.dkim = dkim.into();
        self
    }

    pub fn acl(&mut self, acl: Option<AHashMap<String, Vec<ACL>>>) -> &mut Self {
        self.acl = acl;
        self
    }

    pub fn aliases<T, U>(&mut self, aliases: Option<T>) -> &mut Self
    where
        T: IntoIterator<Item = U>,
        U: Into<String>,
    {
        self.aliases = aliases.map(|l| l.into_iter().map(|v| v.into()).collect());
        self
    }

    pub fn alias(&mut self, alias: &str, set: bool) -> &mut Self {
        self.property_patch
            .get_or_insert_with(AHashMap::new)
            .insert(format!("{}/{}", Property::Aliases, alias), set);
        self
    }

    pub fn capabilities<T, U>(&mut self, capabilities: Option<T>) -> &mut Self
    where
        T: IntoIterator<Item = U>,
        U: Into<String>,
    {
        self.capabilities = capabilities.map(|l| l.into_iter().map(|v| v.into()).collect());
        self
    }

    pub fn members<T, U>(&mut self, members: Option<T>) -> &mut Self
    where
        T: IntoIterator<Item = U>,
        U: Into<String>,
    {
        self.members = members.map(|l| l.into_iter().map(|v| v.into()).collect());
        self
    }

    pub fn member(&mut self, member: &str, set: bool) -> &mut Self {
        self.property_patch
            .get_or_insert_with(AHashMap::new)
            .insert(format!("{}/{}", Property::Members, member), set);
        self
    }
}

impl SetObject for Principal<Set> {
    type SetArguments = ();

    fn new(_create_id: Option<usize>) -> Self {
        Principal {
            _create_id,
            _state: Default::default(),
            id: None,
            ptype: None,
            name: "".to_string().into(),
            description: "".to_string().into(),
            email: "".to_string().into(),
            timezone: "".to_string().into(),
            capabilities: Vec::with_capacity(0).into(),
            aliases: Vec::with_capacity(0).into(),
            secret: "".to_string().into(),
            dkim: None,
            quota: None,
            picture: "".to_string().into(),
            members: Vec::with_capacity(0).into(),
            acl: AHashMap::with_capacity(0).into(),
            property_patch: None,
        }
    }

    fn create_id(&self) -> Option<String> {
        self._create_id.map(|id| format!("c{}", id))
    }
}

impl SetObject for Principal<Get> {
    type SetArguments = ();

    fn new(_create_id: Option<usize>) -> Self {
        unimplemented!()
    }

    fn create_id(&self) -> Option<String> {
        None
    }
}
