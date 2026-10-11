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

use std::{pin::Pin, sync::Arc};

use ahash::AHashMap;
use futures_util::{stream::SplitSink, SinkExt, Stream, StreamExt};
use reqwest::header::SEC_WEBSOCKET_PROTOCOL;
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    ClientConfig, SignatureScheme,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, Message},
    Connector, MaybeTlsStream, WebSocketStream,
};

use crate::{
    client::Client,
    core::{
        error::{ProblemDetails, ProblemType},
        request::{Arguments, Request},
        response::{Response, TaggedMethodResponse},
    },
    DataType, Method, PushObject, URI,
};

#[derive(Debug, Serialize)]
struct WebSocketRequest {
    #[serde(rename = "@type")]
    pub _type: WebSocketRequestType,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    using: Vec<URI>,

    #[serde(rename = "methodCalls")]
    method_calls: Vec<(Method, Arguments, String)>,

    #[serde(rename = "createdIds")]
    #[serde(skip_serializing_if = "Option::is_none")]
    created_ids: Option<AHashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
pub struct WebSocketResponse {
    #[serde(rename = "@type")]
    _type: WebSocketResponseType,

    #[serde(rename = "requestId")]
    request_id: Option<String>,

    #[serde(rename = "methodResponses")]
    method_responses: Vec<TaggedMethodResponse>,

    #[serde(rename = "createdIds")]
    created_ids: Option<AHashMap<String, String>>,

    #[serde(rename = "sessionState")]
    session_state: String,
}

#[derive(Debug, Serialize, Deserialize)]
enum WebSocketResponseType {
    Response,
}

#[derive(Debug, Serialize)]
struct WebSocketPushEnable {
    #[serde(rename = "@type")]
    _type: WebSocketPushEnableType,

    #[serde(rename = "dataTypes")]
    data_types: Option<Vec<DataType>>,

    #[serde(rename = "pushState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    push_state: Option<String>,
}

#[derive(Debug, Serialize)]
struct WebSocketPushDisable {
    #[serde(rename = "@type")]
    _type: WebSocketPushDisableType,
}

#[derive(Debug, Serialize)]
enum WebSocketRequestType {
    Request,
}

#[derive(Debug, Serialize)]
enum WebSocketPushEnableType {
    WebSocketPushEnable,
}

#[derive(Debug, Serialize)]
enum WebSocketPushDisableType {
    WebSocketPushDisable,
}

#[derive(Deserialize, Debug)]
pub struct WebSocketPushObject {
    #[serde(flatten)]
    pub push: PushObject,

    #[serde(rename = "pushState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub push_state: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WebSocketError {
    #[serde(rename = "@type")]
    pub type_: WebSocketErrorType,

    #[serde(rename = "requestId")]
    pub request_id: Option<String>,

