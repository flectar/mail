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

use ahash::AHashMap;
use serde::{de::Visitor, Deserialize};
use std::fmt;

use crate::{
    blob::copy::CopyBlobResponse,
    email::{
        import::EmailImportResponse, parse::EmailParseResponse,
        search_snippet::SearchSnippetGetResponse, Email,
    },
    email_submission::EmailSubmission,
    identity::Identity,
    mailbox::Mailbox,
    principal::Principal,
    push_subscription::PushSubscription,
    sieve::{validate::SieveScriptValidateResponse, SieveScript},
    thread::Thread,
    vacation_response::VacationResponse,
    Get, Method,
};

use super::{
    changes::ChangesResponse, copy::CopyResponse, error::MethodError, get::GetResponse,
    query::QueryResponse, query_changes::QueryChangesResponse, set::SetResponse,
};

#[derive(Debug, Deserialize)]
pub struct Response<T> {
    #[serde(rename = "methodResponses")]
    method_responses: Vec<T>,

    #[serde(rename = "createdIds")]
    created_ids: Option<AHashMap<String, String>>,

    #[serde(rename = "sessionState")]
    session_state: String,

    request_id: Option<String>,
}

impl<T> Response<T> {
    pub fn new(
        method_responses: Vec<T>,
        created_ids: Option<AHashMap<String, String>>,
        session_state: String,
        request_id: Option<String>,
    ) -> Self {
        Response {
            method_responses,
            created_ids,
            session_state,
            request_id,
        }
    }

    pub fn method_responses(&self) -> &[T] {
        self.method_responses.as_ref()
    }

    pub fn unwrap_method_responses(self) -> Vec<T> {
        self.method_responses
    }

    pub fn method_response_by_pos(&mut self, index: usize) -> T {
        self.method_responses.remove(index)
    }

    pub fn pop_method_response(&mut self) -> Option<T> {
        self.method_responses.pop()
    }

    pub fn created_ids(&self) -> Option<impl Iterator<Item = (&String, &String)>> {
        self.created_ids.as_ref().map(|map| map.iter())
    }

    pub fn session_state(&self) -> &str {
        &self.session_state
    }

    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }
}

