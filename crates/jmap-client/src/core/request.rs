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
    blob::copy::CopyBlobRequest,
    client::Client,
    email::{
        import::EmailImportRequest, parse::EmailParseRequest,
        search_snippet::SearchSnippetGetRequest, Email,
    },
    email_submission::EmailSubmission,
    identity::Identity,
    mailbox::Mailbox,
    principal::Principal,
    push_subscription::PushSubscription,
    sieve::{validate::SieveScriptValidateRequest, SieveScript},
    thread::Thread,
    vacation_response::VacationResponse,
    Error, Method, Set, URI,
};
use ahash::AHashMap;
use serde::{de::DeserializeOwned, Serialize};

use super::{
    changes::ChangesRequest,
    copy::CopyRequest,
    get::GetRequest,
    query::QueryRequest,
    query_changes::QueryChangesRequest,
    response::{Response, SingleMethodResponse, TaggedMethodResponse},
    set::SetRequest,
    RequestParams,
};

#[derive(Serialize)]
pub struct Request<'x> {
    #[serde(skip)]
    client: &'x Client,
    #[serde(skip)]
    account_id: String,

    pub using: Vec<URI>,

    #[serde(rename = "methodCalls")]
    pub method_calls: Vec<(Method, Arguments, String)>,

    #[serde(rename = "createdIds")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_ids: Option<AHashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultReference {
    #[serde(rename = "resultOf")]
    result_of: String,
    name: Method,
    path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Arguments {
    Changes(ChangesRequest),
    PushGet(GetRequest<PushSubscription<Set>>),
    PushSet(SetRequest<PushSubscription<Set>>),
    BlobCopy(CopyBlobRequest),
    MailboxGet(GetRequest<Mailbox<Set>>),
    MailboxQuery(QueryRequest<Mailbox<Set>>),
    MailboxQueryChanges(QueryChangesRequest<Mailbox<Set>>),
    MailboxSet(SetRequest<Mailbox<Set>>),
    ThreadGet(GetRequest<Thread>),
    EmailGet(GetRequest<Email<Set>>),
    EmailQuery(QueryRequest<Email<Set>>),
    EmailQueryChanges(QueryChangesRequest<Email<Set>>),
    EmailSet(SetRequest<Email<Set>>),
    EmailCopy(CopyRequest<Email<Set>>),
    EmailImport(EmailImportRequest),
    EmailParse(EmailParseRequest),
    SearchSnippetGet(SearchSnippetGetRequest),
    IdentityGet(GetRequest<Identity<Set>>),
    IdentitySet(SetRequest<Identity<Set>>),
    EmailSubmissionGet(GetRequest<EmailSubmission<Set>>),
    EmailSubmissionQuery(QueryRequest<EmailSubmission<Set>>),
    EmailSubmissionQueryChanges(QueryChangesRequest<EmailSubmission<Set>>),
    EmailSubmissionSet(SetRequest<EmailSubmission<Set>>),
    VacationResponseGet(GetRequest<VacationResponse<Set>>),
    VacationResponseSet(SetRequest<VacationResponse<Set>>),
    SieveScriptGet(GetRequest<SieveScript<Set>>),
    SieveScriptQuery(QueryRequest<SieveScript<Set>>),
    SieveScriptValidate(SieveScriptValidateRequest),
    SieveScriptSet(SetRequest<SieveScript<Set>>),
    PrincipalGet(GetRequest<Principal<Set>>),
    PrincipalQuery(QueryRequest<Principal<Set>>),
    PrincipalQueryChanges(QueryChangesRequest<Principal<Set>>),
    PrincipalSet(SetRequest<Principal<Set>>),
}

impl Arguments {
    pub fn changes(params: RequestParams, since_state: String) -> Self {
        Arguments::Changes(ChangesRequest::new(params, since_state))
    }

    pub fn push_get(params: RequestParams) -> Self {
        Arguments::PushGet(GetRequest::new(params))
    }

    pub fn push_set(params: RequestParams) -> Self {
        Arguments::PushSet(SetRequest::new(params))
    }

