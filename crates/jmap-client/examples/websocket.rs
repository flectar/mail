/*
 * Copyright Stalwart Labs LLC See the COPYING
 * file at the top-level directory of this distribution.
 *
 * Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
 * https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
 * file at the top-level directory of this distribution.
 *
 * Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
 * https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
 * <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
 * option. This file may not be copied, modified, or distributed
 * except according to those terms.
 */

#[cfg(feature = "websockets")]
use futures_util::StreamExt;
#[cfg(feature = "websockets")]
use jmap_client::{client::Client, client_ws::WebSocketMessage, core::set::SetObject};
#[cfg(feature = "websockets")]
use tokio::sync::mpsc;

// Make sure the "websockets" feature is enabled!
#[cfg(feature = "websockets")]
async fn websocket() {
    // Connect to the JMAP server using Basic authentication

    use jmap_client::PushObject;

    let client = Client::new()
        .credentials(("john@example.org", "secret"))
        .connect("https://jmap.example.org")
        .await
        .unwrap();

    // Connect to the WebSocket endpoint
    let mut ws_stream = client.connect_ws().await.unwrap();

    // Read WS messages on a separate thread
    let (stream_tx, mut stream_rx) = mpsc::channel::<WebSocketMessage>(100);
    tokio::spawn(async move {
        while let Some(change) = ws_stream.next().await {
            stream_tx.send(change.unwrap()).await.unwrap();
        }
    });

    // Create a mailbox over WS
    let mut request = client.build();
    let create_id = request
        .set_mailbox()
        .create()
        .name("WebSocket Test")
        .create_id()
        .unwrap();
    let request_id = request.send_ws().await.unwrap();

    // Read response from WS stream
    let mailbox_id = if let Some(WebSocketMessage::Response(mut response)) = stream_rx.recv().await
    {
        assert_eq!(request_id, response.request_id().unwrap());
        response
            .pop_method_response()
            .unwrap()
            .unwrap_set_mailbox()
            .unwrap()
            .created(&create_id)
            .unwrap()
            .take_id()
    } else {
        unreachable!()
    };

    // Enable push notifications over WS
    client
        .enable_push_ws(None::<Vec<_>>, None::<&str>)
        .await
        .unwrap();

    // Make changes over standard HTTP and expect a push notification via WS
    client
        .mailbox_update_sort_order(&mailbox_id, 1)
        .await
        .unwrap();
    if let Some(WebSocketMessage::PushNotification(PushObject::StateChange { changed })) =
        stream_rx.recv().await
    {
        println!("Received changes: {:?}", changed);
    } else {
        unreachable!()
    }
}

fn main() {
    #[cfg(feature = "websockets")]
    let _c = websocket();
}
