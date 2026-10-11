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

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{
    core::{
        query::{self, QueryObject},
        set::from_timestamp,
    },
    Set,
};

use super::{EmailSubmission, UndoStatus};

#[derive(Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum Filter {
    IdentityIds {
        #[serde(rename = "identityIds")]
        value: Vec<String>,
    },
    EmailIds {
        #[serde(rename = "emailIds")]
        value: Vec<String>,
    },
    ThreadIds {
        #[serde(rename = "threadIds")]
        value: Vec<String>,
    },
    UndoStatus {
        #[serde(rename = "undoStatus")]
        value: UndoStatus,
    },
    Before {
        #[serde(rename = "before")]
        value: DateTime<Utc>,
    },
    After {
        #[serde(rename = "after")]
        value: DateTime<Utc>,
    },
}

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "property")]
pub enum Comparator {
    #[serde(rename = "emailId")]
    EmailId,
    #[serde(rename = "threadId")]
    ThreadId,
    #[serde(rename = "sentAt")]
    SentAt,
}

impl Filter {
    pub fn identity_ids<U, V>(value: U) -> Self
    where
        U: IntoIterator<Item = V>,
        V: Into<String>,
    {
        Filter::IdentityIds {
            value: value.into_iter().map(|v| v.into()).collect(),
        }
    }

    pub fn email_ids<U, V>(value: U) -> Self
    where
        U: IntoIterator<Item = V>,
        V: Into<String>,
    {
        Filter::EmailIds {
            value: value.into_iter().map(|v| v.into()).collect(),
        }
    }

    pub fn thread_ids<U, V>(value: U) -> Self
    where
        U: IntoIterator<Item = V>,
        V: Into<String>,
    {
        Filter::ThreadIds {
            value: value.into_iter().map(|v| v.into()).collect(),
        }
    }

    pub fn undo_status(value: UndoStatus) -> Self {
        Filter::UndoStatus { value }
    }

    pub fn before(value: i64) -> Self {
        Filter::Before {
            value: from_timestamp(value),
        }
    }

    pub fn after(value: i64) -> Self {
        Filter::After {
            value: from_timestamp(value),
        }
    }
}

impl Comparator {
    pub fn email_id() -> query::Comparator<Comparator> {
        query::Comparator::new(Comparator::EmailId)
    }

    pub fn thread_id() -> query::Comparator<Comparator> {
        query::Comparator::new(Comparator::ThreadId)
    }

    pub fn sent_at() -> query::Comparator<Comparator> {
        query::Comparator::new(Comparator::SentAt)
    }
}

impl QueryObject for EmailSubmission<Set> {
    type QueryArguments = ();

    type Filter = Filter;

    type Sort = Comparator;
}