impl Response<TaggedMethodResponse> {
    pub fn method_response_by_id(&self, id: &str) -> Option<&TaggedMethodResponse> {
        self.method_responses
            .iter()
            .find(|response| response.call_id() == id)
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SingleMethodResponse<T> {
    Error((Error, MethodError, String)),
    Ok((String, T, String)),
}

#[derive(Debug, Deserialize)]
pub enum Error {
    #[serde(rename = "error")]
    Error,
}

pub type PushSubscriptionSetResponse = SetResponse<PushSubscription<Get>>;
pub type PushSubscriptionGetResponse = GetResponse<PushSubscription<Get>>;
pub type MailboxChangesResponse = ChangesResponse<Mailbox<Get>>;
pub type MailboxSetResponse = SetResponse<Mailbox<Get>>;
pub type MailboxGetResponse = GetResponse<Mailbox<Get>>;
pub type ThreadGetResponse = GetResponse<Thread>;
pub type ThreadChangesResponse = ChangesResponse<Thread>;
pub type EmailGetResponse = GetResponse<Email<Get>>;
pub type EmailSetResponse = SetResponse<Email<Get>>;
pub type EmailCopyResponse = CopyResponse<Email<Get>>;
pub type EmailChangesResponse = ChangesResponse<Email<Get>>;
pub type IdentitySetResponse = SetResponse<Identity<Get>>;
pub type IdentityGetResponse = GetResponse<Identity<Get>>;
pub type IdentityChangesResponse = ChangesResponse<Identity<Get>>;
pub type EmailSubmissionSetResponse = SetResponse<EmailSubmission<Get>>;
pub type EmailSubmissionGetResponse = GetResponse<EmailSubmission<Get>>;
pub type EmailSubmissionChangesResponse = ChangesResponse<EmailSubmission<Get>>;
pub type VacationResponseGetResponse = GetResponse<VacationResponse<Get>>;
pub type VacationResponseSetResponse = SetResponse<VacationResponse<Get>>;
pub type SieveScriptGetResponse = GetResponse<SieveScript<Get>>;
pub type SieveScriptSetResponse = SetResponse<SieveScript<Get>>;
pub type PrincipalChangesResponse = ChangesResponse<Principal<Get>>;
pub type PrincipalSetResponse = SetResponse<Principal<Get>>;
pub type PrincipalGetResponse = GetResponse<Principal<Get>>;

#[derive(Debug)]
pub struct TaggedMethodResponse {
    id: String,
    response: MethodResponse,
}

#[derive(Debug)]
pub enum MethodResponse {
    CopyBlob(CopyBlobResponse),
    GetPushSubscription(PushSubscriptionGetResponse),
    SetPushSubscription(PushSubscriptionSetResponse),
    GetMailbox(MailboxGetResponse),
    ChangesMailbox(MailboxChangesResponse),
    QueryMailbox(QueryResponse),
    QueryChangesMailbox(QueryChangesResponse),
    SetMailbox(MailboxSetResponse),
    GetThread(ThreadGetResponse),
    ChangesThread(ThreadChangesResponse),
    GetEmail(EmailGetResponse),
    ChangesEmail(EmailChangesResponse),
    QueryEmail(QueryResponse),
    QueryChangesEmail(QueryChangesResponse),
    SetEmail(EmailSetResponse),
    CopyEmail(EmailCopyResponse),
    ImportEmail(EmailImportResponse),
    ParseEmail(EmailParseResponse),
    GetSearchSnippet(SearchSnippetGetResponse),
    GetIdentity(IdentityGetResponse),
    ChangesIdentity(IdentityChangesResponse),
    SetIdentity(IdentitySetResponse),
    GetEmailSubmission(EmailSubmissionGetResponse),
    ChangesEmailSubmission(EmailSubmissionChangesResponse),
    QueryEmailSubmission(QueryResponse),
    QueryChangesEmailSubmission(QueryChangesResponse),
    SetEmailSubmission(EmailSubmissionSetResponse),
    GetVacationResponse(VacationResponseGetResponse),
    SetVacationResponse(VacationResponseSetResponse),
    GetSieveScript(SieveScriptGetResponse),
    QuerySieveScript(QueryResponse),
    SetSieveScript(SieveScriptSetResponse),
    ValidateSieveScript(SieveScriptValidateResponse),

    GetPrincipal(PrincipalGetResponse),
    ChangesPrincipal(PrincipalChangesResponse),
    QueryPrincipal(QueryResponse),
    QueryChangesPrincipal(QueryChangesResponse),
    SetPrincipal(PrincipalSetResponse),

    Echo(serde_json::Value),
    Error(MethodError),
}

impl TaggedMethodResponse {
    pub fn call_id(&self) -> &str {
        self.id.as_str()
    }

    pub fn is_type(&self, type_: Method) -> bool {
        matches!(
            (&self.response, type_),
            (MethodResponse::CopyBlob(_), Method::CopyBlob)
                | (
                    MethodResponse::GetPushSubscription(_),
                    Method::GetPushSubscription
                )
                | (
                    MethodResponse::SetPushSubscription(_),
                    Method::SetPushSubscription
                )
                | (MethodResponse::GetMailbox(_), Method::GetMailbox)
                | (MethodResponse::ChangesMailbox(_), Method::ChangesMailbox)
                | (MethodResponse::QueryMailbox(_), Method::QueryMailbox)
                | (
                    MethodResponse::QueryChangesMailbox(_),
                    Method::QueryChangesMailbox
                )
                | (MethodResponse::SetMailbox(_), Method::SetMailbox)
                | (MethodResponse::GetThread(_), Method::GetThread)
                | (MethodResponse::ChangesThread(_), Method::ChangesThread)
                | (MethodResponse::GetEmail(_), Method::GetEmail)
                | (MethodResponse::ChangesEmail(_), Method::ChangesEmail)
                | (MethodResponse::QueryEmail(_), Method::QueryEmail)
                | (
                    MethodResponse::QueryChangesEmail(_),
                    Method::QueryChangesEmail
                )
                | (MethodResponse::SetEmail(_), Method::SetEmail)
                | (MethodResponse::CopyEmail(_), Method::CopyEmail)
                | (MethodResponse::ImportEmail(_), Method::ImportEmail)
                | (MethodResponse::ParseEmail(_), Method::ParseEmail)
                | (
                    MethodResponse::GetSearchSnippet(_),
                    Method::GetSearchSnippet
                )
                | (MethodResponse::GetIdentity(_), Method::GetIdentity)
                | (MethodResponse::ChangesIdentity(_), Method::ChangesIdentity)
                | (MethodResponse::SetIdentity(_), Method::SetIdentity)
                | (
                    MethodResponse::GetEmailSubmission(_),
                    Method::GetEmailSubmission
                )
                | (
                    MethodResponse::ChangesEmailSubmission(_),
                    Method::ChangesEmailSubmission
                )
                | (
                    MethodResponse::QueryEmailSubmission(_),
                    Method::QueryEmailSubmission
                )
                | (
                    MethodResponse::QueryChangesEmailSubmission(_),
                    Method::QueryChangesEmailSubmission
                )
                | (
                    MethodResponse::SetEmailSubmission(_),
                    Method::SetEmailSubmission
                )
                | (
                    MethodResponse::GetVacationResponse(_),
                    Method::GetVacationResponse
                )
                | (
                    MethodResponse::SetVacationResponse(_),
                    Method::SetVacationResponse
                )
                | (MethodResponse::GetSieveScript(_), Method::GetSieveScript)
                | (
                    MethodResponse::ValidateSieveScript(_),
                    Method::ValidateSieveScript
                )
                | (
                    MethodResponse::QuerySieveScript(_),
                    Method::QuerySieveScript
                )
                | (MethodResponse::SetSieveScript(_), Method::SetSieveScript)
                | (MethodResponse::GetPrincipal(_), Method::GetPrincipal)
                | (
                    MethodResponse::ChangesPrincipal(_),
                    Method::ChangesPrincipal
                )
                | (MethodResponse::QueryPrincipal(_), Method::QueryPrincipal)
                | (
                    MethodResponse::QueryChangesPrincipal(_),
                    Method::QueryChangesPrincipal
                )
                | (MethodResponse::SetPrincipal(_), Method::SetPrincipal)
                | (MethodResponse::Echo(_), Method::Echo)
                | (MethodResponse::Error(_), Method::Error)
        )
    }

    pub fn unwrap_method_response(self) -> MethodResponse {
        self.response
    }

    pub fn unwrap_copy_blob(self) -> crate::Result<CopyBlobResponse> {
        match self.response {
            MethodResponse::CopyBlob(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_get_push_subscription(self) -> crate::Result<PushSubscriptionGetResponse> {
        match self.response {
            MethodResponse::GetPushSubscription(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_set_push_subscription(self) -> crate::Result<PushSubscriptionSetResponse> {
        match self.response {
            MethodResponse::SetPushSubscription(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_get_mailbox(self) -> crate::Result<MailboxGetResponse> {
        match self.response {
            MethodResponse::GetMailbox(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_changes_mailbox(self) -> crate::Result<MailboxChangesResponse> {
        match self.response {
            MethodResponse::ChangesMailbox(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_query_mailbox(self) -> crate::Result<QueryResponse> {
        match self.response {
            MethodResponse::QueryMailbox(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_query_changes_mailbox(self) -> crate::Result<QueryChangesResponse> {
        match self.response {
            MethodResponse::QueryChangesMailbox(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_set_mailbox(self) -> crate::Result<MailboxSetResponse> {
        match self.response {
            MethodResponse::SetMailbox(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_get_thread(self) -> crate::Result<ThreadGetResponse> {
        match self.response {
            MethodResponse::GetThread(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_changes_thread(self) -> crate::Result<ThreadChangesResponse> {
        match self.response {
            MethodResponse::ChangesThread(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_get_email(self) -> crate::Result<EmailGetResponse> {
        match self.response {
            MethodResponse::GetEmail(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_changes_email(self) -> crate::Result<EmailChangesResponse> {
        match self.response {
            MethodResponse::ChangesEmail(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_query_email(self) -> crate::Result<QueryResponse> {
        match self.response {
            MethodResponse::QueryEmail(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_query_changes_email(self) -> crate::Result<QueryChangesResponse> {
        match self.response {
            MethodResponse::QueryChangesEmail(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_set_email(self) -> crate::Result<EmailSetResponse> {
        match self.response {
            MethodResponse::SetEmail(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_copy_email(self) -> crate::Result<EmailCopyResponse> {
        match self.response {
            MethodResponse::CopyEmail(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_import_email(self) -> crate::Result<EmailImportResponse> {
        match self.response {
            MethodResponse::ImportEmail(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_parse_email(self) -> crate::Result<EmailParseResponse> {
        match self.response {
            MethodResponse::ParseEmail(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_get_search_snippet(self) -> crate::Result<SearchSnippetGetResponse> {
        match self.response {
            MethodResponse::GetSearchSnippet(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_get_identity(self) -> crate::Result<IdentityGetResponse> {
        match self.response {
            MethodResponse::GetIdentity(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_changes_identity(self) -> crate::Result<IdentityChangesResponse> {
        match self.response {
            MethodResponse::ChangesIdentity(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_set_identity(self) -> crate::Result<IdentitySetResponse> {
        match self.response {
            MethodResponse::SetIdentity(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_get_email_submission(self) -> crate::Result<EmailSubmissionGetResponse> {
        match self.response {
            MethodResponse::GetEmailSubmission(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_changes_email_submission(self) -> crate::Result<EmailSubmissionChangesResponse> {
        match self.response {
            MethodResponse::ChangesEmailSubmission(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_set_email_submission(self) -> crate::Result<EmailSubmissionSetResponse> {
        match self.response {
            MethodResponse::SetEmailSubmission(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_query_email_submission(self) -> crate::Result<QueryResponse> {
        match self.response {
            MethodResponse::QueryEmailSubmission(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_query_changes_email_submission(self) -> crate::Result<QueryChangesResponse> {
        match self.response {
            MethodResponse::QueryChangesEmailSubmission(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_get_vacation_response(self) -> crate::Result<VacationResponseGetResponse> {
        match self.response {
            MethodResponse::GetVacationResponse(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_set_vacation_response(self) -> crate::Result<VacationResponseSetResponse> {
        match self.response {
            MethodResponse::SetVacationResponse(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_get_sieve_script(self) -> crate::Result<SieveScriptGetResponse> {
        match self.response {
            MethodResponse::GetSieveScript(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_validate_sieve_script(self) -> crate::Result<SieveScriptValidateResponse> {
        match self.response {
            MethodResponse::ValidateSieveScript(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_set_sieve_script(self) -> crate::Result<SieveScriptSetResponse> {
        match self.response {
            MethodResponse::SetSieveScript(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_query_sieve_script(self) -> crate::Result<QueryResponse> {
        match self.response {
            MethodResponse::QuerySieveScript(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_get_principal(self) -> crate::Result<PrincipalGetResponse> {
        match self.response {
            MethodResponse::GetPrincipal(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_changes_principal(self) -> crate::Result<PrincipalChangesResponse> {
        match self.response {
            MethodResponse::ChangesPrincipal(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_query_principal(self) -> crate::Result<QueryResponse> {
        match self.response {
            MethodResponse::QueryPrincipal(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_query_changes_principal(self) -> crate::Result<QueryChangesResponse> {
        match self.response {
            MethodResponse::QueryChangesPrincipal(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_set_principal(self) -> crate::Result<PrincipalSetResponse> {
        match self.response {
            MethodResponse::SetPrincipal(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn unwrap_echo(self) -> crate::Result<serde_json::Value> {
        match self.response {
            MethodResponse::Echo(response) => Ok(response),
            MethodResponse::Error(err) => Err(err.into()),
            _ => Err("Response type mismatch".into()),
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self.response, MethodResponse::Error(_))
    }
}

impl<'de> Deserialize<'de> for TaggedMethodResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(TaggedMethodResponseVisitor)
    }
}

struct TaggedMethodResponseVisitor;

impl<'de> Visitor<'de> for TaggedMethodResponseVisitor {
    type Value = TaggedMethodResponse;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a valid JMAP method response")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let response = match seq
            .next_element::<Method>()?
            .ok_or_else(|| serde::de::Error::custom("Expected a method name"))?
        {
            Method::Echo => MethodResponse::Echo(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::CopyBlob => MethodResponse::CopyBlob(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::GetPushSubscription => MethodResponse::GetPushSubscription(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::SetPushSubscription => MethodResponse::SetPushSubscription(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::GetMailbox => MethodResponse::GetMailbox(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::ChangesMailbox => MethodResponse::ChangesMailbox(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::QueryMailbox => MethodResponse::QueryMailbox(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::QueryChangesMailbox => MethodResponse::QueryChangesMailbox(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::SetMailbox => MethodResponse::SetMailbox(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::GetThread => MethodResponse::GetThread(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::ChangesThread => MethodResponse::ChangesThread(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::GetEmail => MethodResponse::GetEmail(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::ChangesEmail => MethodResponse::ChangesEmail(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::QueryEmail => MethodResponse::QueryEmail(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::QueryChangesEmail => MethodResponse::QueryChangesEmail(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::SetEmail => MethodResponse::SetEmail(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::CopyEmail => MethodResponse::CopyEmail(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::ImportEmail => MethodResponse::ImportEmail(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::ParseEmail => MethodResponse::ParseEmail(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::GetSearchSnippet => MethodResponse::GetSearchSnippet(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::GetIdentity => MethodResponse::GetIdentity(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::ChangesIdentity => MethodResponse::ChangesIdentity(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::SetIdentity => MethodResponse::SetIdentity(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::GetEmailSubmission => MethodResponse::GetEmailSubmission(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::ChangesEmailSubmission => MethodResponse::ChangesEmailSubmission(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::QueryEmailSubmission => MethodResponse::QueryEmailSubmission(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::QueryChangesEmailSubmission => MethodResponse::QueryChangesEmailSubmission(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::SetEmailSubmission => MethodResponse::SetEmailSubmission(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::GetVacationResponse => MethodResponse::GetVacationResponse(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::SetVacationResponse => MethodResponse::SetVacationResponse(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::GetSieveScript => MethodResponse::GetSieveScript(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::SetSieveScript => MethodResponse::SetSieveScript(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::QuerySieveScript => MethodResponse::QuerySieveScript(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::ValidateSieveScript => MethodResponse::ValidateSieveScript(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::GetPrincipal => MethodResponse::GetPrincipal(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::ChangesPrincipal => MethodResponse::ChangesPrincipal(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::QueryPrincipal => MethodResponse::QueryPrincipal(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::QueryChangesPrincipal => MethodResponse::QueryChangesPrincipal(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::SetPrincipal => MethodResponse::SetPrincipal(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
            Method::Error => MethodResponse::Error(
                seq.next_element()?
                    .ok_or_else(|| serde::de::Error::custom("Expected a method response"))?,
            ),
        };

        let id = seq
            .next_element::<String>()?
            .ok_or_else(|| serde::de::Error::custom("Expected method call id"))?;

        Ok(TaggedMethodResponse { response, id })
    }
}
