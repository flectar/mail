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

use crate::core::session::URLParser;

pub mod copy;
pub mod download;
pub mod helpers;
pub mod upload;

pub enum URLParameter {
    AccountId,
    BlobId,
    Name,
    Type,
}

impl URLParser for URLParameter {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "accountId" => Some(URLParameter::AccountId),
            "blobId" => Some(URLParameter::BlobId),
            "name" => Some(URLParameter::Name),
            "type" => Some(URLParameter::Type),
            _ => None,
        }
    }
}
