use std::path::Path;
use std::path::PathBuf;

use rama::Layer as _;
use rama::Service;
use rama::http::Body;
use rama::http::Method;
use rama::http::Request;
use rama::http::body::util::BodyExt;
use rama::http::client::HttpConnectorLayer;
use rama::net::client::ConnectorService;
use rama::net::client::EstablishedClientConnection;
use rama::unix::client::UnixConnector;
use serde::de::DeserializeOwned;

use crate::AppendBatchInput;
use crate::AppendBatchResult;
use crate::BootstrapPaths;
use crate::CreateProcessInput;
use crate::CreateProcessResponse;
use crate::ErrorPayload;
use crate::HelloRequest;
use crate::HelloResponse;
use crate::JOURNAL_RPC_PATH;
use crate::JournalRequest;
use crate::JournalResponse;
use crate::Lease;
use crate::ListProcessesRequest;
use crate::ListProcessesResponse;
use crate::LoadJournalRequest;
use crate::LoadedJournal;
use crate::ProcessRecord;
use crate::RequestEnvelope;
use crate::ResponseEnvelope;
use crate::ensure_sqlite_journald_running;

#[derive(Debug, thiserror::Error)]
pub enum JournalClientError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("bootstrap: {0}")]
    Bootstrap(#[from] anyhow::Error),

    #[error("serialize request: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("transport: {0}")]
    Transport(String),

    #[error("http {status}: {body}")]
    HttpStatus { status: u16, body: String },

    #[error("missing response payload")]
    MissingPayload,

    #[error("unexpected response variant: expected {expected}, got {actual}")]
    UnexpectedResponseVariant {
        expected: &'static str,
        actual: &'static str,
    },

    #[error("remote: {0:?}")]
    Remote(ErrorPayload),
}

#[derive(Debug, Clone)]
pub struct JournalRpcClient {
    socket_path: PathBuf,
}

impl JournalRpcClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    pub fn socket_path(&self) -> &Path {
        self.socket_path.as_path()
    }

    pub fn from_default_socket() -> std::io::Result<Self> {
        Ok(Self::new(crate::default_socket_path()?))
    }

    pub async fn default_or_bootstrap(
        binary_path: Option<&Path>,
    ) -> Result<(Self, BootstrapPaths), JournalClientError> {
        let paths = ensure_sqlite_journald_running(binary_path).await?;
        Ok((Self::new(paths.socket_path.clone()), paths))
    }

    pub async fn hello(&self, client_name: &str) -> Result<HelloResponse, JournalClientError> {
        let response = self
            .send_request(JournalRequest::Hello(HelloRequest {
                client_name: client_name.to_string(),
                protocol_version: crate::rama_http::PROTOCOL_VERSION,
            }))
            .await?;
        match response {
            JournalResponse::Hello(hello) => Ok(hello),
            other => Err(unexpected_variant("hello", &other)),
        }
    }

    pub async fn create_process(
        &self,
        input: CreateProcessInput,
    ) -> Result<CreateProcessResponse, JournalClientError> {
        let response = self
            .send_request(JournalRequest::CreateProcess(input))
            .await?;
        match response {
            JournalResponse::CreateProcess(created) => Ok(created),
            other => Err(unexpected_variant("create_process", &other)),
        }
    }

    pub async fn get_process(
        &self,
        process_id: chaos_ipc::ProcessId,
    ) -> Result<Option<ProcessRecord>, JournalClientError> {
        let response = self
            .send_request(JournalRequest::GetProcess(crate::GetProcessRequest {
                process_id,
            }))
            .await?;
        match response {
            JournalResponse::GetProcess(process) => Ok(process.process),
            other => Err(unexpected_variant("get_process", &other)),
        }
    }

    pub async fn list_processes(
        &self,
        archived: Option<bool>,
    ) -> Result<Vec<ProcessRecord>, JournalClientError> {
        let response = self
            .send_request(JournalRequest::ListProcesses(ListProcessesRequest {
                archived,
            }))
            .await?;
        match response {
            JournalResponse::ListProcesses(ListProcessesResponse { items }) => Ok(items),
            other => Err(unexpected_variant("list_processes", &other)),
        }
    }

    pub async fn acquire_lease(
        &self,
        process_id: chaos_ipc::ProcessId,
        owner_id: String,
        ttl_ms: u64,
    ) -> Result<Lease, JournalClientError> {
        let response = self
            .send_request(JournalRequest::AcquireLease(crate::AcquireLeaseRequest {
                process_id,
                owner_id,
                ttl_ms,
            }))
            .await?;
        match response {
            JournalResponse::AcquireLease(lease) => Ok(lease),
            other => Err(unexpected_variant("acquire_lease", &other)),
        }
    }

    pub async fn heartbeat_lease(
        &self,
        process_id: chaos_ipc::ProcessId,
        owner_id: String,
        lease_token: String,
        ttl_ms: u64,
    ) -> Result<Lease, JournalClientError> {
        let response = self
            .send_request(JournalRequest::HeartbeatLease(
                crate::HeartbeatLeaseRequest {
                    process_id,
                    owner_id,
                    lease_token,
                    ttl_ms,
                },
            ))
            .await?;
        match response {
            JournalResponse::HeartbeatLease(lease) => Ok(lease),
            other => Err(unexpected_variant("heartbeat_lease", &other)),
        }
    }

    pub async fn release_lease(
        &self,
        process_id: chaos_ipc::ProcessId,
        owner_id: String,
        lease_token: String,
    ) -> Result<(), JournalClientError> {
        let response = self
            .send_request(JournalRequest::ReleaseLease(crate::ReleaseLeaseRequest {
                process_id,
                owner_id,
                lease_token,
            }))
            .await?;
        match response {
            JournalResponse::ReleaseLease(_) => Ok(()),
            other => Err(unexpected_variant("release_lease", &other)),
        }
    }

    pub async fn append_batch(
        &self,
        input: AppendBatchInput,
    ) -> Result<AppendBatchResult, JournalClientError> {
        let response = self
            .send_request(JournalRequest::AppendBatch(input))
            .await?;
        match response {
            JournalResponse::AppendBatch(result) => Ok(result),
            other => Err(unexpected_variant("append_batch", &other)),
        }
    }

    pub async fn load_journal(
        &self,
        process_id: chaos_ipc::ProcessId,
    ) -> Result<LoadedJournal, JournalClientError> {
        let response = self
            .send_request(JournalRequest::LoadJournal(LoadJournalRequest {
                process_id,
            }))
            .await?;
        match response {
            JournalResponse::LoadJournal(journal) => Ok(journal),
            other => Err(unexpected_variant("load_journal", &other)),
        }
    }

    async fn send_request(
        &self,
        request: JournalRequest,
    ) -> Result<JournalResponse, JournalClientError> {
        let envelope = RequestEnvelope {
            id: uuid::Uuid::now_v7().to_string(),
            request,
        };
        let response: ResponseEnvelope = self.execute_json(envelope).await?;
        if response.ok {
            response.result.ok_or(JournalClientError::MissingPayload)
        } else {
            Err(JournalClientError::Remote(
                response.error.ok_or(JournalClientError::MissingPayload)?,
            ))
        }
    }

    async fn execute_json<Req, Resp>(&self, value: Req) -> Result<Resp, JournalClientError>
    where
        Req: serde::Serialize,
        Resp: DeserializeOwned,
    {
        let request = Request::builder()
            .uri(format!("http://localhost{JOURNAL_RPC_PATH}"))
            .method(Method::POST)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&value)?))
            .map_err(|err| JournalClientError::Transport(err.to_string()))?;

        let EstablishedClientConnection { conn, input } = HttpConnectorLayer::<Body>::default()
            .into_layer(UnixConnector::fixed(&self.socket_path))
            .connect(request)
            .await
            .map_err(|err| JournalClientError::Transport(err.to_string()))?;

        let response = conn
            .serve(input)
            .await
            .map_err(|err| JournalClientError::Transport(err.to_string()))?;
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|err| JournalClientError::Transport(err.to_string()))?
            .to_bytes();
        if !status.is_success() {
            return Err(JournalClientError::HttpStatus {
                status: status.as_u16(),
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }

        serde_json::from_slice(&body).map_err(JournalClientError::from)
    }
}

