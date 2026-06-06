use std::convert::Infallible;
use std::sync::Arc;

use rama::Service;
use rama::http::Body;
use rama::http::Method;
use rama::http::Request;
use rama::http::Response;
use rama::http::StatusCode;
use rama::http::body::util::BodyExt;
use rama::service::service_fn;
use tracing::{Instrument, error, info_span, warn};

use crate::ServerState;
use crate::auth;
use crate::monitor;
use crate::protocol::{ApiErrorResponse, HealthResponse, TriggerRequest, TriggerResponse};
use crate::runner;

/// Build a cloneable HTTP service backed by shared server state.
pub(crate) fn http_service(
    state: Arc<ServerState>,
) -> impl Service<Request, Output = Response, Error = Infallible> + Clone {
    service_fn(move |request: Request| {
        let state = state.clone();
        async move { Ok::<_, Infallible>(handle(state, request).await) }
    })
}

async fn handle(state: Arc<ServerState>, request: Request) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();

    match (method.clone(), path.as_str()) {
        (Method::GET, "/monitor") => monitor::page_response(),
        (Method::GET, "/monitor/events") => monitor::events_response(state),
        (Method::GET, "/assets/datastar.js") => monitor::datastar_script_response(),
        (Method::GET, "/api/health") => handle_health(),
        (Method::POST, "/api/trigger") => handle_trigger(state, request).await,
        // Known routes, wrong method.
        (_, "/monitor") | (_, "/monitor/events") => method_not_allowed("GET"),
        (_, "/assets/datastar.js") => method_not_allowed("GET"),
        (_, "/api/health") => method_not_allowed("GET"),
        (_, "/api/trigger") => method_not_allowed("POST"),
        // Unknown route.
        _ => json_response(StatusCode::NOT_FOUND, &ApiErrorResponse::error("not found")),
    }
}

fn handle_health() -> Response {
    json_response(
        StatusCode::OK,
        &HealthResponse {
            status: "ok",
            version: chaos_ipc::product::CHAOS_VERSION,
        },
    )
}

