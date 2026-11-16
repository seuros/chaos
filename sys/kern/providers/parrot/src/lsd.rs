//! LSD (LLM Service Daemon) native inference adapter: `POST /inference`, the
//! `lsd-protocol` contract. It speaks to the LSD gateway and to `chaos serve`
//! alike.
//!
//! Trips keep the conversation server-side: once a reply is stored, the next
//! request of the turn carries only the messages after it. A rewritten
//! history, or a server that answers `trip_not_found`, gets the whole
//! conversation again under the same trip id.

mod request;
mod stream;

use crate::provider::Provider;
use chaos_abi::AbiError;
use chaos_abi::AdapterFuture;
use chaos_abi::Continuation;
use chaos_abi::ContinuationSlot;
use chaos_abi::ModelAdapter;
use chaos_abi::TurnEvent;
use chaos_abi::TurnRequest;
use chaos_abi::TurnStream;
use chaos_libration::UsageSniffer;
use lsd_protocol::ErrorBody;
use lsd_protocol::ErrorCode;
use lsd_protocol::InferenceRequest;
use lsd_protocol::InferenceResponseChunk;
use lsd_protocol::Role;
use rama::bytes::Bytes;
use rama::error::BoxError;
use rama::futures;
use rama::http::HeaderMap;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use stream::ChunkFolder;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct LsdAdapter {
    provider: Provider,
    api_key: String,
    default_target: Option<String>,
    sniffer: Option<Arc<UsageSniffer>>,
}

impl LsdAdapter {
    pub fn new(provider: Provider, api_key: String, default_target: Option<String>) -> Self {
        Self {
            provider,
            api_key,
            default_target,
            sniffer: None,
        }
    }

    pub fn with_sniffer(mut self, sniffer: Option<Arc<UsageSniffer>>) -> Self {
        self.sniffer = sniffer;
        self
    }

    pub fn from_base_url_and_api_key(
        base_url: String,
        api_key: String,
        default_target: Option<String>,
    ) -> Self {
        let provider =
            Provider::from_base_url_with_default_streaming_config("LSD", base_url, false);
        Self::new(provider, api_key, default_target)
    }

    fn target_for_request(&self, request_model: &str) -> String {
        if request_model.is_empty() {
            self.default_target
                .clone()
                .unwrap_or_else(|| "default".to_string())
        } else {
            request_model.to_string()
        }
    }

    fn build_headers(&self) -> Result<HeaderMap, AbiError> {
        let mut headers = self.provider.headers.clone();
        if !self.api_key.trim().is_empty() {
            crate::http_helpers::insert_bearer_auth(&mut headers, &self.api_key, "LSD")?;
        }
        crate::http_helpers::insert_streaming_json_headers(&mut headers);
        Ok(headers)
    }
}

impl ModelAdapter for LsdAdapter {
    fn stream(&self, request: TurnRequest) -> AdapterFuture<'_> {
        Box::pin(async move {
            let target = self.target_for_request(&request.model);
            let full = request::build_request(&request, &target)?;
            let call = TripCall {
                url: self.provider.url_for_path(lsd_protocol::INFERENCE_PATH),
                headers: self.build_headers()?,
                provider: self.provider.clone(),
                sniffer: self.sniffer.clone(),
                slot: request.continuation.clone(),
            };
            let (tx, rx) = mpsc::channel(64);
            tokio::spawn(async move {
                // The kernel matches the reply's model against what it asked
                // for; the variant the server routed to is its own detail.
                let _ = tx.send(Ok(TurnEvent::ServerModel(target))).await;
                if let Err(error) = call.run(full, tx.clone()).await {
                    let _ = tx.send(Err(error)).await;
                }
            });
            Ok(TurnStream { rx_event: rx })
        })
    }

    fn provider_name(&self) -> &str {
        "LSD"
    }

    fn capabilities(&self) -> chaos_abi::AdapterCapabilities {
        chaos_abi::AdapterCapabilities {
            can_list_models: false,
        }
    }
}

struct TripCall {
    url: String,
    headers: HeaderMap,
    provider: Provider,
    sniffer: Option<Arc<UsageSniffer>>,
    slot: Option<ContinuationSlot>,
}

impl TripCall {
    async fn run(
        &self,
        full: InferenceRequest,
        tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
    ) -> Result<(), AbiError> {
        let stored = self.slot.as_ref().and_then(ContinuationSlot::get);
        let body = plan(&full, stored.as_ref());
        match self.send(&body, &full, tx.clone()).await {
            Err(error) if body.continue_trip && is_trip_not_found(&error) => {
                let mut replay = full.clone();
                replay.trip_id = body.trip_id;
                self.send(&replay, &full, tx).await
            }
            other => other,
        }
    }

