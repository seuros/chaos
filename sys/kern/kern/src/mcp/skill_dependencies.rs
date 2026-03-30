use tokio_util::sync::CancellationToken;

use crate::chaos::Session;
use crate::chaos::TurnContext;
use crate::skills::SkillMetadata;

pub(crate) async fn maybe_prompt_and_install_mcp_dependencies(
    _sess: &Session,
    _turn_context: &TurnContext,
    _cancellation_token: &CancellationToken,
    _mentioned_skills: &[SkillMetadata],
) {
    // no-op
}
