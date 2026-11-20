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
pub mod query;
pub mod set;

use crate::core::changes::ChangesObject;
use crate::core::set::{map_not_set, string_not_set};
use crate::core::Object;
use crate::mailbox::set::role_not_set;
use crate::principal::ACL;
use crate::{Get, Set};
use ahash::AHashMap;
use serde::{Deserialize, Serialize};
use std::fmt::Display;

#[derive(Debug, Clone, Serialize, Default)]
pub struct SetArguments {
    #[serde(rename = "onDestroyRemoveEmails")]
    #[serde(skip_serializing_if = "Option::is_none")]
    on_destroy_remove_emails: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct QueryArguments {
    #[serde(rename = "sortAsTree")]
    sort_as_tree: bool,
    #[serde(rename = "filterAsTree")]
    filter_as_tree: bool,
}

#[derive(Debug, Deserialize, Default)]
pub struct ChangesResponse {
    #[serde(rename = "updatedProperties")]
    updated_properties: Option<Vec<Property>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mailbox<State = Get> {
    #[serde(skip)]
    _create_id: Option<usize>,

    #[serde(skip)]
    _state: std::marker::PhantomData<State>,

    #[serde(rename = "id")]
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,

    #[serde(rename = "name")]
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    #[serde(rename = "parentId")]
    #[serde(skip_serializing_if = "string_not_set")]
    parent_id: Option<String>,

    #[serde(rename = "role")]
    #[serde(skip_serializing_if = "role_not_set")]
    role: Option<Role>,

    #[serde(rename = "sortOrder")]
    #[serde(skip_serializing_if = "Option::is_none")]
    sort_order: Option<u32>,

    #[serde(rename = "totalEmails")]
    #[serde(skip_serializing_if = "Option::is_none")]
    total_emails: Option<usize>,

    #[serde(rename = "unreadEmails")]
    #[serde(skip_serializing_if = "Option::is_none")]
    unread_emails: Option<usize>,

    #[serde(rename = "totalThreads")]
    #[serde(skip_serializing_if = "Option::is_none")]
    total_threads: Option<usize>,

    #[serde(rename = "unreadThreads")]
    #[serde(skip_serializing_if = "Option::is_none")]
    unread_threads: Option<usize>,

    #[serde(rename = "myRights")]
    #[serde(skip_serializing_if = "Option::is_none")]
    my_rights: Option<MailboxRights>,

    #[serde(rename = "isSubscribed")]
    #[serde(skip_serializing_if = "Option::is_none")]
    is_subscribed: Option<bool>,

    #[serde(rename = "shareWith")]
    #[serde(skip_serializing_if = "map_not_set")]
    share_with: Option<AHashMap<String, AHashMap<ACL, bool>>>,

    #[serde(flatten)]
    #[serde(skip_deserializing)]
    #[serde(skip_serializing_if = "Option::is_none")]
    acl_patch: Option<AHashMap<String, ACLPatch>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum ACLPatch {
    Replace(AHashMap<ACL, bool>),
    Set(bool),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Role {
    Archive,
    Drafts,
    Important,
    Inbox,
    Junk,
    Sent,
    Trash,
    Other(String),
    #[default]
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailboxRights {
    #[serde(rename = "mayReadItems")]
    #[serde(default)]
    may_read_items: bool,

    #[serde(rename = "mayAddItems")]
    #[serde(default)]
    may_add_items: bool,

    #[serde(rename = "mayRemoveItems")]
    #[serde(default)]
    may_remove_items: bool,

    #[serde(rename = "maySetSeen")]
    #[serde(default)]
    may_set_seen: bool,

    #[serde(rename = "maySetKeywords")]
    #[serde(default)]
    may_set_keywords: bool,

    #[serde(rename = "mayCreateChild")]
    #[serde(default)]
    may_create_child: bool,

    #[serde(rename = "mayRename")]
    #[serde(default)]
    may_rename: bool,

    #[serde(rename = "mayDelete")]
    #[serde(default)]
    may_delete: bool,

    #[serde(rename = "maySubmit")]
    #[serde(default)]
    may_submit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Copy)]
pub enum Property {
    #[serde(rename = "id")]
    Id,
    #[serde(rename = "name")]
    Name,
    #[serde(rename = "parentId")]
    ParentId,
    #[serde(rename = "role")]
    Role,
    #[serde(rename = "sortOrder")]
    SortOrder,
    #[serde(rename = "totalEmails")]
    TotalEmails,
    #[serde(rename = "unreadEmails")]
    UnreadEmails,
    #[serde(rename = "totalThreads")]
    TotalThreads,
    #[serde(rename = "unreadThreads")]
    UnreadThreads,
    #[serde(rename = "myRights")]
    MyRights,
    #[serde(rename = "isSubscribed")]
    IsSubscribed,
    #[serde(rename = "shareWith")]
    ShareWith,
}

impl Property {
    pub fn is_count(&self) -> bool {
        matches!(
            self,
            Property::TotalEmails
                | Property::UnreadEmails
                | Property::TotalThreads
                | Property::UnreadThreads
        )
    }
}

impl Display for Property {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Property::Id => write!(f, "id"),
            Property::Name => write!(f, "name"),
            Property::ParentId => write!(f, "parentId"),
            Property::Role => write!(f, "role"),
            Property::SortOrder => write!(f, "sortOrder"),
            Property::TotalEmails => write!(f, "totalEmails"),
            Property::UnreadEmails => write!(f, "unreadEmails"),
            Property::TotalThreads => write!(f, "totalThreads"),
            Property::UnreadThreads => write!(f, "unreadThreads"),
            Property::MyRights => write!(f, "myRights"),
            Property::IsSubscribed => write!(f, "isSubscribed"),
            Property::ShareWith => write!(f, "shareWith"),
        }
    }
}

impl ChangesResponse {
    pub fn updated_properties(&self) -> Option<&[Property]> {
        self.updated_properties.as_deref()
    }
}

impl Object for Mailbox<Set> {
    type Property = Property;

    fn requires_account_id() -> bool {
        true
    }
}

impl Object for Mailbox<Get> {
    type Property = Property;

    fn requires_account_id() -> bool {
        true
    }
}

impl ChangesObject for Mailbox<Set> {
    type ChangesResponse = ChangesResponse;
}

impl ChangesObject for Mailbox<Get> {
    type ChangesResponse = ChangesResponse;
}

impl<'de> Deserialize<'de> for Role {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match <&str>::deserialize(deserializer)?
            .to_ascii_lowercase()
            .as_str()
        {
            "inbox" => Ok(Role::Inbox),
            "sent" => Ok(Role::Sent),
            "trash" => Ok(Role::Trash),
            "drafts" => Ok(Role::Drafts),
            "junk" => Ok(Role::Junk),
            "archive" => Ok(Role::Archive),
            "important" => Ok(Role::Important),
            other => Ok(Role::Other(other.to_string())),
        }
    }
}

impl Serialize for Role {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self {
            Role::Inbox => "inbox",
            Role::Sent => "sent",
            Role::Trash => "trash",
            Role::Drafts => "drafts",
            Role::Junk => "junk",
            Role::Archive => "archive",
            Role::Important => "important",
            Role::Other(other) => other,
            Role::None => "",
        })
    }
}
