use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use rama::Service;
use rama::http::Body;
use rama::http::Method;
use rama::http::Request;
use rama::http::Response;
use rama::http::StatusCode;
use rama::http::body::util::BodyExt;
use rama::service::service_fn;
use tracing::error;

use crate::CreateProcessResponse;
use crate::ErrorCode;
use crate::ErrorPayload;
use crate::GetProcessRequest;
use crate::GetProcessResponse;
use crate::HeartbeatLeaseRequest;
use crate::JournalError;
use crate::JournalRequest;
use crate::JournalResponse;
use crate::JournalStore;
use crate::ListProcessesRequest;
use crate::ListProcessesResponse;
use crate::LoadJournalRequest;
use crate::ReleaseLeaseRequest;
use crate::ReleaseLeaseResponse;
use crate::RequestEnvelope;
use crate::ResponseEnvelope;
use crate::model::HelloResponse;
use crate::protocol::AcquireLeaseRequest;
use crate::protocol::GetDefaultProcessRequest;
use crate::protocol::GetDefaultProcessResponse;
use crate::protocol::SetDefaultProcessRequest;
use crate::protocol::SetDefaultProcessResponse;

pub const JOURNAL_RPC_PATH: &str = "/rpc";
pub const SERVER_NAME: &str = "chaos_journald";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PROTOCOL_VERSION: u32 = 5;

pub struct JournalRpcServer<S> {
    store: Arc<S>,
    backend: &'static str,
}

impl<S> Clone for JournalRpcServer<S> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            backend: self.backend,
        }
    }
}

impl<S> JournalRpcServer<S>
where
    S: JournalStore + Send + Sync + 'static,
{
    pub fn new(store: Arc<S>, backend: &'static str) -> Self {
        Self { store, backend }
    }

    pub fn http_service(
        self,
    ) -> impl Service<Request, Output = Response, Error = Infallible> + Clone {
        service_fn(move |request: Request| {
            let server = self.clone();
            async move { Ok::<_, Infallible>(server.handle_http(request).await) }
        })
    }

    pub async fn handle_http(&self, request: Request) -> Response {
        let method = request.method().clone();
        let path = request
            .uri()
            .path()
            .map(rama::net::uri::PathRef::as_encoded_str)
            .unwrap_or(std::borrow::Cow::Borrowed(""));

        if method != Method::POST {
            return text_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
        }

        if path != JOURNAL_RPC_PATH {
            return text_response(StatusCode::NOT_FOUND, "not found");
        }

        let body = match request.into_body().collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(err) => {
                error!("failed reading journal RPC body: {err}");
                return text_response(StatusCode::BAD_REQUEST, "invalid request body");
            }
        };

        let envelope: RequestEnvelope = match serde_json::from_slice(&body) {
            Ok(envelope) => envelope,
            Err(err) => {
                error!("failed parsing journal RPC envelope: {err}");
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &ResponseEnvelope {
                        id: String::new(),
                        ok: false,
                        result: None,
                        error: Some(ErrorPayload {
                            code: ErrorCode::Internal,
                            message: format!("invalid request envelope: {err}"),
                            retryable: false,
                        }),
                    },
                );
            }
        };

        let response = self.dispatch(envelope).await;
        json_response(StatusCode::OK, &response)
    }

    async fn dispatch(&self, envelope: RequestEnvelope) -> ResponseEnvelope {
        let request_id = envelope.id;
        let outcome = match envelope.request {
            JournalRequest::Hello(_request) => Ok(JournalResponse::Hello(HelloResponse {
                server_name: SERVER_NAME.to_string(),
                protocol_version: PROTOCOL_VERSION,
                server_version: SERVER_VERSION.to_string(),
                backend: self.backend.to_string(),
            })),
            JournalRequest::CreateProcess(input) => {
                self.store.create_process(input).await.map(|process| {
                    JournalResponse::CreateProcess(CreateProcessResponse {
                        process_id: process.process_id,
                        next_seq: 0,
                    })
                })
            }
            JournalRequest::InitializeProcess(input) => self
                .store
                .initialize_process(input)
                .await
                .map(|result| JournalResponse::InitializeProcess(Box::new(result))),
            JournalRequest::GetProcess(GetProcessRequest { process_id }) => {
                self.store.get_process(&process_id).await.map(|process| {
                    JournalResponse::GetProcess(Box::new(GetProcessResponse { process }))
                })
            }
            JournalRequest::ListProcesses(ListProcessesRequest { archived }) => self
                .store
                .list_processes(archived)
                .await
                .map(|items| JournalResponse::ListProcesses(ListProcessesResponse { items })),
            JournalRequest::AcquireLease(AcquireLeaseRequest {
                process_id,
                owner_id,
                ttl_ms,
            }) => self
                .store
                .acquire_lease(&process_id, &owner_id, duration_from_millis(ttl_ms))
                .await
                .map(JournalResponse::AcquireLease),
            JournalRequest::HeartbeatLease(HeartbeatLeaseRequest {
                process_id,
                owner_id,
                lease_token,
                ttl_ms,
            }) => self
                .store
                .heartbeat_lease(
                    &process_id,
                    &owner_id,
                    &lease_token,
                    duration_from_millis(ttl_ms),
                )
                .await
                .map(JournalResponse::HeartbeatLease),
            JournalRequest::ReleaseLease(ReleaseLeaseRequest {
                process_id,
                owner_id,
                lease_token,
            }) => self
                .store
                .release_lease(&process_id, &owner_id, &lease_token)
                .await
                .map(|()| JournalResponse::ReleaseLease(ReleaseLeaseResponse {})),
            JournalRequest::AppendBatch(input) => self
                .store
                .append_batch(input)
                .await
                .map(JournalResponse::AppendBatch),
            JournalRequest::LoadJournal(LoadJournalRequest { process_id }) => self
                .store
                .load_journal(&process_id)
                .await
                .map(JournalResponse::LoadJournal),
            JournalRequest::GetDefaultProcess(GetDefaultProcessRequest {}) => {
                self.store.get_default_process().await.map(|process_id| {
                    JournalResponse::GetDefaultProcess(GetDefaultProcessResponse { process_id })
                })
            }
            JournalRequest::SetDefaultProcess(SetDefaultProcessRequest { process_id }) => self
                .store
                .set_default_process(&process_id)
                .await
                .map(|()| JournalResponse::SetDefaultProcess(SetDefaultProcessResponse {})),
        };

        match outcome {
            Ok(result) => ResponseEnvelope {
                id: request_id,
                ok: true,
                result: Some(result),
                error: None,
            },
            Err(err) => ResponseEnvelope {
                id: request_id,
                ok: false,
                result: None,
                error: Some(error_payload_for(err)),
            },
        }
    }
}

