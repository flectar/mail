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
        copy::CopyRequest,
        get::GetRequest,
        query::{Comparator, Filter, QueryRequest, QueryResponse},
        query_changes::{QueryChangesRequest, QueryChangesResponse},
        request::{Arguments, Request},
        response::{EmailCopyResponse, EmailGetResponse, EmailSetResponse},
        set::SetRequest,
    },
    Get, Method, Set,
};

use super::{
    import::{EmailImportRequest, EmailImportResponse},
    parse::{EmailParseRequest, EmailParseResponse},
    search_snippet::{SearchSnippetGetRequest, SearchSnippetGetResponse},
    BodyProperty, Email, Property,
};

impl Client {
    #[maybe_async::maybe_async]
    pub async fn email_import<T, U, V, W>(
        &self,
        raw_message: Vec<u8>,
        mailbox_ids: T,
        keywords: Option<V>,
        received_at: Option<i64>,
    ) -> crate::Result<Email>
    where
        T: IntoIterator<Item = U>,
        U: Into<String>,
        V: IntoIterator<Item = W>,
        W: Into<String>,
    {
        self.email_import_account(
            self.default_account_id(),
            raw_message,
            mailbox_ids,
            keywords,
            received_at,
        )
        .await
    }

    #[maybe_async::maybe_async]
    pub async fn email_import_account<T, U, V, W>(
        &self,
        account_id: &str,
        raw_message: Vec<u8>,
        mailbox_ids: T,
        keywords: Option<V>,
        received_at: Option<i64>,
    ) -> crate::Result<Email>
    where
        T: IntoIterator<Item = U>,
        U: Into<String>,
        V: IntoIterator<Item = W>,
        W: Into<String>,
    {
        let blob_id = self
            .upload(account_id.into(), raw_message, None)
            .await?
            .take_blob_id();
        let mut request = self.build();
        let import_request = request
            .import_email()
            .account_id(account_id)
            .email(blob_id)
            .mailbox_ids(mailbox_ids);

        if let Some(keywords) = keywords {
            import_request.keywords(keywords);
        }

        if let Some(received_at) = received_at {
            import_request.received_at(received_at);
        }

        let id = import_request.create_id();
        request
            .send_single::<EmailImportResponse>()
            .await?
            .created(&id)
    }

