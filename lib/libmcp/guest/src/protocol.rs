use std::collections::BTreeMap;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde_json::Value;

pub use mcp_host::content::annotations::Annotations;
pub use mcp_host::logging::LogLevel;
pub use mcp_host::protocol::methods::McpMethod;
pub use mcp_host::protocol::types::CancelTaskParams;
pub use mcp_host::protocol::types::ErrorCode;
pub use mcp_host::protocol::types::GetTaskParams;
pub use mcp_host::protocol::types::JsonRpcError;
pub use mcp_host::protocol::types::JsonRpcMessage;
pub use mcp_host::protocol::types::JsonRpcRequest;
pub use mcp_host::protocol::types::JsonRpcResponse;
pub use mcp_host::protocol::types::ListRootsResult;
pub use mcp_host::protocol::types::RequestId;
pub use mcp_host::protocol::types::Root;
pub use mcp_host::protocol::types::SetLevelRequest;
pub use mcp_host::protocol::types::Task;
pub use mcp_host::protocol::types::TaskMetadata;
pub use mcp_host::protocol::types::TaskStatus;
pub use mcp_host::protocol::types::TaskSupport;
pub use mcp_host::protocol::types::ToolAnnotations;
pub use mcp_host::protocol::types::ToolExecution;
pub use mcp_host::protocol::version::JSON_RPC_VERSION;
pub use mcp_host::protocol::version::LATEST_PROTOCOL_VERSION;
pub use mcp_host::protocol::version::ProtocolVersion;
pub use mcp_host::protocol::version::SUPPORTED_PROTOCOL_VERSIONS;
pub use mcp_host::protocol::version::is_supported_protocol_version;

pub mod capabilities;
pub mod elicitation;
pub mod implementation;
pub mod messages;
pub mod prompts;
pub mod resources;
pub mod sampling;
pub mod tools;

pub use capabilities::ClientCapabilities;
pub use capabilities::CompletionCapability;
pub use capabilities::ElicitationCapability;
pub use capabilities::FormElicitationCapability;
pub use capabilities::Icon;
pub use capabilities::IconTheme;
pub use capabilities::LoggingCapability;
pub use capabilities::PromptsCapability;
pub use capabilities::ResourcesCapability;
pub use capabilities::RootsCapability;
pub use capabilities::SamplingCapability;
pub use capabilities::ServerCapabilities;
pub use capabilities::TasksCapability;
pub use capabilities::TasksElicitationCapability;
pub use capabilities::TasksRequestsCapability;
pub use capabilities::TasksSamplingCapability;
pub use capabilities::TasksToolsCapability;
pub use capabilities::ToolsCapability;
pub use capabilities::UrlElicitationCapability;
pub use elicitation::CreateElicitationRequest;
pub use elicitation::CreateElicitationResult;
pub use elicitation::ElicitationAction;
pub use elicitation::ElicitationCompleteNotificationParams;
pub use elicitation::ElicitationMode;
pub use elicitation::ElicitationResponse;
pub use elicitation::FormElicitationRequest;
pub use elicitation::UrlElicitationRequest;
pub use implementation::Implementation;
pub use implementation::InitializeRequest;
pub use implementation::InitializeResult;
pub use implementation::ServerInfo;
pub use messages::ContentBlock;
pub use messages::PromptMessage;
pub use messages::Role;
pub use messages::SamplingMessageContentBlock;
pub use prompts::GetPromptRequestParams;
pub use prompts::GetPromptResult;
pub use prompts::ListPromptsResult;
pub use prompts::PromptArgument;
pub use prompts::PromptInfo;
pub use prompts::PromptReference;
pub use prompts::PromptReferenceType;
pub use resources::ListResourceTemplatesResult;
pub use resources::ListResourcesResult;
pub use resources::ReadResourceRequestParams;
pub use resources::ReadResourceResult;
pub use resources::ResourceContents;
pub use resources::ResourceContentsBlob;
pub use resources::ResourceContentsText;
pub use resources::ResourceInfo;
pub use resources::ResourceTemplateInfo;
pub use resources::ResourceUpdatedNotificationParams;
pub use resources::SubscribeRequestParams;
pub use sampling::CallToolResponse;
pub use sampling::ContextInclusion;
pub use sampling::CreateElicitationResponse;
pub use sampling::CreateMessageRequest;
pub use sampling::CreateMessageResponse;
pub use sampling::CreateMessageResult;
pub use sampling::CreateTaskResult;
pub use sampling::ListTasksResult;
pub use sampling::ModelHint;
pub use sampling::ModelPreferences;
pub use sampling::SamplingMessage;
pub use sampling::TaskOrResult;
pub use sampling::ToolChoice;
pub use sampling::ToolChoiceMode;
pub use tools::CallToolRequestParams;
pub use tools::CallToolResult;
pub use tools::ListToolsResult;
pub use tools::ToolInfo;

