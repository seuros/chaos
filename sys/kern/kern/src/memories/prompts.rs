use crate::truncate::TruncationPolicy;
use crate::truncate::truncate_text;
use chaos_ipc::openai_models::ModelInfo;
use chaos_proc::Phase2InputSelection;
use std::path::Path;

// Re-export constants for tests (they reach these via `use super::*`).
#[cfg(test)]
use chaos_memento::prompts::CONTEXT_WINDOW_PERCENT;
#[cfg(test)]
use chaos_memento::prompts::DEFAULT_STAGE_ONE_ROLLOUT_TOKEN_LIMIT;

fn token_truncate(text: &str, token_limit: usize) -> String {
    truncate_text(text, TruncationPolicy::Tokens(token_limit))
}

/// Builds the consolidation subagent prompt for a specific memory root.
pub(super) fn build_consolidation_prompt(
    memory_root: &Path,
    selection: &Phase2InputSelection,
) -> String {
    chaos_memento::prompts::build_consolidation_prompt(memory_root, selection)
}

/// Builds the stage-1 user message containing rollout metadata and content.
pub(super) fn build_stage_one_input_message(
    model_info: &ModelInfo,
    rollout_path: &Path,
    rollout_cwd: &Path,
    rollout_contents: &str,
) -> anyhow::Result<String> {
    chaos_memento::prompts::build_stage_one_input_message(
        model_info,
        rollout_path,
        rollout_cwd,
        rollout_contents,
        token_truncate,
    )
}

/// Build prompt used for read path.
pub(crate) async fn build_memory_tool_developer_instructions(codex_home: &Path) -> Option<String> {
    chaos_memento::prompts::build_memory_tool_developer_instructions(codex_home, token_truncate)
        .await
}

#[cfg(test)]
#[path = "prompts_tests.rs"]
mod tests;
