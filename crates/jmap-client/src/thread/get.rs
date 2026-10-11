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

use crate::core::get::GetObject;

use super::Thread;

impl Thread {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn email_ids(&self) -> &[String] {
        &self.email_ids
    }
}

impl GetObject for Thread {
    type GetArguments = ();
}
