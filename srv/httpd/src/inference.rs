//! `POST /inference`: the native LSD inference contract (`lsd-protocol`),
//! served on chaos's configured providers. The caller owns the conversation
//! and runs its own tools; chaos keeps no trips yet, so `continue_trip`
//! always answers `trip_not_found` and the client replays.

mod convert;

use std::convert::Infallible;
use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_kern::inference::Inference;
use chaos_kern::inference::InferenceError;
use chaos_kern::inference::InferenceInput;
use lsd_protocol::ErrorBody;
use lsd_protocol::ErrorCode;
use lsd_protocol::InferenceRequest;
use lsd_protocol::InferenceResponse;
use lsd_protocol::InferenceResponseChunk;
use rama::futures::StreamExt;
use rama::futures::async_stream::stream_fn;
use rama::http::{Body, HeaderValue, Request, Response, StatusCode};
use tokio::time::Instant;
use uuid::Uuid;

use crate::ServerState;
use crate::ingress;
use convert::Projection;
use convert::Step;

pub(crate) async fn handle(state: Arc<ServerState>, request: Request) -> Response {
    let deadline = Instant::now() + state.timeout;
    if let Err(error) = ingress::validate_json_headers(&request, state.body_limit) {
        return error_response(body_code(&error), error.message());
    }
    let Ok(permit) = state.semaphore.clone().try_acquire_owned() else {
        return error_response(ErrorCode::RateLimited, "too many concurrent requests");
    };
    let bytes = match ingress::read_body(request.into_body(), state.body_limit, deadline).await {
        Ok(bytes) => bytes,
        Err(error) => return error_response(body_code(&error), error.message()),
    };
    let wire: InferenceRequest = match serde_json::from_slice(&bytes) {
        Ok(wire) => wire,
        Err(error) => {
            return error_response(
                ErrorCode::InvalidRequest,
                &format!("invalid request: {error}"),
            );
        }
    };
    let trip_id = wire.trip_id.unwrap_or_else(Uuid::new_v4);
    if wire.continue_trip {
        return error_response(
            ErrorCode::TripNotFound,
            &format!("chaos keeps no trips; replay trip {trip_id} in full"),
        );
    }
    let turn = match convert::turn(&wire) {
        Ok(turn) => turn,
        Err(message) => return error_response(ErrorCode::InvalidRequest, &message),
    };
    let (provider_id, model) = match target(&wire) {
        Ok(target) => target,
        Err(message) => return error_response(ErrorCode::InvalidRequest, message),
    };
    let input = InferenceInput {
        conversation_id: ProcessId::from_string(&trip_id.to_string()).unwrap_or_default(),
        provider_id,
        model,
        instructions: turn.instructions,
        input: turn.input,
        tools: turn.tools,
        parallel_tool_calls: turn.parallel_tool_calls,
        effort: turn.effort,
        summary: turn.summary,
        output_schema: wire.output_schema.clone(),
    };
    let started = tokio::time::timeout_at(
        deadline,
        chaos_kern::inference::infer(
            &state.config,
            &state.process_table,
            &state.auth_manager,
            input,
        ),
    )
    .await;
    let inference = match started {
        Err(_) => return error_response(ErrorCode::Timeout, "inference deadline exceeded"),
        Ok(Err(InferenceError::UnknownTarget(message))) => {
            return error_response(ErrorCode::UnknownTarget, &message);
        }
        Ok(Err(InferenceError::InvalidRequest(message))) => {
            return error_response(ErrorCode::InvalidRequest, &message);
        }
        Ok(Err(InferenceError::Provider(error))) => {
            return error_response(ErrorCode::UpstreamError, &error.to_string());
        }
        Ok(Ok(inference)) => inference,
    };
    let reply = Reply {
        inference_id: Uuid::new_v4(),
        trip_id,
        variant_name: format!("{}::{}", inference.provider_id, inference.model),
        deadline,
    };
    if wire.streams() {
        reply.stream(inference, permit)
    } else {
        let response = reply.collect(inference).await;
        drop(permit);
        response
    }
}

/// `model_name` is `<provider>::<model>`; a bare name, or a `function_name`
/// (chaos has no functions), is a model on the default provider.
fn target(wire: &InferenceRequest) -> Result<(Option<String>, String), &'static str> {
    match (&wire.function_name, &wire.model_name) {
        (Some(_), Some(_)) => Err("send function_name or model_name, not both"),
        (None, None) => Err("send function_name or model_name"),
        (Some(name), None) => Ok((None, name.clone())),
        (None, Some(name)) => Ok(match name.split_once("::") {
            Some((provider, model)) => (Some(provider.to_string()), model.to_string()),
            None => (None, name.clone()),
        }),
    }
}

