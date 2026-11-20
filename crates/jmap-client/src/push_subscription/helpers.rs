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
    core::{
        get::GetRequest,
        request::{Arguments, Request},
        response::{PushSubscriptionGetResponse, PushSubscriptionSetResponse},
        set::{SetObject, SetRequest},
    },
    DataType, Method, Set,
};

use super::{Keys, PushSubscription};

impl Client {
    #[maybe_async::maybe_async]
    pub async fn push_subscription_create(
        &self,
        device_client_id: impl Into<String>,
        url: impl Into<String>,
        keys: Option<Keys>,
    ) -> crate::Result<PushSubscription> {
        let mut request = self.build();
        let create_req = request
            .set_push_subscription()
            .create()
            .device_client_id(device_client_id)
            .url(url);

        if let Some(keys) = keys {
            create_req.keys(keys);
        }

        let id = create_req.create_id().unwrap();
        request
            .send_single::<PushSubscriptionSetResponse>()
            .await?
            .created(&id)
    }

    #[maybe_async::maybe_async]
    pub async fn push_subscription_verify(
        &self,
        id: &str,
        verification_code: impl Into<String>,
    ) -> crate::Result<Option<PushSubscription>> {
        let mut request = self.build();
        request
            .set_push_subscription()
            .update(id)
            .verification_code(verification_code);
        request
            .send_single::<PushSubscriptionSetResponse>()
            .await?
            .updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn push_subscription_update_types(
        &self,
        id: &str,
        types: Option<impl IntoIterator<Item = DataType>>,
    ) -> crate::Result<Option<PushSubscription>> {
        let mut request = self.build();
        request.set_push_subscription().update(id).types(types);
        request
            .send_single::<PushSubscriptionSetResponse>()
            .await?
            .updated(id)
    }

    #[maybe_async::maybe_async]
    pub async fn push_subscription_destroy(&self, id: &str) -> crate::Result<()> {
        let mut request = self.build();
        request.set_push_subscription().destroy([id]);
        request
            .send_single::<PushSubscriptionSetResponse>()
            .await?
            .destroyed(id)
    }
}

impl Request<'_> {
    pub fn get_push_subscription(&mut self) -> &mut GetRequest<PushSubscription<Set>> {
        self.add_method_call(
            Method::GetPushSubscription,
            Arguments::push_get(self.params(Method::GetPushSubscription)),
        )
        .push_get_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_get_push_subscription(self) -> crate::Result<PushSubscriptionGetResponse> {
        self.send_single().await
    }

    pub fn set_push_subscription(&mut self) -> &mut SetRequest<PushSubscription<Set>> {
        self.add_method_call(
            Method::SetPushSubscription,
            Arguments::push_set(self.params(Method::SetPushSubscription)),
        )
        .push_set_mut()
    }

    #[maybe_async::maybe_async]
    pub async fn send_set_push_subscription(self) -> crate::Result<PushSubscriptionSetResponse> {
        self.send_single().await
    }
}
