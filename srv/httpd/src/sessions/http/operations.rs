//! Operator-only financial/archive evidence; never execution admission.
use super::*;
use chaos_journald::{
    ExecutionBillingRequest, ExecutionPriceSchedule, ExecutionReconciliation, ExecutionRequest,
    ExecutionResponse, ExecutionRetentionRequest,
};
use tokio::sync::OwnedSemaphorePermit;

#[derive(serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ArchiveMutation {
    Export,
    Seal {
        manifest_digest: String,
        archive_reference: String,
        actor: String,
    },
}

pub(super) fn is_route(parts: &[&str]) -> bool {
    matches!(
        parts,
        ["api", "sessions", "prices", _]
            | ["api", "sessions", _, "turns", _, "billing"]
            | ["api", "sessions", _, "archive"]
    )
}

pub(super) async fn handle(
    state: &Arc<ServerState>,
    manager: &Arc<SessionManager>,
    request: Request,
    permit: Option<OwnedSemaphorePermit>,
) -> Result<Response, ApiError> {
    let header = request
        .headers()
        .get("x-chaos-recovery-authorization")
        .and_then(|value| value.to_str().ok());
    if state
        .recovery_token
        .as_deref()
        .is_none_or(|token| crate::auth::validate_bearer(header, token).is_err())
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "operator credential required",
        ));
    }
    let store = manager
        .store
        .as_ref()
        .ok_or_else(|| ApiError::conflict("operator evidence requires durable sessions"))?
        .clone();
    let method = request.method().clone();
    let uri = request.uri().to_string();
    if uri.contains('?') {
        return Err(ApiError::bad_request(
            "operator evidence routes do not accept query parameters",
        ));
    }
    let parts: Vec<_> = uri.trim_start_matches('/').split('/').collect();
    if let [_, _, session, "archive"] = parts.as_slice() {
        validate_id(session)?;
        let session_id =
            Uuid::parse_str(session).map_err(|_| ApiError::bad_request("invalid session ID"))?;
        let archive = match method {
            Method::GET => ExecutionRetentionRequest::Read {
                scope: store.scope.clone(),
                session_id,
            },
            Method::POST => match read_json::<ArchiveMutation>(state, request).await? {
                ArchiveMutation::Export => ExecutionRetentionRequest::Export {
                    scope: store.scope.clone(),
                    session_id,
                },
                ArchiveMutation::Seal {
                    manifest_digest,
                    archive_reference,
                    actor,
                } => ExecutionRetentionRequest::Seal {
                    scope: store.scope.clone(),
                    session_id,
                    manifest_digest,
                    archive_reference,
                    actor,
                },
            },
            _ => return Ok(method_not_allowed("GET, POST")),
        };
        archive
            .validate()
            .map_err(|_| ApiError::bad_request("invalid archive evidence"))?;
        let result = tokio::spawn(async move {
            let _permit = permit;
            store
                .call(ExecutionRequest::Retention { request: archive })
                .await
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "archive evidence owner unavailable",
            )
        })??;
        let ExecutionResponse::Retention(result) = result else {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "archive evidence unavailable",
            ));
        };
        return Ok(response(
            StatusCode::OK,
            json!({
                "daemon_id": manager.daemon_id, "retention": result,
            }),
        ));
    }
    let billing = match parts.as_slice() {
        [_, _, "prices", version] => match method {
            Method::GET => ExecutionBillingRequest::GetPrice {
                scope: store.scope.clone(),
                version: (*version).into(),
            },
            Method::PUT => {
                let schedule: ExecutionPriceSchedule = read_json(state, request).await?;
                if schedule.version != *version {
                    return Err(ApiError::bad_request("price version must match its path"));
                }
                ExecutionBillingRequest::PutPrice {
                    scope: store.scope.clone(),
                    schedule,
                }
            }
            _ => return Ok(method_not_allowed("GET, PUT")),
        },
        [_, _, session, "turns", turn, "billing"] => {
            validate_id(session)?;
            validate_id(turn)?;
            let session_id = Uuid::parse_str(session)
                .map_err(|_| ApiError::bad_request("invalid session ID"))?;
            let turn_id =
                Uuid::parse_str(turn).map_err(|_| ApiError::bad_request("invalid turn ID"))?;
            match method {
                Method::GET => ExecutionBillingRequest::Read {
                    scope: store.scope.clone(),
                    session_id,
                    turn_id,
                },
                Method::POST => {
                    let record: ExecutionReconciliation = read_json(state, request).await?;
                    ExecutionBillingRequest::Reconcile {
                        scope: store.scope.clone(),
                        session_id,
                        turn_id,
                        record,
                    }
                }
                _ => return Ok(method_not_allowed("GET, POST")),
            }
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "operator evidence route unavailable",
            ));
        }
    };
    billing
        .validate()
        .map_err(|_| ApiError::bad_request("invalid billing evidence or price schedule"))?;
    let result = tokio::spawn(async move {
        // Once dispatched, a disconnect must not cancel an ambiguous evidence write.
        let _permit = permit;
        store
            .call(ExecutionRequest::Billing { request: billing })
            .await
    })
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing evidence owner unavailable",
        )
    })??;
    let ExecutionResponse::Billing(result) = result else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing evidence unavailable",
        ));
    };
    Ok(response(
        StatusCode::OK,
        json!({ "daemon_id": manager.daemon_id, "billing": result }),
    ))
}

#[cfg(test)]
mod tests;
