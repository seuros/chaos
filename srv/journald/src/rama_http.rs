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
pub const PROTOCOL_VERSION: u32 = 2;

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
        let path = request.uri().path().to_string();

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
                server_name: "chaos-journald".to_string(),
                protocol_version: PROTOCOL_VERSION,
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

fn error_payload_for(error: JournalError) -> ErrorPayload {
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
        JournalError::Db(_)
        | JournalError::Migrate(_)
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
mod tests {
    use std::sync::Arc;

    use chaos_ipc::ProcessId;
    use chaos_ipc::protocol::SessionSource;
    use rama::http::Body;
    use rama::http::Request;
    use rama::http::StatusCode;
    use rama::http::body::util::BodyExt;
    use tempfile::tempdir;

    use super::JOURNAL_RPC_PATH;
    use super::JournalRpcServer;
    use crate::CreateProcessInput;
    use crate::HelloRequest;
    use crate::JournalRequest;
    use crate::JournalResponse;
    use crate::RequestEnvelope;
    use crate::ResponseEnvelope;
    use crate::SqliteJournalStore;

    #[tokio::test]
    async fn hello_round_trip_over_http() {
        let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let store = SqliteJournalStore::open(&temp_dir.path().join("journal.sqlite"))
            .await
            .unwrap_or_else(|err| panic!("open: {err}"));
        let server = JournalRpcServer::new(Arc::new(store), "sqlite");
        let request = Request::builder()
            .method("POST")
            .uri(JOURNAL_RPC_PATH)
            .body(Body::from(
                serde_json::to_vec(&RequestEnvelope {
                    id: "1".to_string(),
                    request: JournalRequest::Hello(HelloRequest {
                        client_name: "test".to_string(),
                        protocol_version: 1,
                    }),
                })
                .unwrap_or_else(|err| panic!("serialize request: {err}")),
            ))
            .unwrap_or_else(|err| panic!("build request: {err}"));

        let response = server.handle_http(request).await;
        assert_eq!(response.status(), StatusCode::OK);

        let body = response
            .into_body()
            .collect()
            .await
            .unwrap_or_else(|err| panic!("collect body: {err}"))
            .to_bytes();
        let envelope: ResponseEnvelope = serde_json::from_slice(&body)
            .unwrap_or_else(|err| panic!("deserialize response: {err}"));
        assert!(envelope.ok);
        match envelope.result {
            Some(JournalResponse::Hello(hello)) => {
                assert_eq!(hello.server_name, "chaos-journald");
                assert_eq!(hello.backend, "sqlite");
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_process_round_trip_over_http() {
        let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let store = SqliteJournalStore::open(&temp_dir.path().join("journal.sqlite"))
            .await
            .unwrap_or_else(|err| panic!("open: {err}"));
        let server = JournalRpcServer::new(Arc::new(store), "sqlite");
        let process_id = ProcessId::new();
        let request = Request::builder()
            .method("POST")
            .uri(JOURNAL_RPC_PATH)
            .body(Body::from(
                serde_json::to_vec(&RequestEnvelope {
                    id: "create-1".to_string(),
                    request: JournalRequest::CreateProcess(CreateProcessInput {
                        process_id,
                        parent: None,
                        source: SessionSource::Cli,
                        cwd: temp_dir.path().to_path_buf(),
                        created_at: jiff::Timestamp::now(),
                        title: Some("rpc process".to_string()),
                        model_provider: Some("openai".to_string()),
                        cli_version: Some("47.0.0".to_string()),
                    }),
                })
                .unwrap_or_else(|err| panic!("serialize request: {err}")),
            ))
            .unwrap_or_else(|err| panic!("build request: {err}"));

        let response = server.handle_http(request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap_or_else(|err| panic!("collect body: {err}"))
            .to_bytes();
        let envelope: ResponseEnvelope = serde_json::from_slice(&body)
            .unwrap_or_else(|err| panic!("deserialize response: {err}"));
        assert!(envelope.ok);
        match envelope.result {
            Some(JournalResponse::CreateProcess(result)) => {
                assert_eq!(result.process_id, process_id);
                assert_eq!(result.next_seq, 0);
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }
}
