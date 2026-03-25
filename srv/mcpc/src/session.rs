use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio::sync::{RwLock, mpsc, oneshot};

use crate::error::GuestError;
use crate::protocol::{
    CallToolRequestParams, CallToolResponse, CompleteRequest, CompleteResult,
    GetPromptRequestParams, GetPromptResult, GetTaskParams, ListPromptsResult,
    ListResourceTemplatesResult, ListResourcesResult, ListTasksResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, RequestId, ServerInfo,
    SetLevelRequest, StringMap, SubscribeRequestParams, Task, ToolInfo,
};

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
                        reason: Some(format!("request timed out after {:?}", timeout)),
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

    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>, GuestError> {
        if let Some(cached) = self.shared.tools.read().await.clone() {
            return Ok(cached);
        }

        let mut cursor = None;
        let mut tools = Vec::new();
        loop {
            let result: ListToolsResult = self
                .request(
                    "tools/list",
                    &PaginatedRequestParams {
                        cursor: cursor.clone(),
                    },
                )
                .await?;
            tools.extend(result.tools);
            cursor = result.next_cursor;
            if cursor.is_none() {
                break;
            }
        }

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

        let mut cursor = None;
        let mut resources = Vec::new();
        loop {
            let result: ListResourcesResult = self
                .request(
                    "resources/list",
                    &PaginatedRequestParams {
                        cursor: cursor.clone(),
                    },
                )
                .await?;
            resources.extend(result.resources);
            cursor = result.next_cursor;
            if cursor.is_none() {
                break;
            }
        }

        *self.shared.resources.write().await = Some(resources.clone());
        Ok(resources)
    }

    pub async fn list_resource_templates(
        &self,
    ) -> Result<Vec<crate::protocol::ResourceTemplateInfo>, GuestError> {
        if let Some(cached) = self.shared.resource_templates.read().await.clone() {
            return Ok(cached);
        }

        let mut cursor = None;
        let mut templates = Vec::new();
        loop {
            let result: ListResourceTemplatesResult = self
                .request(
                    "resources/templates/list",
                    &PaginatedRequestParams {
                        cursor: cursor.clone(),
                    },
                )
                .await?;
            templates.extend(result.resource_templates);
            cursor = result.next_cursor;
            if cursor.is_none() {
                break;
            }
        }

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
        let _: Value = self
            .request(
                "resources/subscribe",
                &SubscribeRequestParams {
                    uri: uri.into(),
                    meta: None,
                },
            )
            .await?;
        Ok(())
    }

    pub async fn unsubscribe_resource(&self, uri: impl Into<String>) -> Result<(), GuestError> {
        let _: Value = self
            .request(
                "resources/unsubscribe",
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

        let mut cursor = None;
        let mut prompts = Vec::new();
        loop {
            let result: ListPromptsResult = self
                .request(
                    "prompts/list",
                    &PaginatedRequestParams {
                        cursor: cursor.clone(),
                    },
                )
                .await?;
            prompts.extend(result.prompts);
            cursor = result.next_cursor;
            if cursor.is_none() {
                break;
            }
        }

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
        let mut cursor = None;
        let mut tasks = Vec::new();

        loop {
            let result: ListTasksResult = self
                .request(
                    "tasks/list",
                    &PaginatedRequestParams {
                        cursor: cursor.clone(),
                    },
                )
                .await?;
            tasks.extend(result.tasks);
            cursor = result.next_cursor;
            if cursor.is_none() {
                break;
            }
        }

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