pub type Meta = Value;
pub type StringMap = BTreeMap<String, String>;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmptyObject {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> From<T> for OneOrMany<T> {
    fn from(value: T) -> Self {
        Self::One(value)
    }
}

impl<T> From<Vec<T>> for OneOrMany<T> {
    fn from(value: Vec<T>) -> Self {
        Self::Many(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PaginatedRequestParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

// Completion types — used by the completion API
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTemplateReference {
    #[serde(rename = "type")]
    pub reference_type: ResourceTemplateReferenceType,
    pub uri: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceTemplateReferenceType;

impl ResourceTemplateReferenceType {
    pub const VALUE: &'static str = "ref/resource";
}

impl Serialize for ResourceTemplateReferenceType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(Self::VALUE)
    }
}

impl<'de> Deserialize<'de> for ResourceTemplateReferenceType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value == Self::VALUE {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(format!(
                "expected {}, got {}",
                Self::VALUE,
                value
            )))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum CompletionRef {
    Prompt(prompts::PromptReference),
    Resource(ResourceTemplateReference),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompletionArgument {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompletionContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<StringMap>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompleteRequest {
    #[serde(rename = "ref")]
    pub reference: CompletionRef,
    pub argument: CompletionArgument,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<CompletionContext>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompleteResult {
    pub completion: CompletionInfo,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompletionInfo {
    #[serde(deserialize_with = "deserialize_completion_values")]
    pub values: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
}

fn deserialize_completion_values<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum CompletionValue {
        String(String),
        Object { value: String },
    }

    let values = Vec::<CompletionValue>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .map(|value| match value {
            CompletionValue::String(value) => value,
            CompletionValue::Object { value } => value,
        })
        .collect())
}

// Notification param types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LogMessageNotificationParams {
    pub level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logger: Option<String>,
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CancelledNotificationParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProgressNotificationParams {
    pub progress_token: Value,
    pub progress: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub fn latest_supported_protocol_version() -> &'static str {
    ProtocolVersion::V_2025_11_25
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_versions_are_latest_two() {
        assert_eq!(SUPPORTED_PROTOCOL_VERSIONS, ["2025-11-25", "2025-06-18"]);
        assert!(is_supported_protocol_version("2025-11-25"));
        assert!(is_supported_protocol_version("2025-06-18"));
        assert!(!is_supported_protocol_version("2025-03-26"));
    }

    #[test]
    fn test_call_tool_result_allows_structured_content_without_blocks() {
        let result: CallToolResult = serde_json::from_value(serde_json::json!({
            "structuredContent": {
                "answer": 42
            }
        }))
        .unwrap();

        assert!(result.content.is_empty());
        assert_eq!(result.structured_content.unwrap()["answer"], 42);
    }

    #[test]
    fn test_resource_link_uses_resource_link_tag() {
        let content = ContentBlock::ResourceLink {
            uri: "file:///tmp/example".to_string(),
            name: "example".to_string(),
            title: None,
            description: None,
            mime_type: None,
            size: None,
            icons: None,
            annotations: None,
            meta: None,
        };

        let json = serde_json::to_value(content).unwrap();
        assert_eq!(json["type"], "resource_link");
    }

    #[test]
    fn test_create_message_result_accepts_current_shape() {
        let result: CreateMessageResult = serde_json::from_value(serde_json::json!({
            "role": "assistant",
            "content": {
                "type": "text",
                "text": "hello"
            },
            "model": "test-model",
            "stopReason": "endTurn"
        }))
        .unwrap();

        assert_eq!(result.role, Role::Assistant);
        assert_eq!(result.stop_reason.as_deref(), Some("endTurn"));
    }

    #[test]
    fn test_create_message_result_accepts_legacy_shape() {
        let result: CreateMessageResult = serde_json::from_value(serde_json::json!({
            "message": {
                "role": "assistant",
                "content": {
                    "type": "text",
                    "text": "hello"
                }
            },
            "model": "test-model",
            "stopReason": "endTurn"
        }))
        .unwrap();

        assert_eq!(result.role, Role::Assistant);
        assert_eq!(result.stop_reason.as_deref(), Some("endTurn"));
    }

    #[test]
    fn test_url_elicitation_request_deserializes() {
        let request: CreateElicitationRequest = serde_json::from_value(serde_json::json!({
            "mode": "url",
            "message": "Authorize access",
            "elicitationId": "auth-1",
            "url": "https://example.com/auth"
        }))
        .unwrap();

        match request {
            CreateElicitationRequest::Url(params) => {
                assert_eq!(params.elicitation_id, "auth-1");
            }
            CreateElicitationRequest::Form(_) => panic!("expected url request"),
        }
    }

    #[test]
    fn test_completion_result_accepts_string_values() {
        let result: CompleteResult = serde_json::from_value(serde_json::json!({
            "completion": {
                "values": ["alpha", "beta"],
                "hasMore": false
            }
        }))
        .unwrap();

        assert_eq!(result.completion.values, vec!["alpha", "beta"]);
    }

    #[test]
    fn test_completion_result_accepts_legacy_object_values() {
        let result: CompleteResult = serde_json::from_value(serde_json::json!({
            "completion": {
                "values": [
                    { "value": "alpha", "label": "Alpha" },
                    { "value": "beta" }
                ]
            }
        }))
        .unwrap();

        assert_eq!(result.completion.values, vec!["alpha", "beta"]);
    }

    #[test]
    fn test_server_capabilities_accepts_legacy_completion_field() {
        let capabilities: ServerCapabilities = serde_json::from_value(serde_json::json!({
            "completion": {}
        }))
        .unwrap();

        assert!(capabilities.completions.is_some());
    }

    #[test]
    fn test_cancelled_notification_allows_missing_request_id() {
        let params: CancelledNotificationParams = serde_json::from_value(serde_json::json!({
            "reason": "server shutdown"
        }))
        .unwrap();

        assert!(params.request_id.is_none());
        assert_eq!(params.reason.as_deref(), Some("server shutdown"));
    }

    #[test]
    fn test_call_tool_response_accepts_task_result() {
        let response: CallToolResponse = serde_json::from_value(serde_json::json!({
            "task": {
                "taskId": "task-123",
                "status": "working",
                "createdAt": "2026-03-24T00:00:00Z",
                "lastUpdatedAt": "2026-03-24T00:00:00Z",
                "ttl": null
            }
        }))
        .unwrap();

        let task = response.into_task().expect("expected task result");
        assert_eq!(task.task.task_id, "task-123");
    }
}
