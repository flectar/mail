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
        query::{Comparator, Filter, QueryRequest, QueryResponse},
        query_changes::{QueryChangesRequest, QueryChangesResponse},
        request::{Arguments, Request},
        response::{PrincipalGetResponse, PrincipalSetResponse},
        set::{SetObject, SetRequest},
    },
    Get, Method, Set,
};

use super::{Principal, Property, Type, DKIM};

impl Client {
    #[maybe_async::maybe_async]
    pub async fn individual_create(
        &self,
        email: impl Into<String>,
        secret: impl Into<String>,
        name: impl Into<String>,
    ) -> crate::Result<Principal> {
        let mut request = self.build();
        let id = request
            .set_principal()
            .create()
            .name(name)
            .secret(secret)
            .email(email)
            .ptype(Type::Individual)
            .create_id()
            .unwrap();
        request
            .send_single::<PrincipalSetResponse>()
            .await?
            .created(&id)
    }

    #[maybe_async::maybe_async]
    pub async fn domain_create(&self, name: impl Into<String>) -> crate::Result<Principal> {
        let mut request = self.build();
        let id = request
            .set_principal()
            .create()
            .name(name)
            .ptype(Type::Domain)
            .create_id()
            .unwrap();
        request
            .send_single::<PrincipalSetResponse>()
            .await?
            .created(&id)
    }

