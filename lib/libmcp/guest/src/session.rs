use std::sync::Arc;
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
        response_tx: oneshot::Sender<()>,
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
    pub(crate) command_tx: mpsc::Sender<RuntimeCommand>,
    pub(crate) shared: Arc<SharedState>,
    pub(crate) next_id: Arc<AtomicU64>,
}

impl McpSession {
    pub fn server_info(&self) -> ServerInfo {
        self.shared.info.clone()
    }

    pub fn protocol_version(&self) -> &str {
        &self.shared.info.protocol_version
    }

    pub fn default_timeout(&self) -> Duration {
        self.shared.default_timeout
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
        let request_id = RequestId::number(self.next_id.fetch_add(1, Ordering::Relaxed) as i64);
        let (response_tx, response_rx) = oneshot::channel();

        self.command_tx
            .send(RuntimeCommand::Request {
                request_id: request_id.clone(),
                method: method.into(),
                params,
                response_tx,
            })
            .await
            .map_err(|_| GuestError::Disconnected)?;

        let timeout = timeout_override.unwrap_or(self.shared.default_timeout);
        match tokio::time::timeout(timeout, response_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(GuestError::Disconnected),
            Err(_) => {
                let _ = self
                    .command_tx
                    .send(RuntimeCommand::Cancel {
                        request_id,
                        reason: Some(format!("request timed out after {timeout:?}")),
                    })
                    .await;
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
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(RuntimeCommand::Notification {
                method: method.into(),
                params,
                response_tx,
            })
            .await
            .map_err(|_| GuestError::Disconnected)?;
        response_rx.await.map_err(|_| GuestError::Disconnected)?
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
        if let Some(cached) = self.shared.tools.read().await.clone() {
            return Ok(cached);
        }
        let tools = self
            .paginated_list("tools/list", |r: ListToolsResult| (r.tools, r.next_cursor))
            .await?;
        *self.shared.tools.write().await = Some(tools.clone());
        Ok(tools)
    }

    pub async fn tools(&self) -> Option<Vec<ToolInfo>> {
        self.shared.tools.read().await.clone()
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
        if let Some(cached) = self.shared.resources.read().await.clone() {
            return Ok(cached);
        }
        let resources = self
            .paginated_list("resources/list", |r: ListResourcesResult| {
                (r.resources, r.next_cursor)
            })
            .await?;
        *self.shared.resources.write().await = Some(resources.clone());
        Ok(resources)
    }

    pub async fn list_resource_templates(
        &self,
    ) -> Result<Vec<crate::protocol::ResourceTemplateInfo>, GuestError> {
        if let Some(cached) = self.shared.resource_templates.read().await.clone() {
            return Ok(cached);
        }
        let templates = self
            .paginated_list(
                "resources/templates/list",
                |r: ListResourceTemplatesResult| (r.resource_templates, r.next_cursor),
            )
            .await?;
        *self.shared.resource_templates.write().await = Some(templates.clone());
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
        if let Some(cached) = self.shared.prompts.read().await.clone() {
            return Ok(cached);
        }
        let prompts = self
            .paginated_list("prompts/list", |r: ListPromptsResult| {
                (r.prompts, r.next_cursor)
            })
            .await?;
        *self.shared.prompts.write().await = Some(prompts.clone());
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
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(RuntimeCommand::Shutdown { response_tx })
            .await
            .map_err(|_| GuestError::Disconnected)?;
        let _ = response_rx.await;
        Ok(())
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
