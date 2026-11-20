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

use super::{Principal, Type};

#[derive(Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum Filter {
    Email {
        #[serde(rename = "email")]
        value: String,
    },
    Name {
        #[serde(rename = "name")]
        value: String,
    },
    DomainName {
        #[serde(rename = "domainName")]
        value: String,
    },
    Text {
        #[serde(rename = "text")]
        value: String,
    },
    Type {
        #[serde(rename = "type")]
        value: Type,
    },
    Timezone {
        #[serde(rename = "timezone")]
        value: String,
    },
    Members {
        #[serde(rename = "members")]
        value: String,
    },
    QuotaLt {
        #[serde(rename = "quotaLowerThan")]
        value: u32,
    },
    QuotaGt {
        #[serde(rename = "quotaGreaterThan")]
        value: u32,
    },
}

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "property")]
pub enum Comparator {
    #[serde(rename = "type")]
    Type,
    #[serde(rename = "name")]
    Name,
    #[serde(rename = "email")]
    Email,
}

impl Filter {
    pub fn name(value: impl Into<String>) -> Self {
        Filter::Name {
            value: value.into(),
        }
    }

    pub fn domain_name(value: impl Into<String>) -> Self {
        Filter::DomainName {
            value: value.into(),
        }
    }

    pub fn email(value: impl Into<String>) -> Self {
        Filter::Email {
            value: value.into(),
        }
    }

    pub fn text(value: impl Into<String>) -> Self {
        Filter::Text {
            value: value.into(),
        }
    }

    pub fn timezone(value: impl Into<String>) -> Self {
        Filter::Timezone {
            value: value.into(),
        }
    }

    pub fn members(value: impl Into<String>) -> Self {
        Filter::Members {
            value: value.into(),
        }
    }

    pub fn ptype(value: Type) -> Self {
        Filter::Type { value }
    }

    pub fn quota_lower_than(value: u32) -> Self {
        Filter::QuotaLt { value }
    }

    pub fn quota_greater_than(value: u32) -> Self {
        Filter::QuotaGt { value }
    }
}

impl Comparator {
    pub fn name() -> query::Comparator<Comparator> {
        query::Comparator::new(Comparator::Name)
    }

    pub fn email() -> query::Comparator<Comparator> {
        query::Comparator::new(Comparator::Email)
    }

    pub fn ptype() -> query::Comparator<Comparator> {
        query::Comparator::new(Comparator::Type)
    }
}

impl QueryObject for Principal<Set> {
    type QueryArguments = ();

    type Filter = Filter;

    type Sort = Comparator;
}
