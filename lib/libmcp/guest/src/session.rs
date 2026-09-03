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

pub(crate) struct SharedState {
    pub info: ServerInfo,
    pub default_timeout: Duration,
    pub tools: RwLock<Option<Vec<ToolInfo>>>,
    pub resources: RwLock<Option<Vec<crate::protocol::ResourceInfo>>>,
    pub resource_templates: RwLock<Option<Vec<crate::protocol::ResourceTemplateInfo>>>,
    pub prompts: RwLock<Option<Vec<crate::protocol::PromptInfo>>>,
}

impl SharedState {
    pub fn new(info: ServerInfo, default_timeout: Duration) -> Self {
        Self {
            info,
            default_timeout,
            tools: RwLock::new(None),
            resources: RwLock::new(None),
            resource_templates: RwLock::new(None),
            prompts: RwLock::new(None),
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

    async fn execute_with_timeout<T, F>(
        &self,
        request_id: RequestId,
        timeout: Duration,
        fut: F,
    ) -> Result<T, GuestError>
    where
        F: std::future::Future<Output = Result<T, GuestError>>,
    {
        match tokio::time::timeout(timeout, fut).await {
            Ok(result) => result,
            Err(_) => {
                let _ = self.inner.command_tx.try_send(RuntimeCommand::Cancel {
                    request_id,
                    reason: Some(format!("request timed out after {timeout:?}")),
                });
                Err(GuestError::Timeout(timeout))
            }
        }
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
        let request_id =
            RequestId::number(self.inner.next_id.fetch_add(1, Ordering::Relaxed) as i64);
        let (response_tx, response_rx) = oneshot::channel();

        tokio::time::timeout(
            timeout,
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

        self.execute_with_timeout(request_id, timeout, async {
            match response_rx.await {
                Ok(result) => result,
                Err(_) => Err(GuestError::Disconnected),
            }
        })
        .await
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
        let (response_tx, response_rx) = oneshot::channel();
        tokio::time::timeout(
            timeout,
            self.inner.command_tx.send(RuntimeCommand::Notification {
                method: method.into(),
                params,
                response_tx,
            }),
        )
        .await
        .map_err(|_| GuestError::Timeout(timeout))?
        .map_err(|_| GuestError::Disconnected)?;
        tokio::time::timeout(timeout, response_rx)
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
        loop {
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
            cursor = next;
            if cursor.is_none() {
                break;
            }
        }
        Ok(items)
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>, GuestError> {
        if let Some(cached) = self.inner.shared.tools.read().await.clone() {
            return Ok(cached);
        }
        let tools = self
            .paginated_list("tools/list", |r: ListToolsResult| (r.tools, r.next_cursor))
            .await?;
        *self.inner.shared.tools.write().await = Some(tools.clone());
        Ok(tools)
    }

    pub async fn tools(&self) -> Option<Vec<ToolInfo>> {
        self.inner.shared.tools.read().await.clone()
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
        if let Some(cached) = self.inner.shared.resources.read().await.clone() {
            return Ok(cached);
        }
        let resources = self
            .paginated_list("resources/list", |r: ListResourcesResult| {
                (r.resources, r.next_cursor)
            })
            .await?;
        *self.inner.shared.resources.write().await = Some(resources.clone());
        Ok(resources)
    }

    pub async fn list_resource_templates(
        &self,
    ) -> Result<Vec<crate::protocol::ResourceTemplateInfo>, GuestError> {
        if let Some(cached) = self.inner.shared.resource_templates.read().await.clone() {
            return Ok(cached);
        }
        let templates = self
            .paginated_list(
                "resources/templates/list",
                |r: ListResourceTemplatesResult| (r.resource_templates, r.next_cursor),
            )
            .await?;
        *self.inner.shared.resource_templates.write().await = Some(templates.clone());
        Ok(templates)
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
        if let Some(cached) = self.inner.shared.prompts.read().await.clone() {
            return Ok(cached);
        }
        let prompts = self
            .paginated_list("prompts/list", |r: ListPromptsResult| {
                (r.prompts, r.next_cursor)
            })
            .await?;
        *self.inner.shared.prompts.write().await = Some(prompts.clone());
        Ok(prompts)
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
