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

use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::Method;

pub mod changes;
pub mod copy;
pub mod error;
pub mod get;
pub mod query;
pub mod query_changes;
pub mod request;
pub mod response;
pub mod session;
pub mod set;

pub struct RequestParams {
    pub account_id: String,
    pub method: Method,
    pub call_id: usize,
}

impl RequestParams {
    pub fn new(account_id: impl Into<String>, method: Method, call_id: usize) -> Self {
        Self {
            account_id: account_id.into(),
            method,
            call_id,
        }
    }
}

pub trait Object: Sized {
    type Property: Display + Serialize + for<'de> Deserialize<'de>;
    fn requires_account_id() -> bool;
}
