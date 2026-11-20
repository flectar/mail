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
    client::Client,
    core::{
        get::GetRequest,
        request::{Arguments, Request},
        response::{VacationResponseGetResponse, VacationResponseSetResponse},
        set::{SetObject, SetRequest},
    },
    Method, Set, URI,
};

use super::{Property, VacationResponse};

impl Client {
    #[maybe_async::maybe_async]
    pub async fn vacation_response_create(
        &self,
        subject: impl Into<String>,
        text_body: Option<impl Into<String>>,
        html_body: Option<impl Into<String>>,
    ) -> crate::Result<VacationResponse> {
        let mut request = self.build();
        let created_id = request
            .set_vacation_response()
            .create()
            .is_enabled(true)
            .subject(Some(subject))
            .text_body(text_body)
            .html_body(html_body)
            .create_id()
            .unwrap();

        request
            .send_single::<VacationResponseSetResponse>()
            .await?
            .created(&created_id)
    }

    #[maybe_async::maybe_async]
    pub async fn vacation_response_enable(
        &self,
        subject: impl Into<String>,
        text_body: Option<impl Into<String>>,
        html_body: Option<impl Into<String>>,
    ) -> crate::Result<Option<VacationResponse>> {
        let mut request = self.build();
        request
            .set_vacation_response()
            .update("singleton")
            .is_enabled(true)
            .subject(Some(subject))
            .text_body(text_body)
            .html_body(html_body);

        request
            .send_single::<VacationResponseSetResponse>()
            .await?
            .updated("singleton")
    }

    #[maybe_async::maybe_async]
    pub async fn vacation_response_disable(&self) -> crate::Result<Option<VacationResponse>> {
        let mut request = self.build();
        request
            .set_vacation_response()
            .update("singleton")
            .is_enabled(false);

        request
            .send_single::<VacationResponseSetResponse>()
            .await?
            .updated("singleton")
    }

    #[maybe_async::maybe_async]
    pub async fn vacation_response_set_dates(
        &self,
        from_date: Option<i64>,
        to_date: Option<i64>,
    ) -> crate::Result<Option<VacationResponse>> {
        let mut request = self.build();
        request
            .set_vacation_response()
            .update("singleton")
            .is_enabled(true)
            .from_date(from_date)
            .to_date(to_date);

        request
            .send_single::<VacationResponseSetResponse>()
            .await?
            .updated("singleton")
    }

    #[maybe_async::maybe_async]
    pub async fn vacation_response_get(
        &self,
        properties: Option<impl IntoIterator<Item = Property>>,
    ) -> crate::Result<Option<VacationResponse>> {
        let mut request = self.build();
        let get_request = request.get_vacation_response().ids(["singleton"]);
        if let Some(properties) = properties {
            get_request.properties(properties);
        }
        request
            .send_single::<VacationResponseGetResponse>()
            .await
            .map(|mut r| r.take_list().pop())
    }

    #[maybe_async::maybe_async]
    pub async fn vacation_response_destroy(&self) -> crate::Result<()> {
        let mut request = self.build();
        request.set_vacation_response().destroy(["singleton"]);
        request
            .send_single::<VacationResponseSetResponse>()
            .await?
            .destroyed("singleton")
    }
}

impl Request<'_> {
    pub fn get_vacation_response(&mut self) -> &mut GetRequest<VacationResponse<Set>> {
        self.add_capability(URI::VacationResponse);
        self.add_method_call(
            Method::GetVacationResponse,
            Arguments::vacation_response_get(self.params(Method::GetVacationResponse)),
        )
        .vacation_response_get_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_get_vacation_response(self) -> crate::Result<VacationResponseGetResponse> {
        self.send_single().await
    }

    pub fn set_vacation_response(&mut self) -> &mut SetRequest<VacationResponse<Set>> {
        self.add_capability(URI::VacationResponse);
        self.add_method_call(
            Method::SetVacationResponse,
            Arguments::vacation_response_set(self.params(Method::GetVacationResponse)),
        )
        .vacation_response_set_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_set_vacation_response(self) -> crate::Result<VacationResponseSetResponse> {
        self.send_single().await
    }
}
