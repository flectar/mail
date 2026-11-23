//! Standards-based CardDAV discovery and two-way contact synchronization.

pub mod discovery;
pub mod http;
pub mod import;
pub mod sync;
pub mod task;
pub mod vcard;
pub mod xml;

use crate::error::CoreError;

pub fn err(message: impl Into<String>) -> CoreError {
    CoreError::CardDav(message.into())
}