    #[maybe_async::maybe_async]
    pub async fn email_set_mailbox(
        &self,
        id: &str,
        mailbox_id: &str,
        set: bool,
    ) -> crate::Result<Option<Email>> {
        let mut request = self.build();
        request.set_email().update(id).mailbox_id(mailbox_id, set);
        request.send_single::<EmailSetResponse>().await?.updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn email_set_mailboxes<T, U>(
        &self,
        id: &str,
        mailbox_ids: T,
    ) -> crate::Result<Option<Email>>
    where
        T: IntoIterator<Item = U>,
        U: Into<String>,
    {
        let mut request = self.build();
        request.set_email().update(id).mailbox_ids(mailbox_ids);
        request.send_single::<EmailSetResponse>().await?.updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn email_set_keyword(
        &self,
        id: &str,
        keyword: &str,
        set: bool,
    ) -> crate::Result<Option<Email>> {
        let mut request = self.build();
        request.set_email().update(id).keyword(keyword, set);
        request.send_single::<EmailSetResponse>().await?.updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn email_set_keywords<T, U>(
        &self,
        id: &str,
        keywords: T,
    ) -> crate::Result<Option<Email>>
    where
        T: IntoIterator<Item = U>,
        U: Into<String>,
    {
        let mut request = self.build();
        request.set_email().update(id).keywords(keywords);
        request.send_single::<EmailSetResponse>().await?.updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn email_destroy(&self, id: &str) -> crate::Result<()> {
        let mut request = self.build();
        request.set_email().destroy([id]);
        request
            .send_single::<EmailSetResponse>()
            .await?
            .destroyed(id)
    }

    #[maybe_async::maybe_async]
    pub async fn email_get(
        &self,
        id: &str,
        properties: Option<impl IntoIterator<Item = Property>>,
    ) -> crate::Result<Option<Email<Get>>> {
        let mut request = self.build();
        let get_request = request.get_email().ids([id]);
        if let Some(properties) = properties {
            get_request.properties(properties);
        }
        request
            .send_single::<EmailGetResponse>()
            .await
            .map(|mut r| r.take_list().pop())
    }

    #[maybe_async::maybe_async]
    pub async fn email_changes(
        &self,
        since_state: impl Into<String>,
        max_changes: Option<usize>,
    ) -> crate::Result<ChangesResponse<Email<Get>>> {
        let mut request = self.build();
        let changes_request = request.changes_email(since_state);
        if let Some(max_changes) = max_changes {
            changes_request.max_changes(max_changes);
        }
        request.send_single().await
    }

    #[maybe_async::maybe_async]
    pub async fn email_query(
        &self,
        filter: Option<impl Into<Filter<super::query::Filter>>>,
        sort: Option<impl IntoIterator<Item = Comparator<super::query::Comparator>>>,
    ) -> crate::Result<QueryResponse> {
        let mut request = self.build();
        let query_request = request.query_email();
        if let Some(filter) = filter {
            query_request.filter(filter);
        }
        if let Some(sort) = sort {
            query_request.sort(sort);
        }
        request.send_single::<QueryResponse>().await
    }

    #[maybe_async::maybe_async]
    pub async fn email_query_changes(
        &self,
        since_query_state: impl Into<String>,
        filter: Option<impl Into<Filter<super::query::Filter>>>,
    ) -> crate::Result<QueryChangesResponse> {
        let mut request = self.build();
        let query_request = request.query_email_changes(since_query_state);
        if let Some(filter) = filter {
            query_request.filter(filter);
        }
        request.send_single::<QueryChangesResponse>().await
    }

    #[maybe_async::maybe_async]
    pub async fn email_parse(
        &self,
        blob_id: &str,
        properties: Option<impl IntoIterator<Item = Property>>,
        body_properties: Option<impl IntoIterator<Item = BodyProperty>>,
        max_body_value_bytes: Option<usize>,
    ) -> crate::Result<Email> {
        let mut request = self.build();
        let parse_request = request.parse_email().blob_ids([blob_id]);
        if let Some(properties) = properties {
            parse_request.properties(properties);
        }

        if let Some(body_properties) = body_properties {
            parse_request.body_properties(body_properties);
        }

        if let Some(max_body_value_bytes) = max_body_value_bytes {
            parse_request
                .fetch_all_body_values(true)
                .max_body_value_bytes(max_body_value_bytes);
        }

        request
            .send_single::<EmailParseResponse>()
            .await
            .and_then(|mut r| r.parsed(blob_id))
    }

    #[maybe_async::maybe_async]
    pub async fn email_copy<T, U, V, W>(
        &self,
        from_account_id: impl Into<String>,
        id: impl Into<String>,
        mailbox_ids: T,
        keywords: Option<V>,
        received_at: Option<i64>,
    ) -> crate::Result<Email>
    where
        T: IntoIterator<Item = U>,
        U: Into<String>,
        V: IntoIterator<Item = W>,
        W: Into<String>,
    {
        let id = id.into();
        let mut request = self.build();
        let email = request
            .copy_email(from_account_id)
            .create(id.clone())
            .mailbox_ids(mailbox_ids);

        if let Some(keywords) = keywords {
            email.keywords(keywords);
        }

        if let Some(received_at) = received_at {
            email.received_at(received_at);
        }

        request
            .send_single::<EmailCopyResponse>()
            .await?
            .created(&id)
    }

    #[maybe_async::maybe_async]
    pub async fn search_snippet_get(
        &self,
        filter: Option<impl Into<Filter<super::query::Filter>>>,
        email_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> crate::Result<SearchSnippetGetResponse> {
        let mut request = self.build();
        let snippet_request = request.get_search_snippet();
        if let Some(filter) = filter {
            snippet_request.filter(filter);
        }
        snippet_request.email_ids(email_ids);
        request.send_single::<SearchSnippetGetResponse>().await
    }
}

impl Request<'_> {
    pub fn get_email(&mut self) -> &mut GetRequest<Email<Set>> {
        self.add_method_call(
            Method::GetEmail,
            Arguments::email_get(self.params(Method::GetEmail)),
        )
        .email_get_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_get_email(self) -> crate::Result<EmailGetResponse> {
        self.send_single().await
    }

    pub fn changes_email(&mut self, since_state: impl Into<String>) -> &mut ChangesRequest {
        self.add_method_call(
            Method::ChangesEmail,
            Arguments::changes(self.params(Method::ChangesEmail), since_state.into()),
        )
        .changes_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_changes_email(self) -> crate::Result<ChangesResponse<Email<Get>>> {
        self.send_single().await
    }

    pub fn query_email(&mut self) -> &mut QueryRequest<Email<Set>> {
        self.add_method_call(
            Method::QueryEmail,
            Arguments::email_query(self.params(Method::QueryEmail)),
        )
        .email_query_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_query_email(self) -> crate::Result<QueryResponse> {
        self.send_single().await
    }

    pub fn query_email_changes(
        &mut self,
        since_query_state: impl Into<String>,
    ) -> &mut QueryChangesRequest<Email<Set>> {
        self.add_method_call(
            Method::QueryChangesEmail,
            Arguments::email_query_changes(
                self.params(Method::QueryChangesEmail),
                since_query_state.into(),
            ),
        )
        .email_query_changes_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_query_email_changes(self) -> crate::Result<QueryChangesResponse> {
        self.send_single().await
    }

    pub fn set_email(&mut self) -> &mut SetRequest<Email<Set>> {
        self.add_method_call(
            Method::SetEmail,
            Arguments::email_set(self.params(Method::SetEmail)),
        )
        .email_set_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_set_email(self) -> crate::Result<EmailSetResponse> {
        self.send_single().await
    }

    pub fn copy_email(
        &mut self,
        from_account_id: impl Into<String>,
    ) -> &mut CopyRequest<Email<Set>> {
        self.add_method_call(
            Method::CopyEmail,
            Arguments::email_copy(self.params(Method::CopyEmail), from_account_id.into()),
        )
        .email_copy_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_copy_email(self) -> crate::Result<EmailCopyResponse> {
        self.send_single().await
    }

    pub fn import_email(&mut self) -> &mut EmailImportRequest {
        self.add_method_call(
            Method::ImportEmail,
            Arguments::email_import(self.params(Method::ImportEmail)),
        )
        .email_import_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_import_email(self) -> crate::Result<EmailImportResponse> {
        self.send_single().await
    }

    pub fn parse_email(&mut self) -> &mut EmailParseRequest {
        self.add_method_call(
            Method::ParseEmail,
            Arguments::email_parse(self.params(Method::ParseEmail)),
        )
        .email_parse_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_parse_email(self) -> crate::Result<EmailParseResponse> {
        self.send_single().await
    }

    pub fn get_search_snippet(&mut self) -> &mut SearchSnippetGetRequest {
        self.add_method_call(
            Method::GetSearchSnippet,
            Arguments::search_snippet_get(self.params(Method::GetSearchSnippet)),
        )
        .search_snippet_get_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_get_search_snippet(self) -> crate::Result<SearchSnippetGetResponse> {
        self.send_single().await
    }
}
