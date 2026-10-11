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

use super::SieveScript;

#[derive(Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum Filter {
    Name {
        #[serde(rename = "name")]
        value: String,
    },
    IsActive {
        #[serde(rename = "isActive")]
        value: bool,
    },
}

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "property")]
pub enum Comparator {
    #[serde(rename = "name")]
    Name,
    #[serde(rename = "isActive")]
    IsActive,
}

impl Filter {
    pub fn name(value: impl Into<String>) -> Self {
        Filter::Name {
            value: value.into(),
        }
    }

    pub fn is_active(value: bool) -> Self {
        Filter::IsActive { value }
    }
}

impl Comparator {
    pub fn name() -> query::Comparator<Comparator> {
        query::Comparator::new(Comparator::Name)
    }

    pub fn is_active() -> query::Comparator<Comparator> {
        query::Comparator::new(Comparator::IsActive)
    }
}

impl QueryObject for SieveScript<Set> {
    type QueryArguments = ();

    type Filter = Filter;

    type Sort = Comparator;
}
