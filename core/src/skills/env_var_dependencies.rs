use std::sync::Arc;

use crate::codex::Session;
use crate::codex::TurnContext;
use crate::skills::SkillMetadata;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillDependencyInfo {
    pub(crate) skill_name: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
}

pub(crate) async fn resolve_skill_dependencies_for_turn(
    _sess: &Arc<Session>,
    _turn_context: &Arc<TurnContext>,
    _dependencies: &[SkillDependencyInfo],
) {
    // no-op
}

pub(crate) fn collect_env_var_dependencies(
    _mentioned_skills: &[SkillMetadata],
) -> Vec<SkillDependencyInfo> {
    Vec::new()
}
