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

use serde::Serialize;

use crate::{
    core::query::{self, QueryObject},
    Set,
};

use super::{Mailbox, QueryArguments, Role};

#[derive(Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum Filter {
    ParentId {
        #[serde(rename = "parentId")]
        value: Option<String>,
    },
    Name {
        #[serde(rename = "name")]
        value: String,
    },
    Role {
        #[serde(rename = "role")]
        value: Option<Role>,
    },
    HasAnyRole {
        #[serde(rename = "hasAnyRole")]
        value: bool,
    },
    IsSubscribed {
        #[serde(rename = "isSubscribed")]
        value: bool,
    },
}

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "property")]
pub enum Comparator {
    #[serde(rename = "name")]
    Name,
    #[serde(rename = "sortOrder")]
    SortOrder,
    #[serde(rename = "parentId")]
    ParentId,
}

impl Filter {
    pub fn parent_id(value: Option<impl Into<String>>) -> Self {
        Filter::ParentId {
            value: value.map(Into::into),
        }
    }

    pub fn name(value: impl Into<String>) -> Self {
        Filter::Name {
            value: value.into(),
        }
    }

    pub fn role(value: Role) -> Self {
        Filter::Role {
            value: if !matches!(value, Role::None) {
                value.into()
            } else {
                None
            },
        }
    }

    pub fn has_any_role(value: bool) -> Self {
        Filter::HasAnyRole { value }
    }

    pub fn is_subscribed(value: bool) -> Self {
        Filter::IsSubscribed { value }
    }
}

impl Comparator {
    pub fn name() -> query::Comparator<Comparator> {
        query::Comparator::new(Comparator::Name)
    }

    pub fn sort_order() -> query::Comparator<Comparator> {
        query::Comparator::new(Comparator::SortOrder)
    }

    pub fn parent_id() -> query::Comparator<Comparator> {
        query::Comparator::new(Comparator::ParentId)
    }
}

impl QueryArguments {
    pub fn sort_as_tree(&mut self, value: bool) -> &mut Self {
        self.sort_as_tree = value;
        self
    }

    pub fn filter_as_tree(&mut self, value: bool) -> &mut Self {
        self.filter_as_tree = value;
        self
    }
}

impl QueryObject for Mailbox<Set> {
    type QueryArguments = QueryArguments;

    type Filter = Filter;

    type Sort = Comparator;
}
