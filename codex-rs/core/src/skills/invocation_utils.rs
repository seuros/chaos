use crate::codex::Session;
use crate::codex::TurnContext;

pub(crate) async fn maybe_emit_implicit_skill_invocation(
    _sess: &Session,
    _turn_context: &TurnContext,
    _command: &str,
    _workdir: Option<&str>,
) {
    // no-op
}
