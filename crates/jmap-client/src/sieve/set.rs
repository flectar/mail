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

use crate::{core::set::SetObject, Get, Set};

use super::{SetArguments, SieveScript};

impl SieveScript<Set> {
    pub fn name(&mut self, name: impl Into<String>) -> &mut Self {
        self.name = Some(name.into());
        self
    }

    pub fn blob_id(&mut self, blob_id: impl Into<String>) -> &mut Self {
        self.blob_id = Some(blob_id.into());
        self
    }
}

impl SetObject for SieveScript<Set> {
    type SetArguments = SetArguments;

    fn new(_create_id: Option<usize>) -> Self {
        SieveScript {
            _create_id,
            _state: Default::default(),
            id: None,
            name: None,
            blob_id: None,
            is_active: None,
        }
    }

    fn create_id(&self) -> Option<String> {
        self._create_id.map(|id| format!("c{}", id))
    }
}

impl SetArguments {
    pub fn on_success_activate_script(&mut self, id: impl Into<String>) -> &mut Self {
        self.on_success_activate_script = Some(format!("#{}", id.into()));
        self
    }

    pub fn on_success_activate_script_id(&mut self, id: impl Into<String>) -> &mut Self {
        self.on_success_activate_script = Some(id.into());
        self
    }

    pub fn on_success_deactivate_script(&mut self, value: bool) -> &mut Self {
        self.on_success_deactivate_script = Some(value);
        self
    }
}

impl SetObject for SieveScript<Get> {
    type SetArguments = ();

    fn new(_create_id: Option<usize>) -> Self {
        unimplemented!()
    }

    fn create_id(&self) -> Option<String> {
        None
    }
}
