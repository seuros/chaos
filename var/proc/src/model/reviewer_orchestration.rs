use anyhow::Result;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewAttemptState {
    Selection,
    Spawn,
    ModelExecution,
    OutputParse,
    SubmissionUnknown,
    Acknowledged,
    Cancelled,
    TerminalFailure,
}

impl ReviewAttemptState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Selection => "selection",
            Self::Spawn => "spawn",
            Self::ModelExecution => "model_execution",
            Self::OutputParse => "output_parse",
            Self::SubmissionUnknown => "submission_unknown",
            Self::Acknowledged => "acknowledged",
            Self::Cancelled => "cancelled",
            Self::TerminalFailure => "terminal_failure",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "selection" => Ok(Self::Selection),
            "spawn" => Ok(Self::Spawn),
            "model_execution" => Ok(Self::ModelExecution),
            "output_parse" => Ok(Self::OutputParse),
            "submission_unknown" => Ok(Self::SubmissionUnknown),
            "acknowledged" => Ok(Self::Acknowledged),
            "cancelled" => Ok(Self::Cancelled),
            "terminal_failure" => Ok(Self::TerminalFailure),
            _ => Err(anyhow::anyhow!("invalid review attempt state: {value}")),
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Acknowledged | Self::Cancelled | Self::TerminalFailure
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRun {
    pub id: String,
    pub review_run_subject: String,
    pub attestation_subject: String,
    pub owner_process_id: String,
    pub created_at: jiff::Timestamp,
    pub updated_at: jiff::Timestamp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewerAttempt {
    pub id: String,
    pub run_id: String,
    pub ordinal: i64,
    pub state: ReviewAttemptState,
    pub provider_id: String,
    pub model: String,
    pub account_subject: String,
    pub model_family_subject: String,
    pub reviewer_attempt_subject: String,
    pub idempotency_key: String,
    pub prompt: String,
    pub mcp_server: String,
    pub mcp_tool: String,
    pub process_id: Option<String>,
    pub raw_output: Option<String>,
    pub submission: Option<Value>,
    pub failure: Option<String>,
    pub created_at: jiff::Timestamp,
    pub updated_at: jiff::Timestamp,
    pub completed_at: Option<jiff::Timestamp>,
}

#[derive(Debug, Clone)]
pub struct ReviewRunCreateParams {
    pub id: String,
    pub review_run_subject: String,
    pub attestation_subject: String,
    pub owner_process_id: String,
}

#[derive(Debug, Clone)]
pub struct ReviewerAttemptCreateParams {
    pub id: String,
    pub ordinal: i64,
    pub provider_id: String,
    pub model: String,
    pub account_subject: String,
    pub model_family_subject: String,
    pub reviewer_attempt_subject: String,
    pub idempotency_key: String,
    pub prompt: String,
    pub mcp_server: String,
    pub mcp_tool: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ReviewRunRow {
    pub(crate) id: String,
    pub(crate) review_run_subject: String,
    pub(crate) attestation_subject: String,
    pub(crate) owner_process_id: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

impl TryFrom<ReviewRunRow> for ReviewRun {
    type Error = anyhow::Error;

    fn try_from(row: ReviewRunRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            review_run_subject: row.review_run_subject,
            attestation_subject: row.attestation_subject,
            owner_process_id: row.owner_process_id,
            created_at: timestamp(row.created_at)?,
            updated_at: timestamp(row.updated_at)?,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReviewAttemptTransitionData {
    pub process_id: Option<String>,
    pub raw_output: Option<String>,
    pub submission: Option<Value>,
    pub failure: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ReviewerAttemptRow {
    pub(crate) id: String,
    pub(crate) run_id: String,
    pub(crate) ordinal: i64,
    pub(crate) state: String,
    pub(crate) provider_id: String,
    pub(crate) model: String,
    pub(crate) account_subject: String,
    pub(crate) model_family_subject: String,
    pub(crate) reviewer_attempt_subject: String,
    pub(crate) idempotency_key: String,
    pub(crate) prompt: String,
    pub(crate) mcp_server: String,
    pub(crate) mcp_tool: String,
    pub(crate) process_id: Option<String>,
    pub(crate) raw_output: Option<String>,
    pub(crate) submission_json: Option<String>,
    pub(crate) failure: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) completed_at: Option<i64>,
}

impl TryFrom<ReviewerAttemptRow> for ReviewerAttempt {
    type Error = anyhow::Error;

    fn try_from(row: ReviewerAttemptRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            run_id: row.run_id,
            ordinal: row.ordinal,
            state: ReviewAttemptState::parse(&row.state)?,
            provider_id: row.provider_id,
            model: row.model,
            account_subject: row.account_subject,
            model_family_subject: row.model_family_subject,
            reviewer_attempt_subject: row.reviewer_attempt_subject,
            idempotency_key: row.idempotency_key,
            prompt: row.prompt,
            mcp_server: row.mcp_server,
            mcp_tool: row.mcp_tool,
            process_id: row.process_id,
            raw_output: row.raw_output,
            submission: row
                .submission_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            failure: row.failure,
            created_at: timestamp(row.created_at)?,
            updated_at: timestamp(row.updated_at)?,
            completed_at: row.completed_at.map(timestamp).transpose()?,
        })
    }
}

fn timestamp(seconds: i64) -> Result<jiff::Timestamp> {
    jiff::Timestamp::from_second(seconds)
        .map_err(|error| anyhow::anyhow!("invalid reviewer orchestration timestamp: {error}"))
}
