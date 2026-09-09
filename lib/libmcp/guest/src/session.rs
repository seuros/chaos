use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Map;
use serde_json::Value;
use tokio::sync::RwLock;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::error::GuestError;
use crate::protocol::CallToolRequestParams;
use crate::protocol::CallToolResponse;
use crate::protocol::CompleteRequest;
use crate::protocol::CompleteResult;
use crate::protocol::GetPromptRequestParams;
use crate::protocol::GetPromptResult;
use crate::protocol::GetTaskParams;
use crate::protocol::ListPromptsResult;
use crate::protocol::ListResourceTemplatesResult;
use crate::protocol::ListResourcesResult;
use crate::protocol::ListTasksResult;
use crate::protocol::ListToolsResult;
use crate::protocol::PaginatedRequestParams;
use crate::protocol::ReadResourceRequestParams;
use crate::protocol::ReadResourceResult;
use crate::protocol::RequestId;
use crate::protocol::ServerInfo;
use crate::protocol::SetLevelRequest;
use crate::protocol::StringMap;
use crate::protocol::SubscribeRequestParams;
use crate::protocol::Task;
use crate::protocol::ToolInfo;
use crate::transport::MessageTransport;

const COMMAND_QUEUE_TIMEOUT: Duration = Duration::from_secs(1);
const GRACEFUL_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const FORCE_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RUNTIME_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

const MAX_LIST_PAGES: usize = 1024;

pub(crate) enum RuntimeCommand {
    Request {
        request_id: RequestId,
        method: String,
        params: Option<Value>,
        response_tx: oneshot::Sender<Result<Value, GuestError>>,
    },
    Notification {
        method: String,
        params: Option<Value>,
        response_tx: oneshot::Sender<Result<(), GuestError>>,
    },
    Cancel {
        request_id: RequestId,
        reason: Option<String>,
    },
    Shutdown {
        response_tx: oneshot::Sender<Result<(), GuestError>>,
    },
}

pub(crate) struct CachedList<T> {
    generation: AtomicU64,
    items: RwLock<Option<Vec<T>>>,
}

impl<T: Clone> CachedList<T> {
    fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            items: RwLock::new(None),
        }
    }

    pub async fn invalidate(&self) {
        let mut items = self.items.write().await;
        self.generation.fetch_add(1, Ordering::AcqRel);
        *items = None;
    }

    async fn cached(&self) -> Option<Vec<T>> {
        self.items.read().await.clone()
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    async fn store_if_generation(&self, generation: u64, items: &[T]) {
        let mut guard = self.items.write().await;
        if self.generation.load(Ordering::Acquire) == generation {
            *guard = Some(items.to_vec());
        }
    }
}

pub(crate) struct SharedState {
    pub info: ServerInfo,
    pub default_timeout: Duration,
    pub tools: CachedList<ToolInfo>,
    pub resources: CachedList<crate::protocol::ResourceInfo>,
    pub resource_templates: CachedList<crate::protocol::ResourceTemplateInfo>,
    pub prompts: CachedList<crate::protocol::PromptInfo>,
}

impl SharedState {
    pub fn new(info: ServerInfo, default_timeout: Duration) -> Self {
        Self {
            info,
            default_timeout,
            tools: CachedList::new(),
            resources: CachedList::new(),
            resource_templates: CachedList::new(),
            prompts: CachedList::new(),
        }
    }
}

#[derive(Clone)]
pub struct McpSession {
    inner: Arc<McpSessionInner>,
}

struct McpSessionInner {
    command_tx: mpsc::Sender<RuntimeCommand>,
    shared: Arc<SharedState>,
    next_id: AtomicU64,
    lifecycle: RuntimeLifecycle,
}

struct RuntimeLifecycle {
    transport: Arc<dyn MessageTransport>,
    runtime_task: StdMutex<Option<JoinHandle<()>>>,
    shutdown_lock: tokio::sync::Mutex<()>,
    closed: AtomicBool,
}

#[derive(Clone)]
pub struct WeakMcpSession {
    inner: Weak<McpSessionInner>,
}

impl WeakMcpSession {
    pub fn upgrade(&self) -> Option<McpSession> {
        self.inner.upgrade().map(|inner| McpSession { inner })
    }
}

