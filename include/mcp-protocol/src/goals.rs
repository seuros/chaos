//! Acknowledged goal checkpoints; identities come from the transport.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CAPABILITY: &str = "chaos/goals";
pub const METHOD: &str = "chaos/goals";
pub const VERSION: u32 = 2;
pub const MAX_CHECKS: u8 = 4;

pub fn capability() -> Value {
    json!({"version": VERSION, "client": "free_chaos", "durable": true})
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Checking,
    Complete,
    Paused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub id: String,
    pub turn_id: String,
    pub revision: u64,
    pub objective: String,
    pub criteria: Vec<String>,
    pub status: Status,
    pub checks: u8,
    pub reason: String,
    pub attempt_id: Option<String>,
    pub evidence_digest: Option<String>,
    pub verdict_ref: Option<String>,
}

impl Snapshot {
    pub fn validate(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= 128
            && !self.turn_id.is_empty()
            && self.turn_id.len() <= 128
            && self.revision > 0
            && !self.objective.trim().is_empty()
            && self.objective.len() <= 4096
            && !self.criteria.is_empty()
            && self.criteria.len() <= 8
            && self
                .criteria
                .iter()
                .all(|s| !s.trim().is_empty() && s.len() <= 1024)
            && self.checks <= MAX_CHECKS
            && self.reason.len() <= 1024
            && [&self.attempt_id, &self.evidence_digest, &self.verdict_ref]
                .iter()
                .all(|s| s.as_ref().is_none_or(|s| !s.is_empty() && s.len() <= 128))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Evaluation {
    Complete,
    Incomplete { missing: Vec<String> },
    Unknown { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerdictRecord {
    pub revision: u64,
    pub attempt_id: String,
    pub evidence_digest: String,
    pub reference: String,
    pub evaluation: Evaluation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub turn_id: String,
    pub eligible: bool,
    pub pause_reason: Option<String>,
    pub evidence_digest: String,
    pub has_evidence: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Restored {
    pub version: u32,
    pub goal: Option<Snapshot>,
    pub context: Context,
    pub interrupted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Restore,
    Checkpoint {
        expected_revision: u64,
        goal: Snapshot,
    },
    Evaluate {
        revision: u64,
        claim: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub version: u32,
    #[serde(flatten)]
    pub operation: Operation,
}

impl Request {
    pub fn parse(value: Value) -> Result<Self, &'static str> {
        if value.to_string().len() > 24_576 {
            return Err("goal request too large");
        }
        let Value::Object(mut fields) = value else {
            return Err("invalid goal request");
        };
        if fields.remove("version") != Some(json!(VERSION)) {
            return Err("unsupported goal protocol");
        }
        if fields.get("operation") == Some(&json!("restore")) && fields.len() != 1 {
            return Err("unexpected restore arguments");
        }
        let operation =
            serde_json::from_value(Value::Object(fields)).map_err(|_| "invalid goal request")?;
        let request = Self {
            version: VERSION,
            operation,
        };
        match &request.operation {
            Operation::Checkpoint { goal, .. } if !goal.validate() => Err("invalid goal snapshot"),
            Operation::Evaluate { claim, .. } if claim.len() > 8192 => Err("claim too large"),
            _ => Ok(request),
        }
    }
}

#[cfg(test)]
mod tests;
