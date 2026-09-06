//! Source-independent background task lifecycle and recovery records.
//!
//! These records describe work, not instructions or permission grants. The
//! owning process is the journal containing the record.

use crate::ProcessId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskSource {
    Exec {
        session_id: i32,
    },
    Agent {
        process_id: ProcessId,
    },
    AgentMessage {
        process_id: ProcessId,
    },
    Mcp {
        server: String,
        remote_task_id: String,
        /// The non-secret configured endpoint identity, not a connection epoch.
        endpoint: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Submission was journaled, but no execution handle was acquired.
    Submitting,
    Running,
    InputRequired,
    Succeeded,
    Failed,
    Cancelled,
    Lost,
    SubmissionUnknown,
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Submitting | Self::Running | Self::InputRequired)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BackgroundTask {
    pub id: String,
    pub source: Option<TaskSource>,
    pub state: TaskState,
    pub status_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub result: Option<Value>,
    /// The initiating tool response must enter history before notification.
    pub origin_call_id: Option<String>,
    #[serde(default)]
    pub origin_turn_id: Option<String>,
    /// Source execution generation (a child turn ID for agents).
    #[serde(default)]
    pub execution_id: Option<String>,
    pub ready: bool,
    pub notify: bool,
    pub delivered: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WakePolicy {
    #[default]
    Enabled,
    Interrupted,
    Closed,
}

/// Kept in the append-only journal, separately from conversational history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TaskJournalEvent {
    Upsert {
        task: Box<BackgroundTask>,
    },
    Delivered {
        task_ids: Vec<String>,
        turn_id: String,
    },
    WakePolicy {
        policy: WakePolicy,
    },
    ContinuationStarted {
        turn_id: String,
    },
    ContinuationFinished {
        turn_id: String,
    },
    OutputSchema {
        schema: Option<Value>,
    },
    RecoveryContext {
        context: TaskRecoveryContext,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskRecoveryContext {
    pub provider: String,
    pub mode_id: String,
    pub allowed_modes: std::collections::BTreeSet<String>,
    pub switching_allowed: bool,
}

/// A level-triggered snapshot: clients must not infer quiescence from a single
/// TurnComplete event while other work can still wake the process.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessActivity {
    pub active_turn: bool,
    pub outstanding_tasks: usize,
    pub pending_completions: usize,
    pub wake_policy: WakePolicy,
    pub blocked: Option<String>,
}

impl ProcessActivity {
    pub fn is_quiescent(&self) -> bool {
        !self.active_turn && self.outstanding_tasks == 0 && self.pending_completions == 0
    }
}
