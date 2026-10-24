use std::sync::Arc;

use rama::http::{Body, HeaderValue, Method, Request, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::json;
use uuid::Uuid;

use super::{
    ApiError, CreateRequest, NoticeRequest, Session, SessionManager, SessionStatus, SteerRequest,
    TurnRequest,
};
use crate::ServerState;
use crate::ingress;

mod operations;

pub(crate) async fn handle(state: Arc<ServerState>, request: Request) -> Response {
    let Some(manager) = state.sessions.clone() else {
        return response(
            StatusCode::NOT_FOUND,
            json!({"error": "session API disabled"}),
        );
    };
    match route(state, &manager, request).await {
        Ok(value) => value,
        Err(error) => response(
            error.code,
            json!({"status": "error", "error": error.message, "daemon_id": manager.daemon_id}),
        ),
    }
}

async fn route(
    state: Arc<ServerState>,
    manager: &Arc<SessionManager>,
    request: Request,
) -> Result<Response, ApiError> {
    let method = request.method().clone();
    let uri = request.uri().to_string();
    let (path, query) = uri.split_once('?').unwrap_or((&uri, ""));
    let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
    // Financial routes are registered only for the direct PostgreSQL backend.
    // Resolve absence before generation checks, capacity acquisition or bodies.
    if operations::is_billing_route(&parts)
        && !manager
            .store
            .as_ref()
            .is_some_and(|store| store.billing_enabled())
    {
        return Ok(response(
            StatusCode::NOT_FOUND,
            json!({"error": "not found"}),
        ));
    }
    if manager.recovery_only()
        && method != Method::GET
        && !(parts.len() == 4 && parts[3] == "recovery" && method == Method::POST)
        && !(operations::is_route(&parts) && matches!(method, Method::POST | Method::PUT))
    {
        return Err(ApiError::conflict(
            "recovery-only mode cannot mutate execution",
        ));
    }

    // Every mutation is conditional on the daemon generation. An application
    // that loses an accepted response cannot silently replay it after restart.
    let _ingress = if method != Method::GET {
        manager.ensure_running()?;
        let expected = request
            .headers()
            .get("x-chaos-daemon-id")
            .and_then(|v| v.to_str().ok());
        match expected {
            None => {
                return Err(ApiError::new(
                    StatusCode::PRECONDITION_REQUIRED,
                    "X-Chaos-Daemon-ID is required",
                ));
            }
            Some(expected) if expected != manager.daemon_id => {
                return Err(ApiError::conflict(
                    "daemon generation changed; reconcile before submitting",
                ));
            }
            _ => {}
        }
        Some(
            manager
                .ingress
                .clone()
                .try_acquire_owned()
                .map_err(|_| ApiError::capacity("too many control requests"))?,
        )
    } else {
        None
    };

    if operations::is_route(&parts) {
        return operations::handle(&state, manager, request, _ingress).await;
    }

    if parts.len() == 2 {
        return match method {
            Method::GET => Ok(response(
                StatusCode::OK,
                json!({
                    "daemon_id": manager.daemon_id,
                    "sessions": manager.snapshots(),
                }),
            )),
            Method::POST => {
                let create: CreateRequest = read_json(&state, request).await?;
                if state.config.ephemeral && create.resume_process_id.is_some() {
                    return Err(ApiError::bad_request("cannot resume with --ephemeral"));
                }
                let create_manager = manager.clone();
                let admission_permit = _ingress;
                let session = tokio::spawn(async move {
                    let _permit = admission_permit;
                    create_manager.create(state, create).await
                })
                .await
                .map_err(|_| {
                    ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "session owner unavailable")
                })??;
                Ok(session_response(StatusCode::ACCEPTED, manager, &session))
            }
            _ => Ok(method_not_allowed("GET, POST")),
        };
    }
    if parts.as_slice() == ["api", "sessions", "deliveries"] {
        if method != Method::GET {
            return Ok(method_not_allowed("GET"));
        }
        let store = manager
            .store
            .as_ref()
            .ok_or_else(|| ApiError::conflict("deliveries require durable sessions"))?;
        let after = if query.is_empty() {
            None
        } else {
            Some(
                query
                    .strip_prefix("after=")
                    .ok_or_else(|| ApiError::bad_request("expected after UUID"))?
                    .parse::<Uuid>()
                    .map_err(|_| ApiError::bad_request("expected after UUID"))?,
            )
        };
        let deliveries = match store
            .call(chaos_journald::ExecutionRequest::Delivery {
                request: chaos_journald::ExecutionDeliveryRequest::List {
                    scope: store.scope.clone(),
                    after,
                    limit: 256,
                },
            })
            .await?
        {
            chaos_journald::ExecutionResponse::Deliveries(deliveries) => deliveries,
            _ => return Err(ApiError::conflict("delivery records unavailable")),
        };
        return Ok(response(
            StatusCode::OK,
            json!({
                "daemon_id": manager.daemon_id,
                "next_cursor": deliveries.last().map(|d| d.event_id),
                "deliveries": deliveries,
            }),
        ));
    }
    let Some(id) = parts.get(2) else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not found"));
    };
    validate_id(id)?;
    let session = manager.get(id)?;
    let result = match parts.as_slice() {
        [_, _, _] => match method {
            Method::GET => session_response(StatusCode::OK, manager, &session),
            Method::DELETE => {
                let status = session.data.lock().unwrap().status;
                if matches!(status, SessionStatus::Closed | SessionStatus::Failed)
                    && manager.store.is_none()
                {
                    manager.forget(id)?;
                    response(
                        StatusCode::OK,
                        json!({"daemon_id": manager.daemon_id, "status": "forgotten"}),
                    )
                } else {
                    if status == SessionStatus::Closed && manager.store.is_some() {
                        return Ok(session_response(StatusCode::OK, manager, &session));
                    }
                    session.close()?;
                    session_response(StatusCode::ACCEPTED, manager, &session)
                }
            }
            _ => method_not_allowed("GET, DELETE"),
        },
        [_, _, _, "events"] => {
            if method != Method::GET {
                return Ok(method_not_allowed("GET"));
            }
            let last_event_id = request
                .headers()
                .get("last-event-id")
                .and_then(|value| value.to_str().ok());
            let after = if query.is_empty() {
                match last_event_id {
                    Some(value) => value
                        .parse::<u64>()
                        .map_err(|_| ApiError::bad_request("invalid Last-Event-ID"))?,
                    None => 0,
                }
            } else {
                query
                    .strip_prefix("after=")
                    .and_then(|value| value.parse::<u64>().ok())
                    .ok_or(ApiError::bad_request("expected an unsigned after cursor"))?
            };
            let wants_stream = request
                .headers()
                .get("accept")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| {
                    value
                        .split(',')
                        .any(|mime| mime.trim() == "text/event-stream")
                });
            if wants_stream {
                // Validate before committing SSE response headers.
                session.read_events(after).await?;
                return Ok(super::stream::response(
                    session,
                    manager.daemon_id.clone(),
                    after,
                    manager.shutdown.subscribe(),
                ));
            }
            response(
                StatusCode::OK,
                json!({
                    "daemon_id": manager.daemon_id, "session_id": id,
                    "replay": session.read_events(after).await?,
                }),
            )
        }
        [_, _, _, "turns", turn_id, "accounting"] => {
            if method != Method::GET {
                return Ok(method_not_allowed("GET"));
            }
            validate_id(turn_id)?;
            let owner = session
                .ledger
                .get()
                .ok_or_else(|| ApiError::conflict("accounting requires durable sessions"))?;
            let accounting = owner
                .accounting(
                    Uuid::parse_str(turn_id)
                        .map_err(|_| ApiError::bad_request("invalid turn UUID"))?,
                )
                .await?;
            response(
                StatusCode::OK,
                json!({
                    "daemon_id": manager.daemon_id, "session_id": id, "turn_id": turn_id,
                    "accounting": accounting,
                }),
            )
        }
        [_, _, _, "recovery"] => match method {
            Method::GET => response(
                StatusCode::OK,
                json!({
                    "daemon_id": manager.daemon_id,
                    "session_id": id,
                    "recovery_enabled": state.recovery_token.is_some(),
                    "recovery": session.recovery_view().await?,
                }),
            ),
            Method::POST => {
                let token = state.recovery_token.as_deref().ok_or_else(|| {
                    ApiError::new(StatusCode::NOT_FOUND, "recovery mutations disabled")
                })?;
                crate::auth::validate_bearer(
                    request
                        .headers()
                        .get("x-chaos-recovery-authorization")
                        .and_then(|value| value.to_str().ok()),
                    token,
                )
                .map_err(|_| {
                    ApiError::new(
                        StatusCode::FORBIDDEN,
                        "operator recovery credential required",
                    )
                })?;
                let resolution: super::durable::recovery::ResolveRequest =
                    read_json(&state, request).await?;
                let resolving = session.clone();
                let admission_permit = _ingress;
                let resolution = tokio::spawn(async move {
                    let _permit = admission_permit;
                    resolving.resolve(resolution).await
                })
                .await
                .map_err(|_| {
                    ApiError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "resolution acknowledgement unavailable; reconcile state",
                    )
                })??;
                response(
                    StatusCode::OK,
                    json!({
                        "daemon_id": manager.daemon_id,
                        "session": session.snapshot(),
                        "resolution": resolution,
                    }),
                )
            }
            _ => method_not_allowed("GET, POST"),
        },
        [_, _, _, "turns", turn_id] => {
            validate_id(turn_id)?;
            let turn = match method {
                Method::GET => session.read_turn(turn_id).await?,
                Method::POST => {
                    let turn: TurnRequest = read_json(&state, request).await?;
                    let session = session.clone();
                    let id = turn_id.to_string();
                    let admission_permit = _ingress;
                    tokio::spawn(async move {
                        let _permit = admission_permit;
                        session
                            .admit(id, turn, &state.semaphore, state.timeout)
                            .await
                    })
                    .await
                    .map_err(|_| {
                        ApiError::new(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "turn owner unavailable; reconcile state",
                        )
                    })??
                }
                _ => return Ok(method_not_allowed("GET, POST")),
            };
            response(
                if method == Method::POST {
                    StatusCode::ACCEPTED
                } else {
                    StatusCode::OK
                },
                json!({"daemon_id": manager.daemon_id, "session_id": id, "turn": turn}),
            )
        }
        [_, _, _, "turns", turn_id, action] => {
            validate_id(turn_id)?;
            if *action == "notice" {
                let (notice, withdraw_id) = match method {
                    Method::POST => (
                        Some(read_json::<NoticeRequest>(&state, request).await?),
                        None,
                    ),
                    Method::DELETE if query.is_empty() => (None, None),
                    Method::DELETE => {
                        let id = query
                            .strip_prefix("id=")
                            .filter(|id| super::valid_notice_id(id))
                            .ok_or(ApiError::bad_request(
                                "expected ?id=<notice id> (A-Z a-z 0-9 - . _ ~)",
                            ))?;
                        (None, Some(id.to_string()))
                    }
                    _ => return Ok(method_not_allowed("POST, DELETE")),
                };
                let posting = notice.is_some();
                // A retried POST is a no-op for an id the session has already
                // seen; a DELETE with ?id= clears only that notice. Neither can
                // disturb a newer notice, so both may be retried after a lost
                // acknowledgement.
                let changed = tokio::time::timeout(
                    state.timeout,
                    session.notice(turn_id.to_string(), notice, withdraw_id),
                )
                .await
                .map_err(|_| {
                    ApiError::new(
                        StatusCode::GATEWAY_TIMEOUT,
                        "notice acknowledgement unavailable; reconcile before retrying",
                    )
                })??;
                let status = match (posting, changed) {
                    (true, true) => "notice_set",
                    (true, false) => "notice_already_seen",
                    (false, true) => "notice_withdrawn",
                    (false, false) => "notice_not_pending",
                };
                return Ok(response(
                    StatusCode::OK,
                    json!({"daemon_id": manager.daemon_id, "turn_id": turn_id, "status": status}),
                ));
            }
            if method != Method::POST {
                return Ok(method_not_allowed("POST"));
            }
            match *action {
                "steer" => {
                    let steer: SteerRequest = read_json(&state, request).await?;
                    // Once queued, disconnect does not withdraw steering. No
                    // blind retry: steering is not an idempotent operation.
                    tokio::time::timeout(
                        state.timeout,
                        session.steer(turn_id.to_string(), steer.request),
                    )
                    .await
                    .map_err(|_| {
                        ApiError::new(
                            StatusCode::GATEWAY_TIMEOUT,
                            "steering acknowledgement unavailable; delivery may have occurred",
                        )
                    })??;
                    response(
                        StatusCode::OK,
                        json!({
                            "daemon_id": manager.daemon_id,
                            "turn_id": turn_id,
                            "status": "steered",
                        }),
                    )
                }
                "interrupt" => {
                    let turn = session.interrupt(turn_id.to_string())?;
                    response(
                        StatusCode::ACCEPTED,
                        json!({"daemon_id": manager.daemon_id, "session_id": id, "turn": turn}),
                    )
                }
                _ => return Err(ApiError::new(StatusCode::NOT_FOUND, "not found")),
            }
        }
        _ => return Err(ApiError::new(StatusCode::NOT_FOUND, "not found")),
    };
    Ok(result)
}

fn validate_id(id: &str) -> Result<(), ApiError> {
    if Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
        Ok(())
    } else {
        Err(ApiError::bad_request("expected a canonical UUID"))
    }
}

async fn read_json<T: DeserializeOwned>(
    state: &ServerState,
    request: Request,
) -> Result<T, ApiError> {
    ingress::validate_json_headers(&request, state.body_limit)?;
    let deadline = tokio::time::Instant::now() + state.timeout;
    let bytes = ingress::read_body(request.into_body(), state.body_limit, deadline).await?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::bad_request("invalid JSON request"))
}

fn session_response(status: StatusCode, manager: &SessionManager, session: &Session) -> Response {
    response(
        status,
        json!({"daemon_id": manager.daemon_id, "session": session.snapshot()}),
    )
}

fn response(status: StatusCode, body: serde_json::Value) -> Response {
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

fn method_not_allowed(allow: &'static str) -> Response {
    let mut value = response(
        StatusCode::METHOD_NOT_ALLOWED,
        json!({"error": "method not allowed"}),
    );
    value
        .headers_mut()
        .insert("allow", HeaderValue::from_static(allow));
    value
}

#[cfg(test)]
mod tests;
