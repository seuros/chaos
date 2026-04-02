//! Minimal WebSocket server handshake utility for test mocks.
//!
//! Uses `tokio-tungstenite` for a real server-side handshake so reconnect tests
//! stop depending on our toy RFC implementation.

use std::sync::Arc;
use std::sync::Mutex;

use futures::SinkExt;
use futures::StreamExt;
use http::HeaderName;
use http::HeaderValue;
use tokio::net::TcpStream;
use tokio_tungstenite::accept_hdr_async_with_config;
pub use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::ErrorResponse;
use tokio_tungstenite::tungstenite::handshake::server::Request;
use tokio_tungstenite::tungstenite::handshake::server::Response;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// The stream type returned by the accept helpers.
pub struct WsStream(tokio_tungstenite::WebSocketStream<TcpStream>);

impl WsStream {
    pub async fn recv_message(&mut self) -> Result<Message, tokio_tungstenite::tungstenite::Error> {
        match self.0.next().await {
            Some(result) => result,
            None => Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed),
        }
    }

    pub async fn send_message(
        &mut self,
        message: Message,
    ) -> Result<(), tokio_tungstenite::tungstenite::Error> {
        self.0.send(message).await
    }

    pub async fn close(
        &mut self,
        frame: Option<CloseFrame>,
    ) -> Result<(), tokio_tungstenite::tungstenite::Error> {
        self.0.close(frame).await
    }
}

/// Parsed information from the client's HTTP upgrade request.
pub struct WsHandshakeRequest {
    pub uri: String,
    pub headers: Vec<(String, String)>,
}

/// Accept a WebSocket upgrade on a raw TCP stream (no header inspection).
pub async fn accept_ws(stream: TcpStream) -> WsStream {
    let (ws, _) = accept_ws_with_handler(stream, None, |_| Vec::new()).await;
    ws
}

/// Accept a WebSocket upgrade with a callback that receives the parsed request
/// and returns extra response headers.
///
/// Returns the WebSocket and the parsed handshake information.
pub async fn accept_ws_with_handler<F>(
    stream: TcpStream,
    config: Option<WebSocketConfig>,
    on_handshake: F,
) -> (WsStream, WsHandshakeRequest)
where
    F: FnOnce(&WsHandshakeRequest) -> Vec<(String, String)> + Unpin,
{
    let handshake_info = Arc::new(Mutex::new(None::<WsHandshakeRequest>));
    let handshake_info_for_callback = Arc::clone(&handshake_info);

    let ws_stream = accept_hdr_async_with_config(
        stream,
        move |request: &Request, mut response: Response| -> Result<Response, ErrorResponse> {
            let info = WsHandshakeRequest {
                uri: request.uri().to_string(),
                headers: request
                    .headers()
                    .iter()
                    .filter_map(|(name, value)| {
                        value
                            .to_str()
                            .ok()
                            .map(|value| (name.as_str().to_string(), value.to_string()))
                    })
                    .collect(),
            };
            let extra_headers = on_handshake(&info);
            for (name, value) in extra_headers {
                let header_name = HeaderName::from_bytes(name.as_bytes())
                    .expect("valid websocket response header name");
                let header_value =
                    HeaderValue::from_str(&value).expect("valid websocket response header value");
                response.headers_mut().insert(header_name, header_value);
            }
            *handshake_info_for_callback.lock().unwrap() = Some(info);
            Ok(response)
        },
        config,
    )
    .await
    .expect("accept websocket");

    let handshake_info = handshake_info
        .lock()
        .unwrap()
        .take()
        .expect("websocket handshake info should be captured");
    (WsStream(ws_stream), handshake_info)
}
