#![doc = include_str!("../README.md")]

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, InitializeRequest, InitializeResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SessionNotification,
};
use agent_client_protocol::{
    Agent, Client, ConnectionTo, JsonRpcNotification, JsonRpcRequest, Lines,
};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

/// Protocol types and traits from the official SDK, without native transports.
pub use agent_client_protocol as protocol;

mod permissions;

/// Application-owned permission decisions. This is not an agent sandbox:
/// callers must independently contain agents and their native tool execution.
pub trait PermissionHandler: std::fmt::Debug + Send + Sync {
    /// Handle one permission request. No automatic approval or retry is added.
    /// The future is dropped when its session is cancelled or disconnected.
    /// It must be cancellation-safe and must not perform the proposed action.
    fn request_permission(
        &self,
        request: RequestPermissionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<RequestPermissionResponse>>;
}

/// Default policy: reject every requested native or MCP permission.
#[derive(Debug)]
pub struct DenyPermissions;

impl PermissionHandler for DenyPermissions {
    fn request_permission(
        &self,
        _request: RequestPermissionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<RequestPermissionResponse>> {
        Box::pin(async {
            Ok(RequestPermissionResponse::new(
                RequestPermissionOutcome::Cancelled,
            ))
        })
    }
}

/// Connection limits and policy. No filesystem or terminal capability is advertised.
#[derive(Clone, Debug)]
pub struct ConnectionOptions {
    /// Deadline for an individual request, including initialization.
    pub request_timeout: Duration,
    /// Deadline for joining the protocol driver during shutdown.
    pub shutdown_timeout: Duration,
    /// Maximum encoded JSON line size in either direction.
    pub max_message_bytes: usize,
    /// Maximum unread session updates; overflow closes the connection.
    pub notification_capacity: usize,
    /// Explicit application policy; defaults to denying all requests.
    pub permissions: Arc<dyn PermissionHandler>,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(5),
            max_message_bytes: 1024 * 1024,
            notification_capacity: 256,
            permissions: Arc::new(DenyPermissions),
        }
    }
}

/// A failure never grants permission to replay a prompt or proves containment.
#[derive(Debug, thiserror::Error)]
pub enum GuestError {
    /// The peer rejected a request, or the protocol/transport failed.
    #[error("ACP error: {0}")]
    Protocol(#[from] agent_client_protocol::Error),
    /// The request timed out; its remote execution outcome may be unknown.
    #[error("ACP request timed out; execution outcome may be unknown")]
    Timeout,
    /// A closed or uncertain connection cannot accept another request.
    #[error("ACP connection is closed")]
    Disconnected,
    /// Invalid local options; no connection was started.
    #[error("ACP connection limits must be nonzero")]
    InvalidOptions,
    /// The peer did not negotiate the supported stable protocol.
    #[error("ACP peer did not negotiate protocol version 1")]
    VersionMismatch,
    /// The driver could not be joined normally.
    #[error("ACP driver failed: {0}")]
    Driver(#[from] tokio::task::JoinError),
}

#[derive(Clone, Debug)]
struct ConnectionState {
    closed: Arc<AtomicBool>,
    shutdown: watch::Sender<bool>,
}

impl ConnectionState {
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.shutdown.send_replace(true);
    }

    fn ensure_open(&self) -> Result<(), GuestError> {
        if self.closed.load(Ordering::Acquire) {
            Err(GuestError::Disconnected)
        } else {
            Ok(())
        }
    }
}

/// Closing on drop is important: dropping a request future is an uncertain
/// submission, not evidence that the remote agent never received it.
struct RequestGuard(Option<ConnectionState>);

impl Drop for RequestGuard {
    fn drop(&mut self) {
        if let Some(state) = self.0.take() {
            state.close();
        }
    }
}

/// Own the driver before the first constructor await, so cancellation of
/// connect() cannot detach a live protocol task holding the agent's pipes.
struct DriverGuard {
    task: JoinHandle<agent_client_protocol::Result<()>>,
    state: ConnectionState,
}

impl Drop for DriverGuard {
    fn drop(&mut self) {
        self.state.close();
        self.task.abort();
    }
}

/// Cloneable request handle. The owner must remain alive and drain updates.
///
/// Callers serialize prompts per session; a cloned handle can send cancellation
/// while a prompt response is pending. Cancellation is only protocol delivery,
/// not proof that remote tools or subprocesses stopped.
#[derive(Clone, Debug)]
pub struct AcpClient {
    connection: ConnectionTo<Agent>,
    state: ConnectionState,
    request_timeout: Duration,
    permissions: permissions::PendingPermissions,
}

impl AcpClient {
    /// Send a typed request once. Timeout leaves the connection open; dropping
    /// the future closes it. Requests are never retried.
    pub async fn request<R: JsonRpcRequest>(&self, request: R) -> Result<R::Response, GuestError> {
        self.state.ensure_open()?;
        let mut guard = RequestGuard(Some(self.state.clone()));
        let result = tokio::time::timeout(
            self.request_timeout,
            self.connection.send_request(request).block_task(),
        )
        .await;
        guard.0 = None;
        result
            .map_err(|_| GuestError::Timeout)?
            .map_err(GuestError::Protocol)
    }