async fn handle_trigger(state: Arc<ServerState>, request: Request) -> Response {
    // Auth check.
    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());
    if let Err(msg) = auth::validate_bearer(auth_header, &state.bearer_token) {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header("content-type", "application/json")
            .header("www-authenticate", "Bearer")
            .body(Body::from(
                serde_json::to_vec(&ApiErrorResponse::error(msg)).unwrap_or_default(),
            ))
            .unwrap();
    }

    // Content-Type check: accept "application/json" with optional params
    // (e.g. "application/json; charset=utf-8") but not subtypes like
    // "application/json-patch+json".
    let content_type = request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mime_base = content_type.split(';').next().unwrap_or("").trim();
    if !mime_base.eq_ignore_ascii_case("application/json") {
        return json_response(
            StatusCode::BAD_REQUEST,
            &ApiErrorResponse::error("Content-Type must be application/json"),
        );
    }

    // Content-Length pre-check: reject before buffering when the header
    // advertises a body larger than the configured limit.
    if let Some(cl) = request.headers().get("content-length")
        && let Ok(len) = cl.to_str().unwrap_or("0").parse::<usize>()
        && len > state.body_limit
    {
        return json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            &ApiErrorResponse::error("request body too large"),
        );
    }

    // Collect body bytes.
    let body_bytes = match request.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            error!(error = %e, "failed to read request body");
            return json_response(
                StatusCode::BAD_REQUEST,
                &ApiErrorResponse::error("failed to read request body"),
            );
        }
    };

    // Post-collection size guard (Content-Length can be absent or lying).
    if body_bytes.len() > state.body_limit {
        return json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            &ApiErrorResponse::error("request body too large"),
        );
    }

    // Deserialize.
    let trigger_req: TriggerRequest = match serde_json::from_slice(&body_bytes) {
        Ok(req) => req,
        Err(e) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &ApiErrorResponse::error(format!("invalid JSON: {e}")),
            );
        }
    };

    // Resolve correlation fields before validation so all error responses
    // can include them for upstream correlation.
    let caller_session_id = trigger_req.caller_session_id.clone();
    let conversation_id = trigger_req
        .conversation_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Validate: reject model override.
    if trigger_req.model.is_some() {
        return json_response(
            StatusCode::BAD_REQUEST,
            &ApiErrorResponse::error("per-request model selection is not supported")
                .with_caller_fields(caller_session_id, Some(conversation_id)),
        );
    }

    // Validate: require non-empty request.
    match trigger_req.request.as_deref() {
        None | Some("") => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &ApiErrorResponse::error("missing required field: request")
                    .with_caller_fields(caller_session_id, Some(conversation_id)),
            );
        }
        Some(s) if s.trim().is_empty() => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &ApiErrorResponse::error("missing required field: request")
                    .with_caller_fields(caller_session_id, Some(conversation_id)),
            );
        }
        _ => {}
    }

    // Acquire concurrency permit.
    let _permit = match state.semaphore.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return json_response(
                StatusCode::TOO_MANY_REQUESTS,
                &ApiErrorResponse::error("too many concurrent requests")
                    .with_caller_fields(caller_session_id, Some(conversation_id)),
            );
        }
    };
    state.monitor.publish(
        monitor::MonitorEventKind::TriggerAccepted,
        Some(conversation_id.clone()),
        None,
        trigger_req.requested_by.clone(),
    );

    let span = info_span!(
        "trigger",
        http.method = "POST",
        http.route = "/api/trigger",
        conversation_id = %conversation_id,
        caller_session_id = trigger_req.caller_session_id.as_deref().unwrap_or(""),
        requested_by = trigger_req.requested_by.as_deref().unwrap_or(""),
        process_id = tracing::field::Empty,
    );

    let config = state.config.as_ref().clone();
    let process_table = state.process_table.clone();
    let timeout = state.timeout;

    async move {
        // Single wall-clock deadline covering both process start and execution.
        let deadline = tokio::time::Instant::now() + timeout;

        // Start the process under the shared deadline.
        let started =
            match tokio::time::timeout_at(deadline, runner::start(&process_table, config)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    error!(error = %e, "failed to start trigger process");
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &ApiErrorResponse::error("internal server error").with_caller_fields(
                            caller_session_id.clone(),
                            Some(conversation_id.clone()),
                        ),
                    );
                }
                Err(_) => {
                    warn!(
                        "trigger timed out after {}s (during start)",
                        timeout.as_secs()
                    );
                    return json_response(
                        StatusCode::GATEWAY_TIMEOUT,
                        &ApiErrorResponse::timeout(format!(
                            "execution exceeded {}s deadline",
                            timeout.as_secs()
                        ))
                        .with_caller_fields(caller_session_id, Some(conversation_id)),
                    );
                }
            };
        state.monitor.publish(
            monitor::MonitorEventKind::ProcessStarted,
            Some(conversation_id.clone()),
            Some(started.process_id.to_string()),
            None,
        );

        tracing::Span::current().record("process_id", started.process_id.to_string());

        // Execute under the same deadline (remaining time).
        let response = match tokio::time::timeout_at(
            deadline,
            runner::execute(&started, &trigger_req, &conversation_id),
        )
        .await
        {
            Ok(Ok(outcome)) => {
                if outcome.is_error {
                    state.monitor.publish(
                        monitor::MonitorEventKind::TriggerFailed,
                        Some(conversation_id.clone()),
                        Some(outcome.process_id.to_string()),
                        Some(outcome.text.clone()),
                    );
                    error!(
                        process_id = %outcome.process_id,
                        error = %outcome.text,
                        "trigger process returned error",
                    );
                    json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &ApiErrorResponse::error("internal server error")
                            .with_process_id(outcome.process_id)
                            .with_caller_fields(
                                caller_session_id.clone(),
                                Some(conversation_id.clone()),
                            ),
                    )
                } else {
                    state.monitor.publish(
                        monitor::MonitorEventKind::TriggerCompleted,
                        Some(conversation_id.clone()),
                        Some(outcome.process_id.to_string()),
                        None,
                    );
                    json_response(
                        StatusCode::OK,
                        &TriggerResponse {
                            status: "ok",
                            caller_session_id,
                            conversation_id: Some(conversation_id.clone()),
                            process_id: outcome.process_id.to_string(),
                            result: outcome.text,
                            usage: outcome.usage,
                        },
                    )
                }
            }
            Ok(Err(e)) => {
                state.monitor.publish(
                    monitor::MonitorEventKind::TriggerFailed,
                    Some(conversation_id.clone()),
                    Some(started.process_id.to_string()),
                    Some(e.to_string()),
                );
                error!(error = %e, "trigger runner failed");
                json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &ApiErrorResponse::error("internal server error")
                        .with_process_id(started.process_id)
                        .with_caller_fields(
                            caller_session_id.clone(),
                            Some(conversation_id.clone()),
                        ),
                )
            }
            Err(_elapsed) => {
                state.monitor.publish(
                    monitor::MonitorEventKind::TriggerTimedOut,
                    Some(conversation_id.clone()),
                    Some(started.process_id.to_string()),
                    Some(format!(
                        "execution exceeded {}s deadline",
                        timeout.as_secs()
                    )),
                );
                warn!("trigger timed out after {}s", timeout.as_secs());
                json_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    &ApiErrorResponse::timeout(format!(
                        "execution exceeded {}s deadline",
                        timeout.as_secs()
                    ))
                    .with_process_id(started.process_id)
                    .with_caller_fields(caller_session_id.clone(), Some(conversation_id.clone())),
                )
            }
        };

        // Always clean up the process — success, error, or timeout.
        // Cleanup internally bounds the shutdown to avoid blocking forever.
        const CLEANUP_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
        runner::cleanup(&process_table, &started, CLEANUP_GRACE).await;
        state.monitor.publish(
            monitor::MonitorEventKind::ProcessCleanedUp,
            Some(conversation_id),
            Some(started.process_id.to_string()),
            None,
        );

        response
    }
    .instrument(span)
    .await
}

/// 405 response with the correct `Allow` header.
fn method_not_allowed(allowed: &str) -> Response {
    let json =
        serde_json::to_vec(&ApiErrorResponse::error("method not allowed")).unwrap_or_default();
    Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .header("content-type", "application/json")
        .header("allow", allowed)
        .body(Body::from(json))
        .unwrap()
}

fn json_response<T: serde::Serialize>(status: StatusCode, body: &T) -> Response {
    let json = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(json))
        .unwrap()
}
