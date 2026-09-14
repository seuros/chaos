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

/// JSON-RPC error code a server returns when it refuses the protocol version
/// the client asked for.
pub const UNSUPPORTED_PROTOCOL_VERSION: i32 = -32022;

/// Fixed request id for `initialize`; session ids start above it and the
/// HTTP transport matches on it during session recovery.
pub const INITIALIZE_REQUEST_ID: i64 = 1;

macro_rules! const_str_marker {
    ($name:ident, $value:literal) => {
        #[derive(Debug, Clone, Default, PartialEq, Eq)]
        pub struct $name;

        impl $name {
            pub const VALUE: &'static str = $value;
        }

        impl serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(Self::VALUE)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = <String as serde::Deserialize>::deserialize(deserializer)?;
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
    };
}

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

const_str_marker!(ResourceTemplateReferenceType, "ref/resource");

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
mod tests;
