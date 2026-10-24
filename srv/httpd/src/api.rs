use std::convert::Infallible;
use std::sync::Arc;

use rama::Service;
use rama::http::Body;
use rama::http::HeaderValue;
use rama::http::Method;
use rama::http::Request;
use rama::http::Response;
use rama::http::StatusCode;
use rama::service::service_fn;
use tracing::{Instrument, error, info_span, warn};

use crate::ServerState;
use crate::auth;
use crate::ingress::{self, BodyError};
use crate::monitor;
use crate::protocol::{ApiErrorResponse, HealthResponse, TriggerRequest, TriggerResponse};
use crate::runner;

const TRIGGER_PATH: &str = "/api/trigger";
const MONITOR_EVENTS_PATH: &str = "/monitor/events";
const INTERNAL_SERVER_ERROR: &str = "internal server error";

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
    let path_cow = request.uri().path().map(|path| path.as_encoded_str());
    let path = path_cow.as_deref().unwrap_or("");

    // The SDK owns protocol admission for every method, including OPTIONS and
    // unsupported methods. Never use the legacy bearer/header/body path here.
    #[cfg(feature = "mcpd")]
    if path == "/mcp"
        && let Some(mcp) = &state.mcp
    {
        // Bound POST ingress across MCP and existing execution APIs before the
        // SDK can collect any body. GET/DELETE/OPTIONS retain control access
        // during saturation; the SDK separately bounds sessions/SSE streams.
        // Release at response creation, not when its lazy SSE body terminates.
        let _permit = if method == Method::POST {
            match state.semaphore.clone().try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    return json_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        &ApiErrorResponse::error("too many concurrent requests"),
                    );
                }
            }
        } else {
            None
        };
        return mcp.handle(request).await;
    }

    // The monitor page is a static login shell; only authenticated callers may
    // subscribe to live state. Authenticate before touching either request body.
    if matches!(path, TRIGGER_PATH | MONITOR_EVENTS_PATH)
        || path == "/api/sessions"
        || path.starts_with("/api/sessions/")
    {
        let auth_header = request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok());
        if let Err(msg) = auth::validate_bearer(auth_header, &state.bearer_token) {
            let mut response =
                json_response(StatusCode::UNAUTHORIZED, &ApiErrorResponse::error(msg));
            response
                .headers_mut()
                .insert("www-authenticate", HeaderValue::from_static("Bearer"));
            return response;
        }
        if state
            .sessions
            .as_ref()
            .is_some_and(|manager| manager.recovery_only())
        {
            let operator = request
                .headers()
                .get("x-chaos-recovery-authorization")
                .and_then(|v| v.to_str().ok());
            if state
                .recovery_token
                .as_deref()
                .is_none_or(|token| auth::validate_bearer(operator, token).is_err())
            {
                return json_response(
                    StatusCode::FORBIDDEN,
                    &ApiErrorResponse::error("operator recovery credential required"),
                );
            }
            if path == TRIGGER_PATH {
                return json_response(
                    StatusCode::CONFLICT,
                    &ApiErrorResponse::error("recovery-only mode cannot execute work"),
                );
            }
        }
    }

    if path == "/api/sessions" || path.starts_with("/api/sessions/") {
        return crate::sessions::http::handle(state, request).await;
    }

    match (method.clone(), path) {
        (Method::GET, "/monitor") => monitor::page_response(),
        (Method::GET, MONITOR_EVENTS_PATH) => monitor::events_response(state),
        (Method::GET, "/assets/datastar.js") => monitor::datastar_script_response(),
        (Method::GET, "/api/health") => handle_health(),
        (Method::POST, TRIGGER_PATH) => handle_trigger(state, request).await,
        // Known routes, wrong method.
        (_, "/monitor") | (_, MONITOR_EVENTS_PATH) => method_not_allowed("GET"),
        (_, "/assets/datastar.js") => method_not_allowed("GET"),
        (_, "/api/health") => method_not_allowed("GET"),
        (_, TRIGGER_PATH) => method_not_allowed("POST"),
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
    if let Some(manager) = &state.sessions
        && manager.check_execution().is_err()
    {
        return json_response(
            StatusCode::CONFLICT,
            &ApiErrorResponse::error("session execution policy unavailable or changed"),
        );
    }
    let deadline = tokio::time::Instant::now() + state.timeout;

    if let Err(error) = ingress::validate_json_headers(&request, state.body_limit) {
        return body_error_response(error);
    }

    // Bound ingress as well as agent execution. Do not buffer a body while
    // waiting for capacity, and keep the permit until process cleanup finishes.
    let _permit = match state.semaphore.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return json_response(
                StatusCode::TOO_MANY_REQUESTS,
                &ApiErrorResponse::error("too many concurrent requests"),
            );
        }
    };

    let body_bytes = match ingress::read_body(request.into_body(), state.body_limit, deadline).await
    {
        Ok(bytes) => bytes,
        Err(error) => return body_error_response(error),
    };

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
    if !trigger_req
        .request
        .as_deref()
        .is_some_and(|request| !request.trim().is_empty())
    {
        return json_response(
            StatusCode::BAD_REQUEST,
            &ApiErrorResponse::error("missing required field: request")
                .with_caller_fields(caller_session_id, Some(conversation_id)),
        );
    }

    state.monitor.publish(
        monitor::MonitorEventKind::TriggerAccepted,
        Some(conversation_id.clone()),
        None,
        trigger_req.requested_by.clone(),
    );

    let span = info_span!(
        "trigger",
        http.method = "POST",
        http.route = TRIGGER_PATH,
        conversation_id = %conversation_id,
        caller_session_id = trigger_req.caller_session_id.as_deref().unwrap_or(""),
        requested_by = trigger_req.requested_by.as_deref().unwrap_or(""),
        process_id = tracing::field::Empty,
    );

    let config = state
        .sessions
        .as_ref()
        .and_then(|manager| manager.pinned_config())
        .unwrap_or_else(|| state.config.clone())
        .as_ref()
        .clone();
    let auth = state
        .sessions
        .as_ref()
        .and_then(|manager| manager.pinned_auth())
        .unwrap_or_else(|| state.auth_manager.clone());
    let process_table = state.process_table.clone();
    let timeout = state.timeout;

    async move {
        if let Some(manager) = &state.sessions
            && manager.check_execution().is_err()
        {
            return json_response(
                StatusCode::CONFLICT,
                &ApiErrorResponse::error("session execution policy unavailable or changed"),
            );
        }
        // Body reading, process start, and execution share one deadline.
        let started =
            match tokio::time::timeout_at(deadline, runner::start(&process_table, config, auth))
                .await
            {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    error!(error = %e, "failed to start trigger process");
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &ApiErrorResponse::error(INTERNAL_SERVER_ERROR).with_caller_fields(
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
        let response = match tokio::time::timeout_at(deadline, async {
            if let Some(manager) = &state.sessions {
                manager.check_execution().map_err(|_| {
                    anyhow::anyhow!("session execution policy unavailable or changed")
                })?;
            }
            runner::execute(&started, &trigger_req, &conversation_id).await
        })
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
                    execution_error_response(
                        outcome.process_id,
                        caller_session_id.clone(),
                        conversation_id.clone(),
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
                execution_error_response(
                    started.process_id,
                    caller_session_id.clone(),
                    conversation_id.clone(),
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

/// Return the same sanitized execution failure with caller correlation.
fn execution_error_response(
    process_id: chaos_ipc::ProcessId,
    caller_session_id: Option<String>,
    conversation_id: String,
) -> Response {
    json_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        &ApiErrorResponse::error(INTERNAL_SERVER_ERROR)
            .with_process_id(process_id)
            .with_caller_fields(caller_session_id, Some(conversation_id)),
    )
}

/// Preserve the legacy timeout status for body-read deadlines.
fn body_error_response(error: BodyError) -> Response {
    let body = if matches!(error, BodyError::Deadline) {
        ApiErrorResponse::timeout(error.message())
    } else {
        ApiErrorResponse::error(error.message())
    };
    json_response(error.status(), &body)
}

/// 405 response with the correct `Allow` header.
fn method_not_allowed(allowed: &'static str) -> Response {
    let mut response = json_response(
        StatusCode::METHOD_NOT_ALLOWED,
        &ApiErrorResponse::error("method not allowed"),
    );
    response
        .headers_mut()
        .insert("allow", HeaderValue::from_static(allowed));
    response
}

fn json_response<T: serde::Serialize>(status: StatusCode, body: &T) -> Response {
    let json = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let mut response = Response::new(Body::from(json));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    response
}

#[cfg(test)]
mod tests;
