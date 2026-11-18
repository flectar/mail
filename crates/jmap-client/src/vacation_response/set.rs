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

use crate::{
    core::set::{from_timestamp, SetObject},
    Get, Set,
};

use super::VacationResponse;

impl VacationResponse<Set> {
    pub fn is_enabled(&mut self, is_enabled: bool) -> &mut Self {
        self.is_enabled = Some(is_enabled);
        self
    }

    pub fn from_date(&mut self, from_date: Option<i64>) -> &mut Self {
        self.from_date = from_date.map(from_timestamp);
        self
    }

    pub fn to_date(&mut self, to_date: Option<i64>) -> &mut Self {
        self.to_date = to_date.map(from_timestamp);
        self
    }

    pub fn subject(&mut self, subject: Option<impl Into<String>>) -> &mut Self {
        self.subject = subject.map(|s| s.into());
        self
    }

    pub fn text_body(&mut self, text_body: Option<impl Into<String>>) -> &mut Self {
        self.text_body = text_body.map(|s| s.into());
        self
    }

    pub fn html_body(&mut self, html_body: Option<impl Into<String>>) -> &mut Self {
        self.html_body = html_body.map(|s| s.into());
        self
    }
}

impl SetObject for VacationResponse<Set> {
    type SetArguments = ();

    fn new(_create_id: Option<usize>) -> Self {
        VacationResponse {
            _create_id,
            _state: Default::default(),
            id: None,
            is_enabled: None,
            from_date: from_timestamp(0).into(),
            to_date: from_timestamp(0).into(),
            subject: "".to_string().into(),
            text_body: "".to_string().into(),
            html_body: "".to_string().into(),
        }
    }

    fn create_id(&self) -> Option<String> {
        self._create_id.map(|id| format!("c{}", id))
    }
}

impl SetObject for VacationResponse<Get> {
    type SetArguments = ();

    fn new(_create_id: Option<usize>) -> Self {
        unimplemented!()
    }

    fn create_id(&self) -> Option<String> {
        None
    }
}