    #[maybe_async::maybe_async]
    pub async fn domain_enable_dkim(
        &self,
        id: &str,
        key: impl Into<String>,
        selector: impl Into<String>,
        expiration: Option<i64>,
    ) -> crate::Result<Option<Principal>> {
        let mut request = self.build();
        request
            .set_principal()
            .update(id)
            .secret(key)
            .dkim(DKIM::new(Some(selector), expiration));
        request
            .send_single::<PrincipalSetResponse>()
            .await?
            .updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn list_create(
        &self,
        email: impl Into<String>,
        name: impl Into<String>,
        members: impl IntoIterator<Item = impl Into<String>>,
    ) -> crate::Result<Principal> {
        let mut request = self.build();
        let id = request
            .set_principal()
            .create()
            .name(name)
            .email(email)
            .ptype(Type::List)
            .members(members.into())
            .create_id()
            .unwrap();
        request
            .send_single::<PrincipalSetResponse>()
            .await?
            .created(&id)
    }

    #[maybe_async::maybe_async]
    pub async fn group_create(
        &self,
        email: impl Into<String>,
        name: impl Into<String>,
        members: impl IntoIterator<Item = impl Into<String>>,
    ) -> crate::Result<Principal> {
        let mut request = self.build();
        let id = request
            .set_principal()
            .create()
            .name(name)
            .email(email)
            .ptype(Type::Group)
            .members(members.into())
            .create_id()
            .unwrap();
        request
            .send_single::<PrincipalSetResponse>()
            .await?
            .created(&id)
    }

    #[maybe_async::maybe_async]
    pub async fn principal_set_name(
        &self,
        id: &str,
        name: impl Into<String>,
    ) -> crate::Result<Option<Principal>> {
        let mut request = self.build();
        request.set_principal().update(id).name(name);
        request
            .send_single::<PrincipalSetResponse>()
            .await?
            .updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn principal_set_secret(
        &self,
        id: &str,
        secret: impl Into<String>,
    ) -> crate::Result<Option<Principal>> {
        let mut request = self.build();
        request.set_principal().update(id).secret(secret);
        request
            .send_single::<PrincipalSetResponse>()
            .await?
            .updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn principal_set_email(
        &self,
        id: &str,
        email: impl Into<String>,
    ) -> crate::Result<Option<Principal>> {
        let mut request = self.build();
        request.set_principal().update(id).email(email);
        request
            .send_single::<PrincipalSetResponse>()
            .await?
            .updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn principal_set_timezone(
        &self,
        id: &str,
        timezone: Option<impl Into<String>>,
    ) -> crate::Result<Option<Principal>> {
        let mut request = self.build();
        request.set_principal().update(id).timezone(timezone);
        request
            .send_single::<PrincipalSetResponse>()
            .await?
            .updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn principal_set_members(
        &self,
        id: &str,
        members: Option<impl IntoIterator<Item = impl Into<String>>>,
    ) -> crate::Result<Option<Principal>> {
        let mut request = self.build();
        request.set_principal().update(id).members(members);
        request
            .send_single::<PrincipalSetResponse>()
            .await?
            .updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn principal_set_aliases(
        &self,
        id: &str,
        aliases: Option<impl IntoIterator<Item = impl Into<String>>>,
    ) -> crate::Result<Option<Principal>> {
        let mut request = self.build();
        request.set_principal().update(id).aliases(aliases);
        request
            .send_single::<PrincipalSetResponse>()
            .await?
            .updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn principal_set_capabilities(
        &self,
        id: &str,
        capabilities: Option<impl IntoIterator<Item = impl Into<String>>>,
    ) -> crate::Result<Option<Principal>> {
        let mut request = self.build();
        request
            .set_principal()
            .update(id)
            .capabilities(capabilities);
        request
            .send_single::<PrincipalSetResponse>()
            .await?
            .updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn principal_destroy(&self, id: &str) -> crate::Result<()> {
        let mut request = self.build();
        request.set_principal().destroy([id]).arguments();
        request
            .send_single::<PrincipalSetResponse>()
            .await?
            .destroyed(id)
    }

    #[maybe_async::maybe_async]
    pub async fn principal_get(
        &self,
        id: &str,
        properties: Option<impl IntoIterator<Item = Property>>,
    ) -> crate::Result<Option<Principal>> {
        let mut request = self.build();
        let get_request = request.get_principal().ids([id]);
        if let Some(properties) = properties {
            get_request.properties(properties);
        }
        request
            .send_single::<PrincipalGetResponse>()
            .await
            .map(|mut r| r.take_list().pop())
    }

    #[maybe_async::maybe_async]
    pub async fn principal_query(
        &self,
        filter: Option<impl Into<Filter<super::query::Filter>>>,
        sort: Option<impl IntoIterator<Item = Comparator<super::query::Comparator>>>,
    ) -> crate::Result<QueryResponse> {
        let mut request = self.build();
        let query_request = request.query_principal();
        if let Some(filter) = filter {
            query_request.filter(filter);
        }
        if let Some(sort) = sort {
            query_request.sort(sort);
        }
        request.send_single::<QueryResponse>().await
    }

    #[maybe_async::maybe_async]
    pub async fn principal_changes(
        &self,
        since_state: impl Into<String>,
        max_changes: usize,
    ) -> crate::Result<ChangesResponse<Principal<Get>>> {
        let mut request = self.build();
        request
            .changes_principal(since_state)
            .max_changes(max_changes);
        request.send_single().await
    }
}

impl Request<'_> {
    pub fn get_principal(&mut self) -> &mut GetRequest<Principal<Set>> {
        self.add_method_call(
            Method::GetPrincipal,
            Arguments::principal_get(self.params(Method::GetPrincipal)),
        )
        .principal_get_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_get_principal(self) -> crate::Result<PrincipalGetResponse> {
        self.send_single().await
    }

    pub fn changes_principal(&mut self, since_state: impl Into<String>) -> &mut ChangesRequest {
        self.add_method_call(
            Method::ChangesPrincipal,
            Arguments::changes(self.params(Method::ChangesPrincipal), since_state.into()),
        )
        .changes_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_changes_principal(self) -> crate::Result<ChangesResponse<Principal<Get>>> {
        self.send_single().await
    }

    pub fn query_principal(&mut self) -> &mut QueryRequest<Principal<Set>> {
        self.add_method_call(
            Method::QueryPrincipal,
            Arguments::principal_query(self.params(Method::QueryPrincipal)),
        )
        .principal_query_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_query_principal(self) -> crate::Result<QueryResponse> {
        self.send_single().await
    }

    pub fn query_principal_changes(
        &mut self,
        since_query_state: impl Into<String>,
    ) -> &mut QueryChangesRequest<Principal<Set>> {
        self.add_method_call(
            Method::QueryChangesPrincipal,
            Arguments::principal_query_changes(
                self.params(Method::QueryChangesPrincipal),
                since_query_state.into(),
            ),
        )
        .principal_query_changes_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_query_principal_changes(self) -> crate::Result<QueryChangesResponse> {
        self.send_single().await
    }

    pub fn set_principal(&mut self) -> &mut SetRequest<Principal<Set>> {
        self.add_method_call(
            Method::SetPrincipal,
            Arguments::principal_set(self.params(Method::SetPrincipal)),
        )
        .principal_set_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_set_principal(self) -> crate::Result<PrincipalSetResponse> {
        self.send_single().await
    }
}