struct Reply {
    inference_id: Uuid,
    trip_id: Uuid,
    variant_name: String,
    deadline: Instant,
}

impl Reply {
    fn chunk(&self, content: Vec<lsd_protocol::ContentBlockChunk>) -> InferenceResponseChunk {
        InferenceResponseChunk::Chat {
            inference_id: self.inference_id,
            trip_id: self.trip_id,
            variant_name: self.variant_name.clone(),
            content,
            usage: None,
            finish_reason: None,
            stop_details: None,
            raw_usage: None,
        }
    }

    fn stream(self, inference: Inference, permit: tokio::sync::OwnedSemaphorePermit) -> Response {
        let mut events = inference.stream;
        let body = stream_fn(async move |mut yielder| {
            let _permit = permit;
            let mut projection = Projection::default();
            loop {
                let next = match tokio::time::timeout_at(self.deadline, events.next()).await {
                    Err(_) => Some(Err(ErrorBody::new(
                        ErrorCode::Timeout,
                        "inference deadline exceeded",
                    ))),
                    Ok(None) => Some(Err(ErrorBody::new(
                        ErrorCode::UpstreamError,
                        "the provider stream ended before the reply finished",
                    ))),
                    Ok(Some(Err(error))) => Some(Err(ErrorBody::new(
                        ErrorCode::UpstreamError,
                        error.to_string(),
                    ))),
                    Ok(Some(Ok(event))) => match projection.step(event) {
                        Step::Content(content) => Some(Ok(self.chunk(content))),
                        Step::Finished {
                            usage,
                            finish_reason,
                        } => {
                            let mut last = self.chunk(Vec::new());
                            if let InferenceResponseChunk::Chat {
                                usage: slot_usage,
                                finish_reason: slot_reason,
                                ..
                            } = &mut last
                            {
                                *slot_usage = usage;
                                *slot_reason = Some(finish_reason);
                            }
                            yielder.yield_item(Ok::<_, Infallible>(sse(&last))).await;
                            break;
                        }
                        Step::Nothing => None,
                    },
                };
                match next {
                    Some(Ok(chunk)) => yielder.yield_item(Ok(sse(&chunk))).await,
                    Some(Err(error)) => {
                        yielder.yield_item(Ok(sse(&error))).await;
                        break;
                    }
                    None => {}
                }
            }
            yielder
                .yield_item(Ok(format!("data: {}\n\n", lsd_protocol::SSE_DONE)))
                .await;
        });
        let mut response = Response::new(Body::from_stream(body));
        let headers = response.headers_mut();
        headers.insert(
            "content-type",
            HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        headers.insert("cache-control", HeaderValue::from_static("no-store"));
        headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
        response
    }

    async fn collect(self, inference: Inference) -> Response {
        let mut events = inference.stream;
        let mut projection = Projection::default();
        loop {
            let event = match tokio::time::timeout_at(self.deadline, events.next()).await {
                Err(_) => return error_response(ErrorCode::Timeout, "inference deadline exceeded"),
                Ok(None) => {
                    return error_response(
                        ErrorCode::UpstreamError,
                        "the provider stream ended before the reply finished",
                    );
                }
                Ok(Some(Err(error))) => {
                    return error_response(ErrorCode::UpstreamError, &error.to_string());
                }
                Ok(Some(Ok(event))) => event,
            };
            if let Step::Finished {
                usage,
                finish_reason,
            } = projection.step(event)
            {
                return json(
                    StatusCode::OK,
                    &InferenceResponse::Chat {
                        inference_id: self.inference_id,
                        trip_id: self.trip_id,
                        variant_name: self.variant_name,
                        content: projection.output,
                        usage: usage.unwrap_or_default(),
                        finish_reason: Some(finish_reason),
                        stop_details: None,
                        raw_usage: None,
                    },
                );
            }
        }
    }
}

fn sse<T: serde::Serialize>(payload: &T) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(payload).unwrap_or_else(|_| "{}".into())
    )
}

fn body_code(error: &ingress::BodyError) -> ErrorCode {
    match error {
        ingress::BodyError::Deadline => ErrorCode::Timeout,
        ingress::BodyError::ContentType
        | ingress::BodyError::TooLarge
        | ingress::BodyError::ReadFailed => ErrorCode::InvalidRequest,
    }
}

pub(crate) fn error_response(code: ErrorCode, message: &str) -> Response {
    json(
        StatusCode::from_u16(code.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        &ErrorBody::new(code, message),
    )
}

fn json<T: serde::Serialize>(status: StatusCode, body: &T) -> Response {
    let mut response = Response::new(Body::from(
        serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec()),
    ));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    response
}

#[cfg(test)]
mod tests;
