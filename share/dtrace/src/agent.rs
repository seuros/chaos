#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookAgentContext {
    pub is_subagent: bool,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub parent_session_id: Option<String>,
    pub agent_depth: Option<i32>,
}