impl McpSession {
    pub(crate) fn new(
        command_tx: mpsc::Sender<RuntimeCommand>,
        shared: Arc<SharedState>,
        transport: Arc<dyn MessageTransport>,
        runtime_task: JoinHandle<()>,
    ) -> Self {
        Self {
            inner: Arc::new(McpSessionInner {
                command_tx,
                shared,
                next_id: AtomicU64::new(2),
                lifecycle: RuntimeLifecycle {
                    transport,
                    runtime_task: StdMutex::new(Some(runtime_task)),
                    shutdown_lock: tokio::sync::Mutex::new(()),
                    closed: AtomicBool::new(false),
                },
            }),
        }
    }

    pub fn downgrade(&self) -> WeakMcpSession {
        WeakMcpSession {
            inner: Arc::downgrade(&self.inner),
        }
    }

    pub fn server_info(&self) -> ServerInfo {
        self.inner.shared.info.clone()
    }

    pub fn protocol_version(&self) -> &str {
        &self.inner.shared.info.protocol_version
    }

    pub fn default_timeout(&self) -> Duration {
        self.inner.shared.default_timeout
    }

    pub async fn request_value(
        &self,
        method: impl Into<String>,
        params: Option<Value>,
    ) -> Result<Value, GuestError> {
        self.request_value_with_timeout(method, params, None).await
    }

    pub async fn request_value_with_timeout(
        &self,
        method: impl Into<String>,
        params: Option<Value>,
        timeout_override: Option<Duration>,
    ) -> Result<Value, GuestError> {
        if self.inner.lifecycle.closed.load(Ordering::Acquire) {
            return Err(GuestError::Disconnected);
        }
        let timeout = timeout_override.unwrap_or(self.inner.shared.default_timeout);
        let deadline = tokio::time::Instant::now() + timeout;
        let request_id =
            RequestId::number(self.inner.next_id.fetch_add(1, Ordering::Relaxed) as i64);
        let (response_tx, response_rx) = oneshot::channel();

        tokio::time::timeout_at(
            deadline,
            self.inner.command_tx.send(RuntimeCommand::Request {
                request_id: request_id.clone(),
                method: method.into(),
                params,
                response_tx,
            }),
        )
        .await
        .map_err(|_| GuestError::Timeout(timeout))?
        .map_err(|_| GuestError::Disconnected)?;

        match tokio::time::timeout_at(deadline, response_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(GuestError::Disconnected),
            Err(_) => {
                let _ = self.inner.command_tx.try_send(RuntimeCommand::Cancel {
                    request_id,
                    reason: Some(format!("request timed out after {timeout:?}")),
                });
                Err(GuestError::Timeout(timeout))
            }
        }
    }

    pub async fn request<TParams, TResult>(
        &self,
        method: impl Into<String>,
        params: &TParams,
    ) -> Result<TResult, GuestError>
    where
        TParams: Serialize + ?Sized,
        TResult: DeserializeOwned,
    {
        self.request_with_timeout(method, params, None).await
    }

    pub async fn request_with_timeout<TParams, TResult>(
        &self,
        method: impl Into<String>,
        params: &TParams,
        timeout_override: Option<Duration>,
    ) -> Result<TResult, GuestError>
    where
        TParams: Serialize + ?Sized,
        TResult: DeserializeOwned,
    {
        let value = self
            .request_value_with_timeout(
                method,
                Some(serde_json::to_value(params)?),
                timeout_override,
            )
            .await?;
        serde_json::from_value(value).map_err(GuestError::from)
    }

    pub async fn notify_value(
        &self,
        method: impl Into<String>,
        params: Option<Value>,
    ) -> Result<(), GuestError> {
        if self.inner.lifecycle.closed.load(Ordering::Acquire) {
            return Err(GuestError::Disconnected);
        }
        let timeout = self.inner.shared.default_timeout;
        let deadline = tokio::time::Instant::now() + timeout;
        let (response_tx, response_rx) = oneshot::channel();
        tokio::time::timeout_at(
            deadline,
            self.inner.command_tx.send(RuntimeCommand::Notification {
                method: method.into(),
                params,
                response_tx,
            }),
        )
        .await
        .map_err(|_| GuestError::Timeout(timeout))?
        .map_err(|_| GuestError::Disconnected)?;
        tokio::time::timeout_at(deadline, response_rx)
            .await
            .map_err(|_| GuestError::Timeout(timeout))?
            .map_err(|_| GuestError::Disconnected)?
    }

