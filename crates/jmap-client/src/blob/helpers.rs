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
    core::request::{Arguments, Request},
    Method,
};

use super::copy::{CopyBlobRequest, CopyBlobResponse};

impl Client {
    #[maybe_async::maybe_async]
    pub async fn blob_copy(
        &self,
        from_account_id: impl Into<String>,
        blob_id: impl Into<String>,
    ) -> crate::Result<String> {
        let blob_id = blob_id.into();
        let mut request = self.build();
        request.copy_blob(from_account_id).blob_id(&blob_id);
        request
            .send_single::<CopyBlobResponse>()
            .await?
            .copied(&blob_id)
    }
}

impl Request<'_> {
    #[maybe_async::maybe_async]
    pub fn copy_blob(&mut self, from_account_id: impl Into<String>) -> &mut CopyBlobRequest {
        self.add_method_call(
            Method::CopyBlob,
            Arguments::blob_copy(self.params(Method::CopyBlob), from_account_id.into()),
        )
        .blob_copy_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_copy_blob(self) -> crate::Result<CopyBlobResponse> {
        self.send_single().await
    }
}