    /// Queue a typed notification. Success does not acknowledge peer processing.
    pub fn notify<N: JsonRpcNotification>(&self, notification: N) -> Result<(), GuestError> {
        self.state.ensure_open()?;
        let message = notification.to_untyped_message()?;
        if message.method == "session/cancel" {
            let cancel: CancelNotification = serde_json::from_value(message.params)
                .map_err(|_| protocol::Error::invalid_params())?;
            return self.permissions.cancel_and_send(&cancel.session_id, || {
                self.connection
                    .send_notification(notification)
                    .map_err(GuestError::Protocol)
            });
        }
        self.connection
            .send_notification(notification)
            .map_err(GuestError::Protocol)
    }

    /// Whether the driver has closed.
    pub fn is_closed(&self) -> bool {
        self.state.closed.load(Ordering::Acquire)
    }
}

/// Owns one Tokio-driven ACP connection, not the agent's subprocess.
///
/// Permission requests are denied by default. Filesystem and terminal requests
/// are unsupported. An embedding application retains responsibility for launching,
/// authenticating, sandboxing, killing and reaping its agent.
pub struct AcpConnection {
    client: AcpClient,
    initialized: InitializeResponse,
    driver: DriverGuard,
    shutdown_timeout: Duration,
}

impl AcpConnection {
    /// Connect to existing Tokio byte streams and negotiate ACP v1.
    ///
    /// The returned update receiver must be drained concurrently with requests,
    /// including session loading, which may replay history. Full/closed update
    /// queues fail closed instead of silently dropping observations.
    pub async fn connect<R, W>(
        reader: R,
        writer: W,
        options: ConnectionOptions,
    ) -> Result<(Self, mpsc::Receiver<SessionNotification>), GuestError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        if options.request_timeout.is_zero()
            || options.shutdown_timeout.is_zero()
            || options.max_message_bytes == 0
            || options.notification_capacity == 0
        {
            return Err(GuestError::InvalidOptions);
        }
        let limit = options.max_message_bytes;
        let incoming =
            FramedRead::new(reader, LinesCodec::new_with_max_length(limit)).map(|result| {
                let line = result.map_err(io::Error::other)?;
                // Validate syntax without copying the JSON tree. The upstream SDK
                // still owns all envelope/schema validation and protocol dispatch.
                serde_json::from_str::<&serde_json::value::RawValue>(&line).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid ACP JSON frame")
                })?;
                Ok(line)
            });
        let outgoing = <FramedWrite<W, LinesCodec> as SinkExt<String>>::sink_map_err(
            FramedWrite::new(writer, LinesCodec::new_with_max_length(limit)),
            io::Error::other,
        )
        .with(move |line: String| async move {
            if line.len() > limit {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "ACP outgoing message exceeds configured limit",
                ))
            } else {
                Ok(line)
            }
        });
        let (updates, receiver) = mpsc::channel(options.notification_capacity);
        let (shutdown, mut stopping) = watch::channel(false);
        let state = ConnectionState {
            closed: Arc::new(AtomicBool::new(false)),
            shutdown,
        };
        let notification_state = state.clone();
        let driver_state = state.clone();
        let permissions = Arc::clone(&options.permissions);
        let pending_permissions = permissions::PendingPermissions::default();
        let incoming_permissions = pending_permissions.clone();
        let (ready, pending) = oneshot::channel();
        let task = tokio::spawn(async move {
            let result = Client
                .builder()
                .name("acp-guest")
                .on_receive_notification(
                    async move |notification: SessionNotification, _cx| {
                        if updates.try_send(notification).is_err() {
                            notification_state.close();
                            return Err(agent_client_protocol::Error::internal_error()
                                .data("ACP update receiver is full or closed"));
                        }
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .on_receive_request(
                    async move |request: RequestPermissionRequest, responder, cx| {
                        let pending = incoming_permissions.register(request.session_id.clone());
                        let permissions = permissions.clone();
                        cx.spawn(async move {
                            let result = tokio::select! {
                                biased;
                                _ = pending.cancelled() => Ok(RequestPermissionResponse::new(
                                    RequestPermissionOutcome::Cancelled,
                                )),
                                result = permissions.request_permission(request) => result,
                            };
                            pending.respond(responder, result)
                        })
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(
                    Lines::new(outgoing, incoming),
                    async move |connection: ConnectionTo<Agent>| {
                        let incoming = connection.clone();
                        if ready.send(connection).is_err() {
                            return Ok(());
                        }
                        tokio::select! {
                            _ = stopping.wait_for(|stop| *stop) => Ok(()),
                            _ = incoming.incoming_closed() => {
                                Err(agent_client_protocol::Error::internal_error()
                                    .data("ACP peer disconnected"))
                            },
                        }
                    },
                )
                .await;
            driver_state.close();
            result
        });
        let mut driver = DriverGuard {
            task,
            state: state.clone(),
        };
        let connection = match tokio::time::timeout(options.request_timeout, pending).await {
            Ok(Ok(connection)) => connection,
            _ => {
                state.close();
                driver.task.abort();
                let _ = (&mut driver.task).await;
                return Err(GuestError::Disconnected);
            }
        };
        let client = AcpClient {
            connection,
            state,
            request_timeout: options.request_timeout,
            permissions: pending_permissions,
        };
        let initialized = match client
            .request(InitializeRequest::new(ProtocolVersion::V1))
            .await
        {
            Ok(response) if response.protocol_version == ProtocolVersion::V1 => response,
            result => {
                client.state.close();
                driver.task.abort();
                let _ = (&mut driver.task).await;
                return Err(match result {
                    Ok(_) => GuestError::VersionMismatch,
                    Err(error) => error,
                });
            }
        };
        Ok((
            Self {
                client,
                initialized,
                driver,
                shutdown_timeout: options.shutdown_timeout,
            },
            receiver,
        ))
    }

    /// Negotiated agent capabilities and available authentication methods.
    pub fn initialized(&self) -> &InitializeResponse {
        &self.initialized
    }

    /// Obtain a handle for typed requests and cancellation notifications.
    pub fn client(&self) -> AcpClient {
        self.client.clone()
    }

    /// Close the protocol and join its driver within the shutdown deadline.
    ///
    /// This closes pipes only. It does not attest subprocess/tool containment.
    pub async fn disconnect(mut self) -> Result<(), GuestError> {
        self.client.state.close();
        match tokio::time::timeout(self.shutdown_timeout, &mut self.driver.task).await {
            Ok(result) => result?.map_err(GuestError::Protocol),
            Err(_) => {
                self.driver.task.abort();
                let _ = (&mut self.driver.task).await;
                Err(GuestError::Timeout)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;
