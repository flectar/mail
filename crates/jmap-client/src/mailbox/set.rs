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

use super::{ACLPatch, Mailbox, Role, SetArguments};
use crate::{core::set::SetObject, principal::ACL, Get, Set};
use ahash::AHashMap;

impl Mailbox<Set> {
    pub fn name(&mut self, name: impl Into<String>) -> &mut Self {
        self.name = Some(name.into());
        self
    }

    pub fn parent_id(&mut self, parent_id: Option<impl Into<String>>) -> &mut Self {
        self.parent_id = parent_id.map(|s| s.into());
        self
    }

    pub fn parent_id_ref(&mut self, parent_id_ref: &str) -> &mut Self {
        self.parent_id = format!("#{}", parent_id_ref).into();
        self
    }

    pub fn role(&mut self, role: Role) -> &mut Self {
        if !matches!(role, Role::None) {
            self.role = Some(role);
        } else {
            self.role = None;
        }
        self
    }

    pub fn sort_order(&mut self, sort_order: u32) -> &mut Self {
        self.sort_order = sort_order.into();
        self
    }

    pub fn is_subscribed(&mut self, is_subscribed: bool) -> &mut Self {
        self.is_subscribed = is_subscribed.into();
        self
    }

    pub fn acls<T, U, V>(&mut self, acls: T) -> &mut Self
    where
        T: IntoIterator<Item = (U, V)>,
        U: Into<String>,
        V: IntoIterator<Item = ACL>,
    {
        self.share_with = Some(
            acls.into_iter()
                .map(|(id, acls)| (id.into(), acls.into_iter().map(|acl| (acl, true)).collect()))
                .collect(),
        );
        self
    }

    pub fn acl(&mut self, id: &str, acl: impl IntoIterator<Item = ACL>) -> &mut Self {
        self.acl_patch.get_or_insert_with(AHashMap::new).insert(
            format!("shareWith/{}", id),
            ACLPatch::Replace(acl.into_iter().map(|acl| (acl, true)).collect()),
        );
        self
    }

    pub fn acl_set(&mut self, id: &str, acl: ACL, set: bool) -> &mut Self {
        self.acl_patch
            .get_or_insert_with(AHashMap::new)
            .insert(format!("shareWith/{}/{}", id, acl), ACLPatch::Set(set));
        self
    }
}

pub fn role_not_set(role: &Option<Role>) -> bool {
    matches!(role, Some(Role::None))
}

impl SetObject for Mailbox<Set> {
    type SetArguments = SetArguments;

    fn new(_create_id: Option<usize>) -> Self {
        Mailbox {
            _create_id,
            _state: Default::default(),
            id: None,
            name: None,
            parent_id: "".to_string().into(),
            role: Role::None.into(),
            sort_order: None,
            total_emails: None,
            unread_emails: None,
            total_threads: None,
            unread_threads: None,
            my_rights: None,
            is_subscribed: None,
            share_with: AHashMap::with_capacity(0).into(),
            acl_patch: None,
        }
    }

    fn create_id(&self) -> Option<String> {
        self._create_id.map(|id| format!("c{}", id))
    }
}

impl SetObject for Mailbox<Get> {
    type SetArguments = SetArguments;

    fn new(_create_id: Option<usize>) -> Self {
        unimplemented!()
    }

    fn create_id(&self) -> Option<String> {
        None
    }
}

impl SetArguments {
    pub fn on_destroy_remove_emails(&mut self, value: bool) -> &mut Self {
        self.on_destroy_remove_emails = value.into();
        self
    }
}
