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
        response::{EmailSubmissionGetResponse, EmailSubmissionSetResponse},
        set::{SetObject, SetRequest},
    },
    Get, Method, Set, URI,
};

use super::{Address, EmailSubmission, Property, UndoStatus};

impl Client {
    #[maybe_async::maybe_async]
    pub async fn email_submission_create(
        &self,
        email_id: impl Into<String>,
        identity_id: impl Into<String>,
    ) -> crate::Result<EmailSubmission<Get>> {
        let mut request = self.build();
        let id = request
            .set_email_submission()
            .create()
            .email_id(email_id)
            .identity_id(identity_id)
            .create_id()
            .unwrap();
        request
            .send_single::<EmailSubmissionSetResponse>()
            .await?
            .created(&id)
    }

    #[maybe_async::maybe_async]
    pub async fn email_submission_create_envelope<S, T, U>(
        &self,
        email_id: impl Into<String>,
        identity_id: impl Into<String>,
        mail_from: S,
        rcpt_to: T,
    ) -> crate::Result<EmailSubmission<Get>>
    where
        S: Into<Address>,
        T: IntoIterator<Item = U>,
        U: Into<Address>,
    {
        let mut request = self.build();
        let id = request
            .set_email_submission()
            .create()
            .email_id(email_id)
            .identity_id(identity_id)
            .envelope(mail_from, rcpt_to)
            .create_id()
            .unwrap();
        request
            .send_single::<EmailSubmissionSetResponse>()
            .await?
            .created(&id)
    }

    #[maybe_async::maybe_async]
    pub async fn email_submission_change_status(
        &self,
        id: &str,
        undo_status: UndoStatus,
    ) -> crate::Result<Option<EmailSubmission>> {
        let mut request = self.build();
        request
            .set_email_submission()
            .update(id)
            .undo_status(undo_status);
        request
            .send_single::<EmailSubmissionSetResponse>()
            .await?
            .updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn email_submission_destroy(&self, id: &str) -> crate::Result<()> {
        let mut request = self.build();
        request.set_email_submission().destroy([id]);
        request
            .send_single::<EmailSubmissionSetResponse>()
            .await?
            .destroyed(id)
    }

    #[maybe_async::maybe_async]
    pub async fn email_submission_get(
        &self,
        id: &str,
        properties: Option<Vec<Property>>,
    ) -> crate::Result<Option<EmailSubmission>> {
        let mut request = self.build();
        let get_request = request.get_email_submission().ids([id]);
        if let Some(properties) = properties {
            get_request.properties(properties);
        }
        request
            .send_single::<EmailSubmissionGetResponse>()
            .await
            .map(|mut r| r.take_list().pop())
    }

    #[maybe_async::maybe_async]
    pub async fn email_submission_query(
        &self,
        filter: Option<impl Into<Filter<super::query::Filter>>>,
        sort: Option<impl IntoIterator<Item = Comparator<super::query::Comparator>>>,
    ) -> crate::Result<QueryResponse> {
        let mut request = self.build();
        let query_request = request.query_email_submission();
        if let Some(filter) = filter {
            query_request.filter(filter);
        }
        if let Some(sort) = sort {
            query_request.sort(sort);
        }
        request.send_single::<QueryResponse>().await
    }

    #[maybe_async::maybe_async]
    pub async fn email_submission_changes(
        &self,
        since_state: impl Into<String>,
        max_changes: usize,
    ) -> crate::Result<ChangesResponse<EmailSubmission<Get>>> {
        let mut request = self.build();
        request
            .changes_email_submission(since_state)
            .max_changes(max_changes);
        request.send_single().await
    }
}

impl Request<'_> {
    pub fn get_email_submission(&mut self) -> &mut GetRequest<EmailSubmission<Set>> {
        self.add_capability(URI::Submission);
        self.add_method_call(
            Method::GetEmailSubmission,
            Arguments::email_submission_get(self.params(Method::GetEmailSubmission)),
        )
        .email_submission_get_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_get_email_submission(self) -> crate::Result<EmailSubmissionGetResponse> {
        self.send_single().await
    }

    pub fn changes_email_submission(
        &mut self,
        since_state: impl Into<String>,
    ) -> &mut ChangesRequest {
        self.add_capability(URI::Submission);
        self.add_method_call(
            Method::ChangesEmailSubmission,
            Arguments::changes(
                self.params(Method::ChangesEmailSubmission),
                since_state.into(),
            ),
        )
        .changes_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_changes_email_submission(
        self,
    ) -> crate::Result<ChangesResponse<EmailSubmission<Get>>> {
        self.send_single().await
    }

    pub fn query_email_submission(&mut self) -> &mut QueryRequest<EmailSubmission<Set>> {
        self.add_capability(URI::Submission);
        self.add_method_call(
            Method::QueryEmailSubmission,
            Arguments::email_submission_query(self.params(Method::QueryEmailSubmission)),
        )
        .email_submission_query_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_query_email_submission(self) -> crate::Result<QueryResponse> {
        self.send_single().await
    }

    pub fn query_email_submission_changes(
        &mut self,
        since_query_state: impl Into<String>,
    ) -> &mut QueryChangesRequest<EmailSubmission<Set>> {
        self.add_capability(URI::Submission);
        self.add_method_call(
            Method::QueryChangesEmailSubmission,
            Arguments::email_submission_query_changes(
                self.params(Method::QueryChangesEmailSubmission),
                since_query_state.into(),
            ),
        )
        .email_submission_query_changes_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_query_email_submission_changes(self) -> crate::Result<QueryChangesResponse> {
        self.send_single().await
    }

    pub fn set_email_submission(&mut self) -> &mut SetRequest<EmailSubmission<Set>> {
        self.add_capability(URI::Submission);
        self.add_method_call(
            Method::SetEmailSubmission,
            Arguments::email_submission_set(self.params(Method::SetEmailSubmission)),
        )
        .email_submission_set_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_set_email_submission(self) -> crate::Result<EmailSubmissionSetResponse> {
        self.send_single().await
    }
}
