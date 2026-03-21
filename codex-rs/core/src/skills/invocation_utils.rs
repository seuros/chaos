use std::collections::HashMap;
use std::path::PathBuf;

use crate::codex::Session;
use crate::codex::TurnContext;
use crate::skills::SkillMetadata;

pub(crate) fn build_implicit_skill_path_indexes(
    _skills: Vec<SkillMetadata>,
) -> (
    HashMap<PathBuf, SkillMetadata>,
    HashMap<PathBuf, SkillMetadata>,
) {
    (HashMap::new(), HashMap::new())
}

pub(crate) async fn maybe_emit_implicit_skill_invocation(
    _sess: &Session,
    _turn_context: &TurnContext,
    _command: &str,
    _workdir: Option<&str>,
) {
    // no-op
}
