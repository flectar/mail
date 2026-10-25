pub mod accounts;
pub mod actions;
pub mod ai_usage;
pub mod caldav;
pub mod calendar;
pub mod carddav;
pub mod contact_entries;
pub mod contacts;
pub mod counts;
pub mod email_stats;
pub mod embeddings;
pub mod folders;
pub mod gmail;
pub mod labels;
pub mod messages;
pub mod notifications;
pub mod pop3;
pub mod search;
pub mod sender_identities;
pub mod settings;
pub mod snippets;
pub mod snoozes;
pub mod splits;
pub mod sync_failures;
pub mod threads;

use crate::models::{Address, AuthKind, CalendarConnectionKind, MailProtocol, Provider};
use rusqlite::ToSql;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, Type, ValueRef};
use serde::de::DeserializeOwned;

/// Account and calendar enums are stored as their stable names. An unknown
/// name is a conversion error, never a fallback to another protocol, sign-in
/// method or calendar service.
macro_rules! stored_name_column {
    ($type:ty) => {
        impl FromSql for $type {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                let name = value.as_str()?;
                <$type>::parse(name).ok_or_else(|| {
                    FromSqlError::Other(format!("unknown {} {name:?}", stringify!($type)).into())
                })
            }
        }

        impl ToSql for $type {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }
    };
}

stored_name_column!(Provider);
stored_name_column!(AuthKind);
stored_name_column!(MailProtocol);
stored_name_column!(CalendarConnectionKind);

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
