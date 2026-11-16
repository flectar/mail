//! Automatic replies through JMAP's VacationResponse (RFC 8621 section 8,
//! #239), for accounts that offer it without JMAP Sieve. Servers with JMAP
//! Sieve, such as Stalwart and Cyrus, keep the response as a Sieve script of
//! their own: saving one there turns the user's own filters off, or is
//! refused, so those accounts keep replies in the app's own script
//! (`jmap::sieve`).

use super::client::{ConnectedClient, map_error};
use crate::error::Result;
use crate::models::AutoReply;
use jmap_client::core::response::{VacationResponseGetResponse, VacationResponseSetResponse};
use jmap_client::vacation_response::VacationResponse;

const VACATION_CAPABILITY: &str = "urn:ietf:params:jmap:vacationresponse";
const SIEVE_CAPABILITY: &str = "urn:ietf:params:jmap:sieve";

/// Whether the app keeps this account's automatic replies as its
/// VacationResponse.
pub(crate) fn offered(connected: &ConnectedClient) -> bool {
    connected
        .client
        .session()
        .account(&connected.account_id)
        .is_some_and(|account| {
            account.capability(VACATION_CAPABILITY).is_some()
                && account.capability(SIEVE_CAPABILITY).is_none()
        })
}

/// The account's automatic replies.
pub(crate) async fn read(connected: &ConnectedClient) -> Result<AutoReply> {
    let mut request = connected.request();
    request
        .get_vacation_response()
        .account_id(&connected.account_id)
        .ids(["singleton"]);
    let response = connected
        .send_single::<VacationResponseGetResponse>(request)
        .await
        .map_err(map_error)?
        .take_list()
        .pop();
    Ok(response.as_ref().map(reply).unwrap_or_default())
}

/// Saves `wanted`, and gives the replies as the server keeps them now.
pub(crate) async fn save(connected: &ConnectedClient, wanted: &AutoReply) -> Result<AutoReply> {
    let mut request = connected.request();
    request
        .set_vacation_response()
        .account_id(&connected.account_id)
        .update("singleton")
        .is_enabled(wanted.enabled)
        .from_date(wanted.starts_at.map(|at| at.div_euclid(1000)))
        // JMAP's end is the first moment without replies.
        .to_date(wanted.ends_at.map(|at| (at + 1).div_euclid(1000)))
        // Without a subject of its own, the server chooses one.
        .subject((!wanted.subject.is_empty()).then(|| wanted.subject.clone()))
        .text_body(Some(wanted.text.clone()))
        .html_body(None::<String>);
    connected
        .send_single::<VacationResponseSetResponse>(request)
        .await
        .map_err(map_error)?
        .updated("singleton")
        .map_err(map_error)?;
    read(connected).await
}

fn reply(response: &VacationResponse) -> AutoReply {
    AutoReply {
        enabled: response.is_enabled(),
        subject: response.subject().unwrap_or_default().to_owned(),
        text: crate::mime::body_text(response.text_body(), response.html_body())
            .map(|text| text.trim().to_owned())
            .unwrap_or_default(),
        starts_at: response.from_date().map(|at| at * 1000),
        ends_at: response.to_date().map(|at| at * 1000 - 1),
        contacts_only: false,
        domain_only: false,
    }
}
