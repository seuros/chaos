//! Bounded, untrusted tree input shared by tool workflows and agent synopses.
//!
//! Validation is a preflight check, not authorization. A runtime adapter must
//! recheck the current caller's policy and retry safety before every attempt.

mod bindings;
mod compile;
mod validation;

use mcp_host::protocol::elicitation::ElicitationSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use bindings::{Binding, BindingError, resolve_arguments};
pub use compile::compile_workflow;
pub use validation::{Domain, TreeError, TreeErrorCode, TreeStats, ValidatedTree, is_control_tool};

/// Maximum number of nodes in one definition.
pub const MAX_NODES: usize = 128;
/// Maximum depth, counting the root as one.
pub const MAX_DEPTH: usize = 16;
/// Conservative upper bound on concurrently active leaves.
pub const MAX_CONCURRENT_LEAVES: usize = 16;
/// Maximum total leaf activations, including explicit retries.
pub const MAX_LEAF_ATTEMPTS: usize = 256;
/// Maximum attempts allowed by one retry decorator, including its first try.
pub const MAX_RETRY_ATTEMPTS: usize = 3;
/// Maximum substitutions on a call leaf.
pub const MAX_BINDINGS: usize = 128;
/// Maximum declared agent jobs, preserving the flat synopsis limit.
pub const MAX_AGENT_JOBS: usize = 16;
/// Maximum explicit workflow or decorator timeout.
pub const MAX_TIMEOUT_MS: u64 = 60 * 60 * 1000;
/// Default tool workflow timeout.
pub const DEFAULT_WORKFLOW_TIMEOUT_MS: u64 = 120_000;

/// A declarative node. Every node has a unique `id`; result-producing leaves
/// store under `result_key`, defaulting to their node ID. `input` is reserved.
///
/// No field contains executable code or implicit string interpolation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TreeNode {
    /// Runs children in order, stopping at the first failure.
    Sequence {
        /// Stable node identifier.
        id: String,
        /// Nonempty ordered children.
        children: Vec<Self>,
    },
    /// Runs children in order until one succeeds.
    Fallback {
        /// Stable node identifier.
        id: String,
        /// Nonempty ordered children.
        children: Vec<Self>,
    },
    /// Succeeds after all children succeed; fails on the first failure.
    ParallelAll {
        /// Stable node identifier.
        id: String,
        /// Nonempty concurrent children.
        children: Vec<Self>,
    },
    /// Finishes on the first observed success or failure and halts losers.
    Race {
        /// Stable node identifier.
        id: String,
        /// Nonempty concurrent children; leftmost decides simultaneous ties.
        children: Vec<Self>,
    },
    /// Fails and halts its child when the deadline expires.
    Timeout {
        /// Stable node identifier.
        id: String,
        /// Positive duration, at most one hour.
        timeout_ms: u64,
        /// The bounded subtree.
        child: Box<Self>,
    },
    /// Repeats a failing subtree up to `attempts` total activations.
    Retry {
        /// Stable node identifier.
        id: String,
        /// Between one and three, including the initial attempt.
        attempts: usize,
        /// Only replay-safe calls/jobs and conditions may appear below it.
        child: Box<Self>,
    },
    /// Checks a prior structured result or the input using JSON Pointer.
    Condition {
        /// Stable node identifier.
        id: String,
        /// An absolute JSON Pointer into the run's result board.
        pointer: String,
        /// Existence or literal JSON equality; never an expression.
        predicate: Predicate,
    },
    /// Dispatches a tool using the caller's current policy.
    Call {
        /// Stable node identifier.
        id: String,
        /// Tool name; never an authorization grant.
        tool: String,
        /// Literal object arguments.
        #[serde(default = "empty_object")]
        arguments: Value,
        /// Explicit JSON Pointer substitutions, resolved before dispatch.
        #[serde(default)]
        bindings: Vec<Binding>,
        /// Result board key, or this node's ID when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_key: Option<String>,
    },
    /// Requests additional user input, not action-specific authorization.
    Ask {
        /// Stable node identifier.
        id: String,
        /// Literal question.
        message: String,
        /// Optional typed form schema; absent means an empty confirmation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schema: Option<ElicitationSchema>,
        /// Result board key, or this node's ID when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_key: Option<String>,
    },
    /// Uses the existing native model path or negotiated MCP sampling.
    Sample {
        /// Stable node identifier.
        id: String,
        /// Literal user prompt.
        prompt: String,
        /// Optional literal system instruction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system: Option<String>,
        /// Positive maximum output tokens.
        #[serde(default = "default_max_tokens")]
        max_tokens: u32,
        /// Result board key, or this node's ID when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_key: Option<String>,
    },
    /// Executes a declared agent job, creating a fresh child per attempt.
    Job {
        /// Stable node identifier.
        id: String,
        /// ID from the call's separate `jobs` list.
        job_id: String,
        /// Result board key, or this node's ID when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_key: Option<String>,
    },
}

impl TreeNode {
    /// This node's stable identifier.
    pub fn id(&self) -> &str {
        match self {
            Self::Sequence { id, .. }
            | Self::Fallback { id, .. }
            | Self::ParallelAll { id, .. }
            | Self::Race { id, .. }
            | Self::Timeout { id, .. }
            | Self::Retry { id, .. }
            | Self::Condition { id, .. }
            | Self::Call { id, .. }
            | Self::Ask { id, .. }
            | Self::Sample { id, .. }
            | Self::Job { id, .. } => id,
        }
    }

    /// The result key written by a successful leaf, if it produces a result.
    pub fn result_key(&self) -> Option<&str> {
        match self {
            Self::Call { result_key, .. }
            | Self::Ask { result_key, .. }
            | Self::Sample { result_key, .. }
            | Self::Job { result_key, .. } => Some(result_key.as_deref().unwrap_or(self.id())),
            _ => None,
        }
    }
}

/// A non-executable condition over JSON data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Predicate {
    /// True when the pointer resolves, including a present JSON null.
    Exists,
    /// True only when a present value equals this literal.
    Equals {
        /// Literal comparison value.
        value: Value,
    },
}

impl Predicate {
    /// Evaluates on a JSON result board; a missing value is always false.
    pub fn matches(&self, board: &Value, pointer: &str) -> bool {
        board.pointer(pointer).is_some_and(|actual| match self {
            Self::Exists => true,
            Self::Equals { value } => actual == value,
        })
    }
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

const fn default_max_tokens() -> u32 {
    1024
}