    async fn send(
        &self,
        body: &InferenceRequest,
        full: &InferenceRequest,
        tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
    ) -> Result<(), AbiError> {
        let json = serde_json::to_value(body).map_err(|error| AbiError::InvalidRequest {
            message: error.to_string(),
        })?;
        let response = crate::sse::transport::start_rama_post_sse_request(
            &self.url,
            &self.headers,
            &json,
            &self.provider,
            "lsd",
            self.sniffer.as_ref(),
        )
        .await
        .map_err(protocol_error)?;
        let stored_trip = process_sse_data_stream(
            response.into_body().into_data_stream(),
            self.provider.stream_idle_timeout,
            tx,
        )
        .await?;
        if let (Some(slot), Some(trip_id)) = (&self.slot, stored_trip) {
            let prefix_len = full.input.messages.len();
            slot.set(Continuation {
                token: trip_id.to_string(),
                prefix_len,
                fingerprint: request::fingerprint(full, prefix_len),
            });
        }
        Ok(())
    }
}

/// Continue the trip when this turn's history still extends what the server
/// holds, sending only what came after the server's stored reply; otherwise
/// send everything under the same trip id.
fn plan(full: &InferenceRequest, stored: Option<&Continuation>) -> InferenceRequest {
    let mut body = full.clone();
    let Some(stored) = stored else {
        return body;
    };
    body.trip_id = stored.token.parse().ok();
    let messages = &full.input.messages;
    if stored.prefix_len > messages.len()
        || request::fingerprint(full, stored.prefix_len) != stored.fingerprint
    {
        return body;
    }
    let fresh: Vec<_> = messages[stored.prefix_len..]
        .iter()
        .skip_while(|message| message.role == Role::Assistant)
        .cloned()
        .collect();
    if !fresh.is_empty() {
        body.continue_trip = true;
        body.input.messages = fresh;
    }
    body
}

fn is_trip_not_found(error: &AbiError) -> bool {
    matches!(
        error,
        AbiError::Transport { status: 404, message }
            if error_body(message).is_some_and(|body| body.is_trip_not_found())
    )
}

fn error_body(message: &str) -> Option<ErrorBody> {
    serde_json::from_str(message).ok()
}

/// Failures the contract names become typed ABI errors. The trip-loss 404 is
/// left as transport so the caller can replay; anything unnamed keeps its
/// status for the kernel's retry policy.
fn protocol_error(error: AbiError) -> AbiError {
    let AbiError::Transport { status, message } = &error else {
        return error;
    };
    let Some(body) = error_body(message) else {
        return error;
    };
    match body.error_code {
        ErrorCode::RateLimited => AbiError::Retryable {
            message: body.error,
            delay: body.retry_after_ms.map(Duration::from_millis),
        },
        ErrorCode::Overloaded => AbiError::ServerOverloaded,
        ErrorCode::InvalidRequest | ErrorCode::UnknownTarget => AbiError::InvalidRequest {
            message: body.error,
        },
        ErrorCode::TripNotFound => error,
        _ => AbiError::Transport {
            status: *status,
            message: body.error,
        },
    }
}

/// Streams one inference into `tx`. Returns the trip id once the reply is
/// finished, which is when the server stores the turn.
pub(crate) async fn process_sse_data_stream<S, E>(
    data_stream: S,
    idle_timeout: Duration,
    tx: mpsc::Sender<Result<TurnEvent, AbiError>>,
) -> Result<Option<uuid::Uuid>, AbiError>
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    E: Into<BoxError> + std::fmt::Display,
{
    let mut folder = ChunkFolder::default();
    let mut stored_trip = None;
    crate::sse::process_json_sse(data_stream, idle_timeout, tx, |json, response_id| {
        if let Some(message) = json.get("error").and_then(Value::as_str) {
            return Err(match error_body(&json.to_string()) {
                Some(body) => protocol_error(AbiError::Transport {
                    status: body.error_code.status(),
                    message: json.to_string(),
                }),
                None => AbiError::Transport {
                    status: 500,
                    message: message.to_string(),
                },
            });
        }
        let chunk: InferenceResponseChunk =
            serde_json::from_value(json.clone()).map_err(|error| {
                AbiError::Stream(format!("LSD sent a chunk outside the protocol: {error}"))
            })?;
        let (events, finished) = folder.fold(chunk, response_id)?;
        if let Some(finished) = finished {
            stored_trip = Some(finished.trip_id);
        }
        Ok(events)
    })
    .await?;
    Ok(stored_trip)
}

#[cfg(test)]
mod tests;