    pub async fn notify<TParams>(
        &self,
        method: impl Into<String>,
        params: Option<&TParams>,
    ) -> Result<(), GuestError>
    where
        TParams: Serialize + ?Sized,
    {
        let params = match params {
            Some(params) => Some(serde_json::to_value(params)?),
            None => None,
        };
        self.notify_value(method, params).await
    }

    pub async fn ping(&self) -> Result<(), GuestError> {
        self.request_value("ping", Some(serde_json::json!({})))
            .await?;
        Ok(())
    }

    /// Drive a cursor-based MCP list endpoint to completion, collecting all
    /// pages into a single `Vec<Item>`.
    ///
    /// `extract` receives each page response and returns `(items, next_cursor)`.
    async fn paginated_list<Resp, Item, F>(
        &self,
        method: &'static str,
        extract: F,
    ) -> Result<Vec<Item>, GuestError>
    where
        Resp: DeserializeOwned,
        F: Fn(Resp) -> (Vec<Item>, Option<String>),
    {
        let mut cursor: Option<String> = None;
        let mut items = Vec::new();
        for _ in 0..MAX_LIST_PAGES {
            let resp: Resp = self
                .request(
                    method,
                    &PaginatedRequestParams {
                        cursor: cursor.clone(),
                    },
                )
                .await?;
            let (page, next) = extract(resp);
            items.extend(page);
            if next.is_none() {
                return Ok(items);
            }
            if next == cursor {
                return Err(GuestError::Protocol(format!(
                    "{method} returned the same cursor twice"
                )));
            }
            cursor = next;
        }
        Err(GuestError::Protocol(format!(
            "{method} did not complete within {MAX_LIST_PAGES} pages"
        )))
    }

