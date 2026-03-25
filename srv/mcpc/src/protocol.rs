use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

pub use mcp_host::content::annotations::Annotations;
pub use mcp_host::logging::LogLevel;
pub use mcp_host::protocol::methods::McpMethod;
pub use mcp_host::protocol::types::{
    CancelTaskParams, ErrorCode, GetTaskParams, JsonRpcError, JsonRpcMessage, JsonRpcRequest,
    JsonRpcResponse, ListRootsResult, RequestId, Root, SetLevelRequest, Task, TaskMetadata,
    TaskStatus, ToolAnnotations, ToolExecution,
};
pub use mcp_host::protocol::version::{
    JSON_RPC_VERSION, LATEST_PROTOCOL_VERSION, ProtocolVersion, SUPPORTED_PROTOCOL_VERSIONS,
    is_supported_protocol_version,
};

pub type Meta = Value;
pub type StringMap = BTreeMap<String, String>;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmptyObject {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IconTheme {
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Icon {
    pub src: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sizes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<IconTheme>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RootsCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SamplingCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<EmptyObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<EmptyObject>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CompletionCapability {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LoggingCapability {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PromptsCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribe: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FormElicitationCapability {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UrlElicitationCapability {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ElicitationCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub form: Option<FormElicitationCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<UrlElicitationCapability>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TasksToolsCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<EmptyObject>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TasksSamplingCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_message: Option<EmptyObject>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TasksElicitationCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create: Option<EmptyObject>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TasksRequestsCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<TasksToolsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<TasksSamplingCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elicitation: Option<TasksElicitationCapability>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TasksCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list: Option<EmptyObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel: Option<EmptyObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requests: Option<TasksRequestsCapability>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<HashMap<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<RootsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elicitation: Option<ElicitationCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tasks: Option<TasksCapability>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<HashMap<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logging: Option<LoggingCapability>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "completion")]
    pub completions: Option<CompletionCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tasks: Option<TasksCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Implementation {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
}

impl Implementation {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            title: None,
            description: None,
            icons: None,
            website_url: None,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_icons(mut self, icons: Vec<Icon>) -> Self {
        self.icons = Some(icons);
        self
    }

    pub fn with_website_url(mut self, url: impl Into<String>) -> Self {
        self.website_url = Some(url.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    pub client_info: Implementation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: Implementation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(rename = "outputSchema", skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ToolExecution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListToolsResult {
    #[serde(default)]
    pub tools: Vec<ToolInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolRequestParams {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Map<String, Value>>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceContentsText {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "mimeType")]
    pub mime_type: Option<String>,
    pub text: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceContentsBlob {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "mimeType")]
    pub mime_type: Option<String>,
    pub blob: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ResourceContents {
    Text(ResourceContentsText),
    Blob(ResourceContentsBlob),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Meta>,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Meta>,
    },
    Audio {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Meta>,
    },
    ResourceLink {
        uri: String,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", rename = "mimeType")]
        mime_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        icons: Option<Vec<Icon>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Meta>,
    },
    Resource {
        resource: ResourceContents,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Meta>,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            annotations: None,
            meta: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfo {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "mimeType")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTemplateInfo {
    pub uri_template: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "mimeType")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ListResourcesResult {
    #[serde(default)]
    pub resources: Vec<ResourceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ListResourceTemplatesResult {
    #[serde(default)]
    pub resource_templates: Vec<ResourceTemplateInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReadResourceRequestParams {
    pub uri: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReadResourceResult {
    #[serde(default)]
    pub contents: Vec<ResourceContents>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeRequestParams {
    pub uri: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceUpdatedNotificationParams {
    pub uri: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptArgument {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PromptInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PromptMessage {
    pub role: Role,
    pub content: ContentBlock,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GetPromptResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub messages: Vec<PromptMessage>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GetPromptRequestParams {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<StringMap>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ListPromptsResult {
    #[serde(default)]
    pub prompts: Vec<PromptInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTasksResult {
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LogMessageNotificationParams {
    pub level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logger: Option<String>,
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationCompleteNotificationParams {
    pub elicitation_id: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PaginatedRequestParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PromptReference {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "type")]
    pub reference_type: PromptReferenceType,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptReferenceType;

impl PromptReferenceType {
    pub const VALUE: &'static str = "ref/prompt";
}

impl Serialize for PromptReferenceType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(Self::VALUE)
    }
}

impl<'de> Deserialize<'de> for PromptReferenceType {
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
    Prompt(PromptReference),
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ElicitationAction {
    Accept,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ElicitationMode {
    Form,
    Url,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormElicitationRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ElicitationMode>,
    pub message: String,
    pub requested_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UrlElicitationRequest {
    pub mode: ElicitationMode,
    pub message: String,
    pub elicitation_id: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CreateElicitationRequest {
    Url(UrlElicitationRequest),
    Form(FormElicitationRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreateElicitationResult {
    pub action: ElicitationAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationResponse {
    pub action: ElicitationAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

impl From<CreateElicitationResult> for ElicitationResponse {
    fn from(value: CreateElicitationResult) -> Self {
        Self {
            action: value.action,
            content: value.content,
            meta: None,
        }
    }
}

impl From<ElicitationResponse> for CreateElicitationResult {
    fn from(value: ElicitationResponse) -> Self {
        Self {
            action: value.action,
            content: value.content,
        }
    }
}

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
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SamplingMessageContentBlock {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Meta>,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Meta>,
    },
    Audio {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Meta>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Map<String, Value>,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Meta>,
    },
    ToolResult {
        #[serde(rename = "toolUseId")]
        tool_use_id: String,
        content: Vec<ContentBlock>,
        #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
        structured_content: Option<Value>,
        #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Meta>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SamplingMessage {
    pub role: Role,
    pub content: OneOrMany<SamplingMessageContentBlock>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelHint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPreferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<ModelHint>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_priority: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_priority: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intelligence_priority: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ContextInclusion {
    None,
    ThisServer,
    AllServers,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolChoice {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ToolChoiceMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoiceMode {
    Auto,
    Required,
    None,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMessageRequest {
    pub messages: Vec<SamplingMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_preferences: Option<ModelPreferences>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_context: Option<ContextInclusion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskMetadata>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreateMessageResult {
    pub role: Role,
    pub content: OneOrMany<SamplingMessageContentBlock>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurrentCreateMessageResult {
    role: Role,
    content: OneOrMany<SamplingMessageContentBlock>,
    model: String,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(rename = "_meta", default)]
    meta: Option<Meta>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyCreateMessageResult {
    message: SamplingMessage,
    model: String,
    #[serde(default)]
    stop_reason: Option<String>,
}

impl<'de> Deserialize<'de> for CreateMessageResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum WireCreateMessageResult {
            Current(CurrentCreateMessageResult),
            Legacy(LegacyCreateMessageResult),
        }

        match WireCreateMessageResult::deserialize(deserializer)? {
            WireCreateMessageResult::Current(current) => Ok(CreateMessageResult {
                role: current.role,
                content: current.content,
                model: current.model,
                stop_reason: current.stop_reason,
                meta: current.meta,
            }),
            WireCreateMessageResult::Legacy(legacy) => Ok(CreateMessageResult {
                role: legacy.message.role,
                content: legacy.message.content,
                model: legacy.model,
                stop_reason: legacy.stop_reason,
                meta: legacy.message.meta,
            }),
        }
    }
}

impl CreateMessageResult {
    pub const STOP_REASON_END_TURN: &'static str = "endTurn";
    pub const STOP_REASON_STOP_SEQUENCE: &'static str = "stopSequence";
    pub const STOP_REASON_MAX_TOKENS: &'static str = "maxTokens";
    pub const STOP_REASON_TOOL_USE: &'static str = "toolUse";
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum TaskOrResult<T> {
    Result(T),
    Task(CreateTaskResult),
}

impl<'de, T> Deserialize<'de> for TaskOrResult<T>
where
    T: DeserializeOwned,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;

        if value
            .as_object()
            .is_some_and(|object| object.contains_key("task"))
        {
            if let Ok(task) = serde_json::from_value::<CreateTaskResult>(value.clone()) {
                return Ok(Self::Task(task));
            }
        }

        serde_json::from_value::<T>(value.clone())
            .map(Self::Result)
            .or_else(|_| serde_json::from_value::<CreateTaskResult>(value).map(Self::Task))
            .map_err(serde::de::Error::custom)
    }
}

impl<T> TaskOrResult<T> {
    pub fn as_result(&self) -> Option<&T> {
        match self {
            Self::Result(value) => Some(value),
            Self::Task(_) => None,
        }
    }

    pub fn as_task(&self) -> Option<&CreateTaskResult> {
        match self {
            Self::Result(_) => None,
            Self::Task(task) => Some(task),
        }
    }

    pub fn into_result(self) -> Option<T> {
        match self {
            Self::Result(value) => Some(value),
            Self::Task(_) => None,
        }
    }

    pub fn into_task(self) -> Option<CreateTaskResult> {
        match self {
            Self::Result(_) => None,
            Self::Task(task) => Some(task),
        }
    }
}

pub type CreateTaskResult = mcp_host::protocol::types::CreateTaskResult;
pub type CallToolResponse = TaskOrResult<CallToolResult>;
pub type CreateMessageResponse = TaskOrResult<CreateMessageResult>;
pub type CreateElicitationResponse = TaskOrResult<CreateElicitationResult>;

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub server_info: Implementation,
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub instructions: Option<String>,
}

impl ServerInfo {
    pub fn name(&self) -> &str {
        &self.server_info.name
    }

    pub fn version(&self) -> &str {
        &self.server_info.version
    }
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