fn duration_from_millis(value: u64) -> Duration {
    Duration::from_millis(value)
}

pub(crate) fn error_payload_for(error: JournalError) -> ErrorPayload {
    match error {
        JournalError::ProcessNotFound(_) => ErrorPayload {
            code: ErrorCode::NotFound,
            message: error.to_string(),
            retryable: false,
        },
        JournalError::ProcessAlreadyExists(_) => ErrorPayload {
            code: ErrorCode::AlreadyExists,
            message: error.to_string(),
            retryable: false,
        },
        JournalError::LeaseConflict { .. } => ErrorPayload {
            code: ErrorCode::LeaseConflict,
            message: error.to_string(),
            retryable: true,
        },
        JournalError::LeaseExpired { .. } => ErrorPayload {
            code: ErrorCode::LeaseExpired,
            message: error.to_string(),
            retryable: true,
        },
        JournalError::InvalidLease { .. } => ErrorPayload {
            code: ErrorCode::InvalidLease,
            message: error.to_string(),
            retryable: false,
        },
        JournalError::SequenceConflict { .. } => ErrorPayload {
            code: ErrorCode::SequenceConflict,
            message: error.to_string(),
            retryable: true,
        },
        JournalError::InvalidRequest(_) => ErrorPayload {
            code: ErrorCode::InvalidRequest,
            message: error.to_string(),
            retryable: false,
        },
        JournalError::Db(ref err) => ErrorPayload {
            code: ErrorCode::Internal,
            message: error.to_string(),
            retryable: is_retryable_db_error(err),
        },
        JournalError::Migrate(_)
        | JournalError::Io(_)
        | JournalError::Serialize { .. }
        | JournalError::Deserialize { .. }
        | JournalError::InvalidProcessId { .. }
        | JournalError::InvalidTimestamp { .. } => ErrorPayload {
            code: ErrorCode::Internal,
            message: error.to_string(),
            retryable: false,
        },
    }
}

fn is_retryable_db_error(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(db_error) => {
            db_error.code().as_deref().is_some_and(|code| {
                matches!(
                    code,
                    "5" | "SQLITE_BUSY" | "SQLITE_LOCKED" | "40001" | "40P01" | "55P03"
                )
            }) || db_error.message().contains("database is locked")
        }
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => true,
        _ => false,
    }
}

fn text_response(status: StatusCode, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::from(body.to_string())))
}

fn json_response<T: serde::Serialize>(status: StatusCode, value: &T) -> Response {
    let body = match serde_json::to_string(value) {
        Ok(body) => body,
        Err(err) => {
            error!("failed to serialize journal RPC response: {err}");
            "{}".to_string()
        }
    };

    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::from("{}")))
}

#[cfg(test)]
mod tests;