    #[serde(rename = "type")]
    p_type: ProblemType,
    status: Option<u32>,
    title: Option<String>,
    detail: Option<String>,
    limit: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum WebSocketErrorType {
    RequestError,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WebSocketMessage_ {
    Response(WebSocketResponse),
    PushNotification(WebSocketPushObject),
    Error(WebSocketError),
}

#[derive(Debug)]
pub enum WebSocketMessage {
    Response(Response<TaggedMethodResponse>),
    PushNotification(PushObject),
}

pub struct WsStream {
    tx: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    req_id: usize,
}

#[doc(hidden)]
#[derive(Debug)]
struct DummyVerifier;

impl ServerCertVerifier for DummyVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA1,
            SignatureScheme::ECDSA_SHA1_Legacy,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}

impl Client {
    pub async fn connect_ws(
        &self,
    ) -> crate::Result<Pin<Box<impl Stream<Item = crate::Result<WebSocketMessage>>>>> {
        let session = self.session();
        let capabilities = session.websocket_capabilities().ok_or_else(|| {
            crate::Error::Internal(
                "JMAP server does not advertise any websocket capabilities.".to_string(),
            )
        })?;

        let mut request = capabilities.url().into_client_request()?;
        request
            .headers_mut()
            .insert("Authorization", self.authorization.parse().unwrap());
        request
            .headers_mut()
            .insert(SEC_WEBSOCKET_PROTOCOL, "jmap".parse().unwrap());

        let (stream, _) = if self.accept_invalid_certs & capabilities.url().starts_with("wss") {
            tokio_tungstenite::connect_async_tls_with_config(
                request,
                None,
                false,
                Connector::Rustls(Arc::new(
                    ClientConfig::builder()
                        .dangerous()
                        .with_custom_certificate_verifier(Arc::new(DummyVerifier {}))
                        .with_no_client_auth(),
                ))
                .into(),
            )
            .await?
        } else {
            tokio_tungstenite::connect_async(request).await?
        };
        let (tx, mut rx) = stream.split();

        *self.ws.lock().await = WsStream { tx, req_id: 0 }.into();

        Ok(Box::pin(async_stream::stream! {
            while let Some(message) = rx.next().await {
                match message {
                    Ok(message) if message.is_text() => {
                        match serde_json::from_slice::<WebSocketMessage_>(&message.into_data()) {
                            Ok(message) => match message {
                                WebSocketMessage_::Response(response) => {
                                    yield Ok(WebSocketMessage::Response(Response::new(
                                        response.method_responses,
                                        response.created_ids,
                                        response.session_state,
                                        response.request_id,
                                    )))
                                }
                                WebSocketMessage_::PushNotification(push) => {
                                    yield Ok(WebSocketMessage::PushNotification(push.push))
                                }
                                WebSocketMessage_::Error(err) => yield Err(ProblemDetails::from(err).into()),
                            },
                            Err(err) => yield Err(err.into()),
                        }
                    }
                    Ok(_) => (),
                    Err(err) => yield Err(err.into()),
                }
            }
        }))
    }

    pub async fn send_ws(&self, request: Request<'_>) -> crate::Result<String> {
        let mut _ws = self.ws.lock().await;
        let ws = _ws
            .as_mut()
            .ok_or_else(|| crate::Error::Internal("Websocket stream not set.".to_string()))?;

        // Assign request id
        let request_id = ws.req_id.to_string();
        ws.req_id += 1;

        ws.tx
            .send(Message::text(
                serde_json::to_string(&WebSocketRequest {
                    _type: WebSocketRequestType::Request,
                    id: request_id.clone().into(),
                    using: request.using,
                    method_calls: request.method_calls,
                    created_ids: request.created_ids,
                })
                .unwrap_or_default(),
            ))
            .await?;

        Ok(request_id)
    }

    pub async fn enable_push_ws(
        &self,
        data_types: Option<impl IntoIterator<Item = DataType>>,
        push_state: Option<impl Into<String>>,
    ) -> crate::Result<()> {
        self.ws
            .lock()
            .await
            .as_mut()
            .ok_or_else(|| crate::Error::Internal("Websocket stream not set.".to_string()))?
            .tx
            .send(Message::text(
                serde_json::to_string(&WebSocketPushEnable {
                    _type: WebSocketPushEnableType::WebSocketPushEnable,
                    data_types: data_types.map(|it| it.into_iter().collect()),
                    push_state: push_state.map(|it| it.into()),
                })
                .unwrap_or_default(),
            ))
            .await
            .map_err(|err| err.into())
    }

    pub async fn disable_push_ws(&self) -> crate::Result<()> {
        self.ws
            .lock()
            .await
            .as_mut()
            .ok_or_else(|| crate::Error::Internal("Websocket stream not set.".to_string()))?
            .tx
            .send(Message::text(
                serde_json::to_string(&WebSocketPushDisable {
                    _type: WebSocketPushDisableType::WebSocketPushDisable,
                })
                .unwrap_or_default(),
            ))
            .await
            .map_err(|err| err.into())
    }

    pub async fn ws_ping(&self) -> crate::Result<()> {
        self.ws
            .lock()
            .await
            .as_mut()
            .ok_or_else(|| crate::Error::Internal("Websocket stream not set.".to_string()))?
            .tx
            .send(Message::Ping(vec![].into()))
            .await
            .map_err(|err| err.into())
    }
}

impl From<WebSocketError> for ProblemDetails {
    fn from(problem: WebSocketError) -> Self {
        ProblemDetails::new(
            problem.p_type,
            problem.status,
            problem.title,
            problem.detail,
            problem.limit,
            problem.request_id,
        )
    }
}