    async fn cached_paginated_list<Resp, Item, F>(
        &self,
        cache: &CachedList<Item>,
        method: &'static str,
        extract: F,
    ) -> Result<Vec<Item>, GuestError>
    where
        Item: Clone,
        Resp: DeserializeOwned,
        F: Fn(Resp) -> (Vec<Item>, Option<String>),
    {
        if let Some(cached) = cache.cached().await {
            return Ok(cached);
        }
        let generation = cache.generation();
        let items = self.paginated_list(method, extract).await?;
        cache.store_if_generation(generation, &items).await;
        Ok(items)
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>, GuestError> {
        self.cached_paginated_list(
            &self.inner.shared.tools,
            "tools/list",
            |r: ListToolsResult| (r.tools, r.next_cursor),
        )
        .await
    }

    pub async fn tools(&self) -> Option<Vec<ToolInfo>> {
        self.inner.shared.tools.cached().await
    }

    pub async fn call_tool(
        &self,
        name: impl Into<String>,
        arguments: Option<Map<String, Value>>,
    ) -> Result<CallToolResponse, GuestError> {
        self.call_tool_with(CallToolRequestParams {
            name: name.into(),
            arguments,
            meta: None,
            task: None,
        })
        .await
    }

    pub async fn call_tool_with(
        &self,
        params: CallToolRequestParams,
    ) -> Result<CallToolResponse, GuestError> {
        self.request("tools/call", &params).await
    }

    pub async fn list_resources(&self) -> Result<Vec<crate::protocol::ResourceInfo>, GuestError> {
        self.cached_paginated_list(
            &self.inner.shared.resources,
            "resources/list",
            |r: ListResourcesResult| (r.resources, r.next_cursor),
        )
        .await
    }

    pub async fn list_resource_templates(
        &self,
    ) -> Result<Vec<crate::protocol::ResourceTemplateInfo>, GuestError> {
        self.cached_paginated_list(
            &self.inner.shared.resource_templates,
            "resources/templates/list",
            |r: ListResourceTemplatesResult| (r.resource_templates, r.next_cursor),
        )
        .await
    }

    pub async fn read_resource(
        &self,
        uri: impl Into<String>,
    ) -> Result<ReadResourceResult, GuestError> {
        self.request(
            "resources/read",
            &ReadResourceRequestParams {
                uri: uri.into(),
                meta: None,
            },
        )
        .await
    }

    pub async fn subscribe_resource(&self, uri: impl Into<String>) -> Result<(), GuestError> {
        self.subscribe_request("resources/subscribe", uri).await
    }

    pub async fn unsubscribe_resource(&self, uri: impl Into<String>) -> Result<(), GuestError> {
        self.subscribe_request("resources/unsubscribe", uri).await
    }

    async fn subscribe_request(
        &self,
        method: &'static str,
        uri: impl Into<String>,
    ) -> Result<(), GuestError> {
        let _: Value = self
            .request(
                method,
                &SubscribeRequestParams {
                    uri: uri.into(),
                    meta: None,
                },
            )
            .await?;
        Ok(())
    }

    pub async fn list_prompts(&self) -> Result<Vec<crate::protocol::PromptInfo>, GuestError> {
        self.cached_paginated_list(
            &self.inner.shared.prompts,
            "prompts/list",
            |r: ListPromptsResult| (r.prompts, r.next_cursor),
        )
        .await
    }

    pub async fn get_prompt(
        &self,
        name: impl Into<String>,
        arguments: Option<Map<String, Value>>,
    ) -> Result<GetPromptResult, GuestError> {
        self.request(
            "prompts/get",
            &GetPromptRequestParams {
                name: name.into(),
                arguments: coerce_string_arguments(arguments)?,
                meta: None,
            },
        )
        .await
    }

    pub async fn complete(&self, request: &CompleteRequest) -> Result<CompleteResult, GuestError> {
        self.request("completion/complete", request).await
    }

    pub async fn set_logging_level(&self, level: impl Into<String>) -> Result<(), GuestError> {
        let _: Value = self
            .request(
                "logging/setLevel",
                &SetLevelRequest {
                    level: level.into(),
                },
            )
            .await?;
        Ok(())
    }

    pub async fn list_tasks(&self) -> Result<ListTasksResult, GuestError> {
        let tasks = self
            .paginated_list("tasks/list", |r: ListTasksResult| (r.tasks, r.next_cursor))
            .await?;
        Ok(ListTasksResult {
            tasks,
            next_cursor: None,
            meta: None,
        })
    }

    pub async fn get_task(&self, task_id: impl Into<String>) -> Result<Task, GuestError> {
        self.request(
            "tasks/get",
            &GetTaskParams {
                task_id: task_id.into(),
            },
        )
        .await
    }

    pub async fn task_result(&self, task_id: impl Into<String>) -> Result<Value, GuestError> {
        self.request_value(
            "tasks/result",
            Some(serde_json::to_value(GetTaskParams {
                task_id: task_id.into(),
            })?),
        )
        .await
    }

    pub async fn cancel_task(&self, task_id: impl Into<String>) -> Result<Task, GuestError> {
        self.request(
            "tasks/cancel",
            &crate::protocol::CancelTaskParams {
                task_id: task_id.into(),
            },
        )
        .await
    }

    pub async fn disconnect(&self) -> Result<(), GuestError> {
        let _shutdown_guard = self.inner.lifecycle.shutdown_lock.lock().await;
        if self
            .inner
            .lifecycle
            .runtime_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            return Ok(());
        }
        self.inner.lifecycle.closed.store(true, Ordering::Release);

        let (response_tx, response_rx) = oneshot::channel();
        let graceful = tokio::time::timeout(GRACEFUL_DISCONNECT_TIMEOUT, async {
            tokio::time::timeout(
                COMMAND_QUEUE_TIMEOUT,
                self.inner
                    .command_tx
                    .send(RuntimeCommand::Shutdown { response_tx }),
            )
            .await
            .map_err(|_| GuestError::Timeout(COMMAND_QUEUE_TIMEOUT))?
            .map_err(|_| GuestError::Disconnected)?;
            response_rx.await.map_err(|_| GuestError::Disconnected)?
        })
        .await;

        let graceful_succeeded = matches!(graceful, Ok(Ok(())));
        let mut shutdown_error = None;
        if !graceful_succeeded {
            match &graceful {
                Ok(Err(error)) => tracing::warn!(
                    %error,
                    "graceful MCP session disconnect failed; forcing transport shutdown"
                ),
                Err(_) => tracing::warn!(
                    timeout = ?GRACEFUL_DISCONNECT_TIMEOUT,
                    "graceful MCP session disconnect timed out; forcing transport shutdown"
                ),
                Ok(Ok(())) => {}
            }
            shutdown_error = match tokio::time::timeout(
                FORCE_DISCONNECT_TIMEOUT,
                self.inner.lifecycle.transport.force_shutdown(),
            )
            .await
            {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(_) => Some(GuestError::Timeout(FORCE_DISCONNECT_TIMEOUT)),
            };
        }

        let runtime_task = self
            .inner
            .lifecycle
            .runtime_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(mut runtime_task) = runtime_task {
            match tokio::time::timeout(RUNTIME_JOIN_TIMEOUT, &mut runtime_task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    shutdown_error = Some(GuestError::Protocol(format!(
                        "MCP runtime task failed during shutdown: {error}"
                    )));
                }
                Err(_) => {
                    tracing::error!(
                        timeout = ?RUNTIME_JOIN_TIMEOUT,
                        "MCP runtime task did not exit after transport shutdown; aborting it"
                    );
                    runtime_task.abort();
                    let _ = runtime_task.await;
                }
            }
        }

