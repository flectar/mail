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

pub mod parser;
pub mod stream;

use crate::{core::session::URLParser, CalendarAlert, DataType};
use ahash::AHashMap;
use serde::{Deserialize, Serialize};

pub enum URLParameter {
    Types,
    CloseAfter,
    Ping,
}

impl URLParser for URLParameter {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "types" => Some(URLParameter::Types),
            "closeafter" => Some(URLParameter::CloseAfter),
            "ping" => Some(URLParameter::Ping),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushNotification {
    StateChange(Changes),
    CalendarAlert(CalendarAlert),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    id: Option<String>,
    changes: AHashMap<String, AHashMap<DataType, String>>,
}

impl Changes {
    pub fn new(id: Option<String>, changes: AHashMap<String, AHashMap<DataType, String>>) -> Self {
        Self { id, changes }
    }

    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn account_changes(&mut self, account_id: &str) -> Option<AHashMap<DataType, String>> {
        self.changes.remove(account_id)
    }

    pub fn changed_accounts(&self) -> impl Iterator<Item = &String> {
        self.changes.keys()
    }

    pub fn changes(&self, account_id: &str) -> Option<impl Iterator<Item = (&DataType, &String)>> {
        self.changes.get(account_id).map(|changes| changes.iter())
    }

    pub fn has_type(&self, type_: DataType) -> bool {
        self.changes
            .values()
            .any(|changes| changes.contains_key(&type_))
    }

    pub fn into_inner(self) -> AHashMap<String, AHashMap<DataType, String>> {
        self.changes
    }

    pub fn is_empty(&self) -> bool {
        !self.changes.values().any(|changes| !changes.is_empty())
    }
}
