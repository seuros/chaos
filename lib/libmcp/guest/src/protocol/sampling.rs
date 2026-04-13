use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use mcp_host::protocol::types::Task;
use mcp_host::protocol::types::TaskMetadata;

use super::Meta;
use super::messages::Role;
use super::messages::SamplingMessageContentBlock;
use super::tools::ToolInfo;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SamplingMessage {
    pub role: Role,
    pub content: super::OneOrMany<SamplingMessageContentBlock>,
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
    pub content: super::OneOrMany<SamplingMessageContentBlock>,
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
    content: super::OneOrMany<SamplingMessageContentBlock>,
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

pub type CreateTaskResult = mcp_host::protocol::types::CreateTaskResult;

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
            && let Ok(task) = serde_json::from_value::<CreateTaskResult>(value.clone())
        {
            return Ok(Self::Task(task));
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

pub type CallToolResponse = TaskOrResult<super::tools::CallToolResult>;
pub type CreateMessageResponse = TaskOrResult<CreateMessageResult>;
pub type CreateElicitationResponse = TaskOrResult<super::elicitation::CreateElicitationResult>;

// Task listing
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
