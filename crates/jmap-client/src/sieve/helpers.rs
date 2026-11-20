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
        query::{Comparator, Filter, QueryRequest, QueryResponse},
        request::{Arguments, Request},
        response::{SieveScriptGetResponse, SieveScriptSetResponse},
        set::{SetObject, SetRequest},
    },
    Method, Set, URI,
};

use super::{
    validate::{SieveScriptValidateRequest, SieveScriptValidateResponse},
    Property, SieveScript,
};

impl Client {
    #[maybe_async::maybe_async]
    pub async fn sieve_script_create(
        &self,
        name: impl Into<String>,
        script: impl Into<Vec<u8>>,
        activate: bool,
    ) -> crate::Result<SieveScript> {
        let blob_id = self.upload(None, script.into(), None).await?.take_blob_id();
        let mut request = self.build();
        let set_request = request.set_sieve_script();
        let id = set_request
            .create()
            .name(name)
            .blob_id(blob_id)
            .create_id()
            .unwrap();
        if activate {
            set_request
                .arguments()
                .on_success_activate_script(id.clone());
        }
        request
            .send_single::<SieveScriptSetResponse>()
            .await?
            .created(&id)
    }

    #[maybe_async::maybe_async]
    pub async fn sieve_script_replace(
        &self,
        id: &str,
        script: impl Into<Vec<u8>>,
        activate: bool,
    ) -> crate::Result<Option<SieveScript>> {
        let blob_id = self.upload(None, script.into(), None).await?.take_blob_id();
        let mut request = self.build();
        let set_request = request.set_sieve_script();
        set_request.update(id).blob_id(blob_id);
        if activate {
            set_request.arguments().on_success_activate_script_id(id);
        }
        request
            .send_single::<SieveScriptSetResponse>()
            .await?
            .updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn sieve_script_rename(
        &self,
        id: &str,
        name: impl Into<String>,
        activate: bool,
    ) -> crate::Result<Option<SieveScript>> {
        let mut request = self.build();
        let set_request = request.set_sieve_script();
        set_request.update(id).name(name);
        if activate {
            set_request.arguments().on_success_activate_script_id(id);
        }
        request
            .send_single::<SieveScriptSetResponse>()
            .await?
            .updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn sieve_script_activate(&self, id: &str) -> crate::Result<()> {
        let mut request = self.build();
        request
            .set_sieve_script()
            .arguments()
            .on_success_activate_script_id(id);
        request
            .send_single::<SieveScriptSetResponse>()
            .await?
            .unwrap_update_errors()
    }

    #[maybe_async::maybe_async]
    pub async fn sieve_script_deactivate(&self) -> crate::Result<()> {
        let mut request = self.build();
        request
            .set_sieve_script()
            .arguments()
            .on_success_deactivate_script(true);
        request
            .send_single::<SieveScriptSetResponse>()
            .await?
            .unwrap_update_errors()
    }

    #[maybe_async::maybe_async]
    pub async fn sieve_script_destroy(&self, id: &str) -> crate::Result<()> {
        let mut request = self.build();
        request.set_sieve_script().destroy([id]);
        request
            .send_single::<SieveScriptSetResponse>()
            .await?
            .destroyed(id)
    }

    #[maybe_async::maybe_async]
    pub async fn sieve_script_get(
        &self,
        id: &str,
        properties: Option<impl IntoIterator<Item = Property>>,
    ) -> crate::Result<Option<SieveScript>> {
        let mut request = self.build();
        let get_request = request.get_sieve_script().ids([id]);
        if let Some(properties) = properties {
            get_request.properties(properties);
        }
        request
            .send_single::<SieveScriptGetResponse>()
            .await
            .map(|mut r| r.take_list().pop())
    }

    #[maybe_async::maybe_async]
    pub async fn sieve_script_query(
        &self,
        filter: Option<impl Into<Filter<super::query::Filter>>>,
        sort: Option<impl IntoIterator<Item = Comparator<super::query::Comparator>>>,
    ) -> crate::Result<QueryResponse> {
        let mut request = self.build();
        let query_request = request.query_sieve_script();
        if let Some(filter) = filter {
            query_request.filter(filter);
        }
        if let Some(sort) = sort {
            query_request.sort(sort);
        }
        request.send_single::<QueryResponse>().await
    }

    #[maybe_async::maybe_async]
    pub async fn sieve_script_validate(&self, script: impl Into<Vec<u8>>) -> crate::Result<()> {
        let blob_id = self.upload(None, script.into(), None).await?.take_blob_id();
        let mut request = self.build();
        request.validate_sieve_script(blob_id);
        request
            .send_single::<SieveScriptValidateResponse>()
            .await?
            .unwrap_error()
    }
}

impl Request<'_> {
    pub fn get_sieve_script(&mut self) -> &mut GetRequest<SieveScript<Set>> {
        self.add_capability(URI::Sieve);
        self.add_method_call(
            Method::GetSieveScript,
            Arguments::sieve_script_get(self.params(Method::GetSieveScript)),
        )
        .sieve_script_get_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_get_sieve_script(self) -> crate::Result<SieveScriptGetResponse> {
        self.send_single().await
    }

    pub fn set_sieve_script(&mut self) -> &mut SetRequest<SieveScript<Set>> {
        self.add_capability(URI::Sieve);
        self.add_method_call(
            Method::SetSieveScript,
            Arguments::sieve_script_set(self.params(Method::SetSieveScript)),
        )
        .sieve_script_set_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_set_sieve_script(self) -> crate::Result<SieveScriptSetResponse> {
        self.send_single().await
    }

    pub fn validate_sieve_script(
        &mut self,
        blob_id: impl Into<String>,
    ) -> &mut SieveScriptValidateRequest {
        self.add_capability(URI::Sieve);
        self.add_method_call(
            Method::ValidateSieveScript,
            Arguments::sieve_script_validate(self.params(Method::ValidateSieveScript), blob_id),
        )
        .sieve_script_validate_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_validate_sieve_script(self) -> crate::Result<SieveScriptValidateResponse> {
        self.send_single().await
    }

    pub fn query_sieve_script(&mut self) -> &mut QueryRequest<SieveScript<Set>> {
        self.add_capability(URI::Sieve);
        self.add_method_call(
            Method::QuerySieveScript,
            Arguments::sieve_script_query(self.params(Method::QuerySieveScript)),
        )
        .sieve_script_query_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_query_sieve_script(self) -> crate::Result<QueryResponse> {
        self.send_single().await
    }
}