        match shutdown_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn coerce_string_arguments(
    arguments: Option<Map<String, Value>>,
) -> Result<Option<StringMap>, GuestError> {
    arguments
        .map(|arguments| {
            arguments
                .into_iter()
                .map(|(key, value)| match value {
                    Value::String(value) => Ok((key, value)),
                    other => Err(GuestError::InvalidParams(format!(
                        "prompt argument `{key}` must be a string, got {}",
                        value_kind(&other)
                    ))),
                })
                .collect()
        })
        .transpose()
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::NoopClientHandler;
    use crate::protocol::ClientCapabilities;
    use crate::protocol::Implementation;
    use crate::protocol::JsonRpcMessage;
    use crate::protocol::JsonRpcResponse;
    use crate::runtime::ConnectionOptions;
    use crate::runtime::connect_with_transport;
    use crate::transport::TransportFuture;
    use tokio::sync::Mutex;

    struct CursorLoopTransport {
        incoming_tx: mpsc::UnboundedSender<JsonRpcMessage>,
        incoming_rx: Mutex<mpsc::UnboundedReceiver<JsonRpcMessage>>,
    }

    impl CursorLoopTransport {
        fn new() -> Arc<Self> {
            let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
            Arc::new(Self {
                incoming_tx,
                incoming_rx: Mutex::new(incoming_rx),
            })
        }
    }

    impl MessageTransport for CursorLoopTransport {
        fn send<'a>(&'a self, message: JsonRpcMessage) -> TransportFuture<'a, ()> {
            Box::pin(async move {
                if let JsonRpcMessage::Request(request) = &message {
                    let id = request.id.clone().unwrap();
                    let result = match request.method.as_str() {
                        "initialize" => serde_json::json!({
                            "protocolVersion": "2025-11-25",
                            "capabilities": {},
                            "serverInfo": {"name": "loop-server", "version": "0.0.1"}
                        }),
                        "tools/list" => serde_json::json!({
                            "tools": [],
                            "nextCursor": "same-cursor"
                        }),
                        other => serde_json::json!({"unexpected": other}),
                    };
                    let _ = self
                        .incoming_tx
                        .send(JsonRpcMessage::Response(JsonRpcResponse::success(
                            id, result,
                        )));
                }
                Ok(())
            })
        }

        fn recv<'a>(&'a self) -> TransportFuture<'a, JsonRpcMessage> {
            Box::pin(async move {
                match self.incoming_rx.lock().await.recv().await {
                    Some(message) => Ok(message),
                    None => std::future::pending().await,
                }
            })
        }

        fn shutdown<'a>(&'a self) -> TransportFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn list_tools_rejects_a_server_that_repeats_cursors() {
        let session = connect_with_transport(
            CursorLoopTransport::new(),
            ConnectionOptions {
                client_info: Implementation::new("test-client", "1.0.0"),
                capabilities: ClientCapabilities::default(),
                handler: Arc::new(NoopClientHandler),
                default_timeout: Duration::from_secs(5),
            },
        )
        .await
        .unwrap();

        let error = session.list_tools().await.unwrap_err();
        assert!(matches!(error, GuestError::Protocol(_)), "got {error:?}");
        assert!(session.tools().await.is_none());

        session.disconnect().await.unwrap();
    }
}
