use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
pub struct HistoryEntry {
    pub conversation_id: String,
    pub ts: u64,
    pub text: String,
}