    pub fn blob_copy(params: RequestParams, from_account_id: String) -> Self {
        Arguments::BlobCopy(CopyBlobRequest::new(params, from_account_id))
    }

    pub fn mailbox_get(params: RequestParams) -> Self {
        Arguments::MailboxGet(GetRequest::new(params))
    }

    pub fn mailbox_query(params: RequestParams) -> Self {
        Arguments::MailboxQuery(QueryRequest::new(params))
    }

    pub fn mailbox_query_changes(params: RequestParams, since_query_state: String) -> Self {
        Arguments::MailboxQueryChanges(QueryChangesRequest::new(params, since_query_state))
    }

    pub fn mailbox_set(params: RequestParams) -> Self {
        Arguments::MailboxSet(SetRequest::new(params))
    }

    pub fn thread_get(params: RequestParams) -> Self {
        Arguments::ThreadGet(GetRequest::new(params))
    }

    pub fn email_get(params: RequestParams) -> Self {
        Arguments::EmailGet(GetRequest::new(params))
    }

    pub fn email_query(params: RequestParams) -> Self {
        Arguments::EmailQuery(QueryRequest::new(params))
    }

    pub fn email_query_changes(params: RequestParams, since_query_state: String) -> Self {
        Arguments::EmailQueryChanges(QueryChangesRequest::new(params, since_query_state))
    }

    pub fn email_set(params: RequestParams) -> Self {
        Arguments::EmailSet(SetRequest::new(params))
    }

    pub fn email_copy(params: RequestParams, from_account_id: String) -> Self {
        Arguments::EmailCopy(CopyRequest::new(params, from_account_id))
    }

    pub fn email_import(params: RequestParams) -> Self {
        Arguments::EmailImport(EmailImportRequest::new(params))
    }

    pub fn email_parse(params: RequestParams) -> Self {
        Arguments::EmailParse(EmailParseRequest::new(params))
    }

    pub fn search_snippet_get(params: RequestParams) -> Self {
        Arguments::SearchSnippetGet(SearchSnippetGetRequest::new(params))
    }

    pub fn identity_get(params: RequestParams) -> Self {
        Arguments::IdentityGet(GetRequest::new(params))
    }

    pub fn identity_set(params: RequestParams) -> Self {
        Arguments::IdentitySet(SetRequest::new(params))
    }

    pub fn email_submission_get(params: RequestParams) -> Self {
        Arguments::EmailSubmissionGet(GetRequest::new(params))
    }

    pub fn email_submission_query(params: RequestParams) -> Self {
        Arguments::EmailSubmissionQuery(QueryRequest::new(params))
    }

    pub fn email_submission_query_changes(
        params: RequestParams,
        since_query_state: String,
    ) -> Self {
        Arguments::EmailSubmissionQueryChanges(QueryChangesRequest::new(params, since_query_state))
    }

    pub fn email_submission_set(params: RequestParams) -> Self {
        Arguments::EmailSubmissionSet(SetRequest::new(params))
    }

    pub fn vacation_response_get(params: RequestParams) -> Self {
        Arguments::VacationResponseGet(GetRequest::new(params))
    }

    pub fn vacation_response_set(params: RequestParams) -> Self {
        Arguments::VacationResponseSet(SetRequest::new(params))
    }

    pub fn sieve_script_get(params: RequestParams) -> Self {
        Arguments::SieveScriptGet(GetRequest::new(params))
    }

    pub fn sieve_script_query(params: RequestParams) -> Self {
        Arguments::SieveScriptQuery(QueryRequest::new(params))
    }

    pub fn sieve_script_validate(params: RequestParams, blob_id: impl Into<String>) -> Self {
        Arguments::SieveScriptValidate(SieveScriptValidateRequest::new(params, blob_id))
    }

    pub fn sieve_script_set(params: RequestParams) -> Self {
        Arguments::SieveScriptSet(SetRequest::new(params))
    }

