//! Minimal WebSocket server handshake utility for test mocks.
//!
//! Replaces `tokio-tungstenite::accept_async` / `accept_hdr_async_with_config`
//! so tests can use rama's `AsyncWebSocket` and `Message` types without pulling
//! in tungstenite as a direct dependency.

use base64::Engine;
use rama::http::ws::protocol::{Role, WebSocketConfig};
use rama::http::ws::AsyncWebSocket;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// The stream type returned by the accept helpers.
pub type WsStream = AsyncWebSocket<rama::tcp::TcpStream>;

/// RFC 6455 magic GUID appended to `Sec-WebSocket-Key` before hashing.
const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-5ADF5B3F4A84";

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
    F: FnOnce(&WsHandshakeRequest) -> Vec<(String, String)>,
{
    let mut reader = BufReader::new(stream);

    // Read the HTTP request line.
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .await
        .expect("read request line");

    // Extract URI from "GET /path HTTP/1.1\r\n".
    let uri = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();

    // Read headers until the blank line.
    let mut ws_key = None;
    let mut has_deflate = false;
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("read header line");
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name_trimmed = name.trim();
            let value_trimmed = value.trim().trim_end_matches(['\r', '\n']);
            headers.push((name_trimmed.to_string(), value_trimmed.to_string()));
            if name_trimmed.eq_ignore_ascii_case("sec-websocket-key") {
                ws_key = Some(value_trimmed.to_string());
            } else if name_trimmed.eq_ignore_ascii_case("sec-websocket-extensions")
                && value_trimmed.contains("permessage-deflate")
            {
                has_deflate = true;
            }
        }
    }

    let ws_key = ws_key.expect("missing Sec-WebSocket-Key header");

    let handshake_info = WsHandshakeRequest { uri, headers };

    // Compute Sec-WebSocket-Accept per RFC 6455 §4.2.2.
    let mut hasher = Sha1::new();
    hasher.update(ws_key.as_bytes());
    hasher.update(WS_GUID);
    let accept = base64::engine::general_purpose::STANDARD.encode(hasher.finalize());

    // Call the handler to get extra response headers.
    let extra_headers = on_handshake(&handshake_info);

    // Build the 101 Switching Protocols response.
    let mut response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n"
    );
    if has_deflate {
        response.push_str("Sec-WebSocket-Extensions: permessage-deflate\r\n");
    }
    for (name, value) in &extra_headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");

    // Drain the BufReader, preserving any buffered data.
    let remaining = reader.buffer().to_vec();
    let mut stream = reader.into_inner();
    stream
        .write_all(response.as_bytes())
        .await
        .expect("write 101 response");

    // Wrap in rama's TcpStream for ExtensionsMut support.
    let stream = rama::tcp::TcpStream::new(stream);

    let ws_config = if has_deflate {
        Some(config.unwrap_or_default().with_per_message_deflate_default())
    } else {
        config
    };

    let ws = if remaining.is_empty() {
        AsyncWebSocket::from_raw_socket(stream, Role::Server, ws_config).await
    } else {
        AsyncWebSocket::from_partially_read(stream, remaining, Role::Server, ws_config).await
    };

    (ws, handshake_info)
}
