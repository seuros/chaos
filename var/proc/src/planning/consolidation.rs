use super::Clarification;
use serde::{Deserialize, Serialize};
use strum_macros::{AsRefStr, EnumString};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationRequest {
    pub request_id: String,
    pub plan: String,
    pub task: Option<String>,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsolidationInput {
    pub title: String,
    pub body: String,
    pub body_revision: i16,
    pub through_seq: i64,
    pub clarifications: Vec<Clarification>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, AsRefStr, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ConsolidationState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Stale,
}

impl TryFrom<String> for ConsolidationState {
    type Error = strum::ParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl ConsolidationState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ConsolidationJob {
    pub id: String,
    pub plan_id: String,
    pub task_id: Option<String>,
    pub actor: String,
    pub installation_id: String,
    #[serde(skip_serializing)]
    pub origin_call_id: String,
    #[sqlx(json)]
    pub request: ConsolidationRequest,
    #[serde(skip_serializing)]
    #[sqlx(json)]
    pub input: ConsolidationInput,
    #[sqlx(try_from = "String")]
    pub state: ConsolidationState,
    #[serde(skip_serializing)]
    pub execution_id: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
}
