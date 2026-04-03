use crate::storage::rollout_summary_file_stem_from_parts;
use chaos_ipc::openai_models::ModelInfo;
use chaos_proc::Phase2InputSelection;
use chaos_proc::Stage1Output;
use chaos_proc::Stage1OutputRef;
use minijinja::Environment;
use std::path::Path;
use tokio::fs;
use tracing::warn;

/// Fallback stage-1 rollout truncation limit (tokens) when model metadata
/// does not include a valid context window.
pub const DEFAULT_STAGE_ONE_ROLLOUT_TOKEN_LIMIT: usize = 150_000;

/// Maximum number of tokens from `memory_summary.md` injected into memory
/// tool developer instructions.
pub const MEMORY_TOOL_DEVELOPER_INSTRUCTIONS_SUMMARY_TOKEN_LIMIT: usize = 5_000;

/// Portion of the model effective input window reserved for the stage-1
/// rollout input.
///
/// Keeping this below 100% leaves room for system instructions, prompt
/// framing, and model output.
pub const CONTEXT_WINDOW_PERCENT: i64 = 70;

const CONSOLIDATION_TEMPLATE: &str = include_str!("../templates/memories/consolidation.md");
const STAGE_ONE_INPUT_TEMPLATE: &str = include_str!("../templates/memories/stage_one_input.md");
const READ_PATH_TEMPLATE: &str = include_str!("../templates/memories/read_path.md");

fn render(source: &str, ctx: minijinja::value::Value) -> Option<String> {
    let mut env = Environment::new();
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    env.add_template("t", source).ok()?;
    env.get_template("t").ok()?.render(ctx).ok()
}

/// Builds the consolidation subagent prompt for a specific memory root.
pub fn build_consolidation_prompt(memory_root: &Path, selection: &Phase2InputSelection) -> String {
    let memory_root = memory_root.display().to_string();
    let phase2_input_selection = render_phase2_input_selection(selection);
    let ctx = minijinja::context! {
        memory_root => memory_root,
        phase2_input_selection => phase2_input_selection,
    };
    render(CONSOLIDATION_TEMPLATE, ctx).unwrap_or_else(|| {
        warn!("failed to render memories consolidation prompt template");
        format!(
            "## Memory Phase 2 (Consolidation)\nConsolidate Codex memories in: {memory_root}\n\n{phase2_input_selection}"
        )
    })
}

fn render_phase2_input_selection(selection: &Phase2InputSelection) -> String {
    let retained = selection.retained_process_ids.len();
    let added = selection.selected.len().saturating_sub(retained);
    let selected = if selection.selected.is_empty() {
        "- none".to_string()
    } else {
        selection
            .selected
            .iter()
            .map(|item| {
                render_selected_input_line(
                    item,
                    selection.retained_process_ids.contains(&item.process_id),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let removed = if selection.removed.is_empty() {
        "- none".to_string()
    } else {
        selection
            .removed
            .iter()
            .map(render_removed_input_line)
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "- selected inputs this run: {}\n- newly added since the last successful Phase 2 run: {added}\n- retained from the last successful Phase 2 run: {retained}\n- removed from the last successful Phase 2 run: {}\n\nCurrent selected Phase 1 inputs:\n{selected}\n\nRemoved from the last successful Phase 2 selection:\n{removed}\n",
        selection.selected.len(),
        selection.removed.len(),
    )
}

fn render_selected_input_line(item: &Stage1Output, retained: bool) -> String {
    let status = if retained { "retained" } else { "added" };
    let rollout_summary_file = format!(
        "rollout_summaries/{}.md",
        rollout_summary_file_stem_from_parts(
            item.process_id,
            item.source_updated_at,
            item.rollout_slug.as_deref(),
        )
    );
    format!(
        "- [{status}] process_id={}, rollout_summary_file={rollout_summary_file}",
        item.process_id
    )
}

fn render_removed_input_line(item: &Stage1OutputRef) -> String {
    let rollout_summary_file = format!(
        "rollout_summaries/{}.md",
        rollout_summary_file_stem_from_parts(
            item.process_id,
            item.source_updated_at,
            item.rollout_slug.as_deref(),
        )
    );
    format!(
        "- process_id={}, rollout_summary_file={rollout_summary_file}",
        item.process_id
    )
}

/// Builds the stage-1 user message containing rollout metadata and content.
pub fn build_stage_one_input_message(
    model_info: &ModelInfo,
    process_ref: &str,
    rollout_cwd: &Path,
    rollout_contents: &str,
    truncate_fn: impl Fn(&str, usize) -> String,
) -> anyhow::Result<String> {
    let rollout_token_limit = model_info
        .context_window
        .and_then(|limit| (limit > 0).then_some(limit))
        .map(|limit| limit.saturating_mul(model_info.effective_context_window_percent) / 100)
        .map(|limit| (limit.saturating_mul(CONTEXT_WINDOW_PERCENT) / 100).max(1))
        .and_then(|limit| usize::try_from(limit).ok())
        .unwrap_or(DEFAULT_STAGE_ONE_ROLLOUT_TOKEN_LIMIT);
    let truncated_rollout_contents = truncate_fn(rollout_contents, rollout_token_limit);
    let rollout_cwd = rollout_cwd.display().to_string();

    let ctx = minijinja::context! {
        process_ref => process_ref,
        rollout_cwd => rollout_cwd,
        rollout_contents => truncated_rollout_contents,
    };
    render(STAGE_ONE_INPUT_TEMPLATE, ctx)
        .ok_or_else(|| anyhow::anyhow!("failed to render stage_one_input template"))
}

/// Build prompt used for read path.
pub async fn build_memory_tool_developer_instructions(
    chaos_home: &Path,
    truncate_fn: impl Fn(&str, usize) -> String,
) -> Option<String> {
    let base_path = chaos_home.join("memories");
    let memory_summary_path = base_path.join("memory_summary.md");
    let memory_summary = fs::read_to_string(&memory_summary_path)
        .await
        .ok()?
        .trim()
        .to_string();
    let memory_summary = truncate_fn(
        &memory_summary,
        MEMORY_TOOL_DEVELOPER_INSTRUCTIONS_SUMMARY_TOKEN_LIMIT,
    );
    if memory_summary.is_empty() {
        return None;
    }
    let base_path = base_path.display().to_string();
    let ctx = minijinja::context! {
        base_path => base_path,
        memory_summary => memory_summary,
    };
    render(READ_PATH_TEMPLATE, ctx)
}
