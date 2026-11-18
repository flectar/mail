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
        changes::{ChangesRequest, ChangesResponse},
        get::GetRequest,
        request::{Arguments, Request},
        response::{IdentityGetResponse, IdentitySetResponse},
        set::{SetObject, SetRequest},
    },
    Get, Method, Set,
};

use super::{Identity, Property};

impl Client {
    #[maybe_async::maybe_async]
    pub async fn identity_create(
        &self,
        name: impl Into<String>,
        email: impl Into<String>,
    ) -> crate::Result<Identity> {
        let mut request = self.build();
        let id = request
            .set_identity()
            .create()
            .name(name)
            .email(email)
            .create_id()
            .unwrap();
        request
            .send_single::<IdentitySetResponse>()
            .await?
            .created(&id)
    }

    #[maybe_async::maybe_async]
    pub async fn identity_destroy(&self, id: &str) -> crate::Result<()> {
        let mut request = self.build();
        request.set_identity().destroy([id]);
        request
            .send_single::<IdentitySetResponse>()
            .await?
            .destroyed(id)
    }

    #[maybe_async::maybe_async]
    pub async fn identity_get(
        &self,
        id: &str,
        properties: Option<Vec<Property>>,
    ) -> crate::Result<Option<Identity>> {
        let mut request = self.build();
        let get_request = request.get_identity().ids([id]);
        if let Some(properties) = properties {
            get_request.properties(properties);
        }
        request
            .send_single::<IdentityGetResponse>()
            .await
            .map(|mut r| r.take_list().pop())
    }

    #[maybe_async::maybe_async]
    pub async fn identity_changes(
        &self,
        since_state: impl Into<String>,
        max_changes: usize,
    ) -> crate::Result<ChangesResponse<Identity<Get>>> {
        let mut request = self.build();
        request
            .changes_identity(since_state)
            .max_changes(max_changes);
        request.send_single().await
    }
}

impl Request<'_> {
    pub fn get_identity(&mut self) -> &mut GetRequest<Identity<Set>> {
        self.add_method_call(
            Method::GetIdentity,
            Arguments::identity_get(self.params(Method::GetIdentity)),
        )
        .identity_get_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_get_identity(self) -> crate::Result<IdentityGetResponse> {
        self.send_single().await
    }

    pub fn set_identity(&mut self) -> &mut SetRequest<Identity<Set>> {
        self.add_method_call(
            Method::SetIdentity,
            Arguments::identity_set(self.params(Method::SetIdentity)),
        )
        .identity_set_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_set_identity(self) -> crate::Result<IdentitySetResponse> {
        self.send_single().await
    }

    pub fn changes_identity(&mut self, since_state: impl Into<String>) -> &mut ChangesRequest {
        self.add_method_call(
            Method::ChangesIdentity,
            Arguments::changes(self.params(Method::ChangesIdentity), since_state.into()),
        )
        .changes_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_changes_identity(self) -> crate::Result<ChangesResponse<Identity<Get>>> {
        self.send_single().await
    }
}