    pub fn principal_get(params: RequestParams) -> Self {
        Arguments::PrincipalGet(GetRequest::new(params))
    }

    pub fn principal_query(params: RequestParams) -> Self {
        Arguments::PrincipalQuery(QueryRequest::new(params))
    }

    pub fn principal_query_changes(params: RequestParams, since_query_state: String) -> Self {
        Arguments::PrincipalQueryChanges(QueryChangesRequest::new(params, since_query_state))
    }

    pub fn principal_set(params: RequestParams) -> Self {
        Arguments::PrincipalSet(SetRequest::new(params))
    }

    pub fn changes_mut(&mut self) -> &mut ChangesRequest {
        match self {
            Arguments::Changes(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn push_get_mut(&mut self) -> &mut GetRequest<PushSubscription<Set>> {
        match self {
            Arguments::PushGet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn push_set_mut(&mut self) -> &mut SetRequest<PushSubscription<Set>> {
        match self {
            Arguments::PushSet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn blob_copy_mut(&mut self) -> &mut CopyBlobRequest {
        match self {
            Arguments::BlobCopy(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn mailbox_get_mut(&mut self) -> &mut GetRequest<Mailbox<Set>> {
        match self {
            Arguments::MailboxGet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn mailbox_query_mut(&mut self) -> &mut QueryRequest<Mailbox<Set>> {
        match self {
            Arguments::MailboxQuery(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn mailbox_query_changes_mut(&mut self) -> &mut QueryChangesRequest<Mailbox<Set>> {
        match self {
            Arguments::MailboxQueryChanges(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn mailbox_set_mut(&mut self) -> &mut SetRequest<Mailbox<Set>> {
        match self {
            Arguments::MailboxSet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn thread_get_mut(&mut self) -> &mut GetRequest<Thread> {
        match self {
            Arguments::ThreadGet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn email_get_mut(&mut self) -> &mut GetRequest<Email<Set>> {
        match self {
            Arguments::EmailGet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn email_query_mut(&mut self) -> &mut QueryRequest<Email<Set>> {
        match self {
            Arguments::EmailQuery(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn email_query_changes_mut(&mut self) -> &mut QueryChangesRequest<Email<Set>> {
        match self {
            Arguments::EmailQueryChanges(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn email_set_mut(&mut self) -> &mut SetRequest<Email<Set>> {
        match self {
            Arguments::EmailSet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn email_copy_mut(&mut self) -> &mut CopyRequest<Email<Set>> {
        match self {
            Arguments::EmailCopy(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn email_import_mut(&mut self) -> &mut EmailImportRequest {
        match self {
            Arguments::EmailImport(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn email_parse_mut(&mut self) -> &mut EmailParseRequest {
        match self {
            Arguments::EmailParse(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn search_snippet_get_mut(&mut self) -> &mut SearchSnippetGetRequest {
        match self {
            Arguments::SearchSnippetGet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn identity_get_mut(&mut self) -> &mut GetRequest<Identity<Set>> {
        match self {
            Arguments::IdentityGet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn identity_set_mut(&mut self) -> &mut SetRequest<Identity<Set>> {
        match self {
            Arguments::IdentitySet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn email_submission_get_mut(&mut self) -> &mut GetRequest<EmailSubmission<Set>> {
        match self {
            Arguments::EmailSubmissionGet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn email_submission_query_mut(&mut self) -> &mut QueryRequest<EmailSubmission<Set>> {
        match self {
            Arguments::EmailSubmissionQuery(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn email_submission_query_changes_mut(
        &mut self,
    ) -> &mut QueryChangesRequest<EmailSubmission<Set>> {
        match self {
            Arguments::EmailSubmissionQueryChanges(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn email_submission_set_mut(&mut self) -> &mut SetRequest<EmailSubmission<Set>> {
        match self {
            Arguments::EmailSubmissionSet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn vacation_response_get_mut(&mut self) -> &mut GetRequest<VacationResponse<Set>> {
        match self {
            Arguments::VacationResponseGet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn vacation_response_set_mut(&mut self) -> &mut SetRequest<VacationResponse<Set>> {
        match self {
            Arguments::VacationResponseSet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn sieve_script_get_mut(&mut self) -> &mut GetRequest<SieveScript<Set>> {
        match self {
            Arguments::SieveScriptGet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn sieve_script_query_mut(&mut self) -> &mut QueryRequest<SieveScript<Set>> {
        match self {
            Arguments::SieveScriptQuery(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn sieve_script_validate_mut(&mut self) -> &mut SieveScriptValidateRequest {
        match self {
            Arguments::SieveScriptValidate(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn sieve_script_set_mut(&mut self) -> &mut SetRequest<SieveScript<Set>> {
        match self {
            Arguments::SieveScriptSet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn principal_get_mut(&mut self) -> &mut GetRequest<Principal<Set>> {
        match self {
            Arguments::PrincipalGet(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn principal_query_mut(&mut self) -> &mut QueryRequest<Principal<Set>> {
        match self {
            Arguments::PrincipalQuery(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn principal_query_changes_mut(&mut self) -> &mut QueryChangesRequest<Principal<Set>> {
        match self {
            Arguments::PrincipalQueryChanges(ref mut r) => r,
            _ => unreachable!(),
        }
    }

    pub fn principal_set_mut(&mut self) -> &mut SetRequest<Principal<Set>> {
        match self {
            Arguments::PrincipalSet(ref mut r) => r,
            _ => unreachable!(),
        }
    }
}

impl<'x> Request<'x> {
    pub fn new(client: &'x Client) -> Self {
        Request {
            using: vec![
                URI::Core,
                URI::Mail,
                URI::Submission,
                URI::VacationResponse,
                URI::Contacts,
                URI::Calendars,
                URI::WebSocket,
                URI::Sieve,
                URI::Blob,
                URI::Quota,
                URI::Principals,
            ],
            method_calls: vec![],
            created_ids: None,
            account_id: client.default_account_id().to_string(),
            client,
        }
    }

    pub fn account_id(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = account_id.into();
        self
    }

    #[maybe_async::maybe_async]
    pub async fn send(self) -> crate::Result<Response<TaggedMethodResponse>> {
        self.client.send(&self).await
    }

    #[cfg(feature = "websockets")]
    pub async fn send_ws(self) -> crate::Result<String> {
        self.client.send_ws(self).await
    }

    #[maybe_async::maybe_async]
    pub async fn send_single<T>(self) -> crate::Result<T>
    where
        T: DeserializeOwned,
    {
        let response: Response<SingleMethodResponse<T>> = self.client.send(&self).await?;
        match response
            .unwrap_method_responses()
            .pop()
            .ok_or_else(|| Error::Internal("Server returned no results".to_string()))?
        {
            SingleMethodResponse::Ok((_, response, _)) => Ok(response),
            SingleMethodResponse::Error((_, err, _)) => Err(err.into()),
        }
    }

    pub fn params(&self, method: Method) -> RequestParams {
        RequestParams {
            account_id: self.account_id.clone(),
            method,
            call_id: self.method_calls.len(),
        }
    }

    pub fn add_method_call(&mut self, method: Method, arguments: Arguments) -> &mut Arguments {
        let call_id = format!("s{}", self.method_calls.len());
        self.method_calls.push((method, arguments, call_id));
        &mut self.method_calls.last_mut().unwrap().1
    }

    pub fn add_capability(&mut self, uri: URI) {
        if !self.using.contains(&uri) {
            self.using.push(uri);
        }
    }

    pub fn last_result_reference(&self, path: impl Into<String>) -> ResultReference {
        let last_method = self.method_calls.last().unwrap();
        ResultReference {
            result_of: last_method.2.clone(),
            name: last_method.0,
            path: path.into(),
        }
    }
}

impl ResultReference {
    pub fn new(method: Method, call_id: usize, path: impl Into<String>) -> Self {
        ResultReference {
            result_of: format!("s{}", call_id),
            name: method,
            path: path.into(),
        }
    }
}
