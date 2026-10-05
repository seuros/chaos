use super::Plan;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Clarification {
    pub seq: i64,
    pub text: String,
    pub actor: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarificationPage {
    pub entries: Vec<Clarification>,
    pub next_after: Option<i64>,
}

impl ClarificationPage {
    pub(crate) fn new(mut entries: Vec<Clarification>) -> Self {
        let next_after = (entries.len() > 50).then(|| entries[49].seq);
        entries.truncate(50);
        Self {
            entries,
            next_after,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanningContent {
    pub body: String,
    pub body_revision: i16,
    pub incorporated_seq: i64,
    pub clarifications: ClarificationPage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDetail {
    pub plan: Plan,
    #[serde(flatten)]
    pub content: PlanningContent,
}