fn unexpected_variant(expected: &'static str, actual: &JournalResponse) -> JournalClientError {
    JournalClientError::UnexpectedResponseVariant {
        expected,
        actual: response_variant_name(actual),
    }
}

fn response_variant_name(response: &JournalResponse) -> &'static str {
    match response {
        JournalResponse::Hello(_) => "hello",
        JournalResponse::CreateProcess(_) => "create_process",
        JournalResponse::GetProcess(_) => "get_process",
        JournalResponse::ListProcesses(_) => "list_processes",
        JournalResponse::AcquireLease(_) => "acquire_lease",
        JournalResponse::HeartbeatLease(_) => "heartbeat_lease",
        JournalResponse::ReleaseLease(_) => "release_lease",
        JournalResponse::AppendBatch(_) => "append_batch",
        JournalResponse::LoadJournal(_) => "load_journal",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chaos_ipc::ProcessId;
    use chaos_ipc::protocol::SessionSource;
    use rama::http::server::HttpServer;
    use rama::rt::Executor;
    use tempfile::tempdir;

    use super::JournalRpcClient;
    use crate::CreateProcessInput;
    use crate::JournalRpcServer;
    use crate::SqliteJournalStore;

    #[tokio::test]
    async fn client_round_trip_over_unix_socket() {
        let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let socket_path = temp_dir.path().join("journald.sock");
        let db_path = temp_dir.path().join("journal.sqlite");
        let store = Arc::new(
            SqliteJournalStore::open(&db_path)
                .await
                .unwrap_or_else(|err| panic!("open store: {err}")),
        );
        let service = JournalRpcServer::new(store, "sqlite").http_service();
        let socket_path_for_server = socket_path.clone();
        let server_task = tokio::spawn(async move {
            HttpServer::new_http1(Executor::default())
                .listen_unix(&socket_path_for_server, service)
                .await
                .unwrap_or_else(|err| panic!("listen_unix: {err}"));
        });

        for _ in 0..50 {
            if tokio::fs::metadata(&socket_path).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            tokio::fs::metadata(&socket_path).await.is_ok(),
            "socket did not appear"
        );

        let client = JournalRpcClient::new(socket_path.clone());
        let hello = client
            .hello("journal-client-test")
            .await
            .unwrap_or_else(|err| panic!("hello: {err}"));
        assert_eq!(hello.server_name, "chaos-journald");

        let process_id = ProcessId::new();
        let created = client
            .create_process(CreateProcessInput {
                process_id,
                parent: None,
                source: SessionSource::Cli,
                cwd: temp_dir.path().to_path_buf(),
                created_at: jiff::Timestamp::now(),
                title: Some("client round trip".to_string()),
                model_provider: Some("openai".to_string()),
                cli_version: Some("47.0.0".to_string()),
            })
            .await
            .unwrap_or_else(|err| panic!("create_process: {err}"));
        assert_eq!(created.process_id, process_id);
        assert_eq!(created.next_seq, 0);

        server_task.abort();
        let _ = server_task.await;
        let _ = tokio::fs::remove_file(&socket_path).await;
    }
}
