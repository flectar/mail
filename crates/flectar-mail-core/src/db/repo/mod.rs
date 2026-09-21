pub mod accounts;
pub mod actions;
pub mod ai_usage;
pub mod caldav;
pub mod calendar;
pub mod carddav;
pub mod contacts;
pub mod counts;
pub mod email_stats;
pub mod embeddings;
pub mod folders;
pub mod gmail;
pub mod labels;
pub mod messages;
pub mod notifications;
pub mod search;
pub mod sender_identities;
pub mod settings;
pub mod snippets;
pub mod snoozes;
pub mod splits;
pub mod sync_failures;
pub mod threads;

use crate::models::Address;
use rusqlite::types::Type;
use serde::de::DeserializeOwned;

pub(crate) fn parse_json_column<T: DeserializeOwned>(
    json: &str,
    column: usize,
) -> rusqlite::Result<T> {
    serde_json::from_str(json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
    })
}

pub(crate) fn parse_addrs(json: &str, column: usize) -> rusqlite::Result<Vec<Address>> {
    parse_json_column(json, column)
}
