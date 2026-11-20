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

use crate::{core::get::GetObject, Get, Set};

use super::VacationResponse;

impl VacationResponse<Get> {
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn is_enabled(&self) -> bool {
        self.is_enabled.unwrap_or(false)
    }

    pub fn from_date(&self) -> Option<i64> {
        self.from_date.as_ref().map(|dt| dt.timestamp())
    }

    pub fn to_date(&self) -> Option<i64> {
        self.to_date.as_ref().map(|dt| dt.timestamp())
    }

    pub fn subject(&self) -> Option<&str> {
        self.subject.as_deref()
    }

    pub fn text_body(&self) -> Option<&str> {
        self.text_body.as_deref()
    }

    pub fn html_body(&self) -> Option<&str> {
        self.html_body.as_deref()
    }
}

impl GetObject for VacationResponse<Set> {
    type GetArguments = ();
}

impl GetObject for VacationResponse<Get> {
    type GetArguments = ();
}
