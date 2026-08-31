use std::collections::HashSet;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::instrument;
use tracing::warn;

use crate::client::ModelClientSession;
use crate::client_common::Prompt;
use crate::error::ChaosErr;
use crate::error::Result as ChaosResult;
use crate::tools::ToolRouter;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::parallel::ToolCallRuntime;
use crate::util::backoff;
use chaos_mcp_runtime::McpServerInstructions;

use super::super::Session;
use super::super::TurnContext;
use super::SamplingRequestResult;
use super::execution::try_run_sampling_request;
use super::progress::TurnProgressTracker;

fn append_mcp_server_instructions(
    base_instructions: &mut chaos_ipc::models::BaseInstructions,
    server_instructions: &[McpServerInstructions],
) {
    if server_instructions.is_empty() {
        return;
    }

    base_instructions.text.push_str(
        "\n\n# MCP server instructions\n\n\
         The following instructions were provided by configured MCP servers. \
         Apply each section when using that server's tools or resources.",
    );
    for entry in server_instructions {
        base_instructions.text.push_str(&format!(
            "\n\n## {}\n\n{}",
            entry.server_name, entry.instructions
        ));
    }
}

pub(super) fn build_prompt(
    input: Vec<chaos_ipc::models::ResponseItem>,
    router: &ToolRouter,
    turn_context: &TurnContext,
    base_instructions: chaos_ipc::models::BaseInstructions,
) -> Prompt {
    let deferred_dynamic_tools = turn_context
        .dynamic_tools
        .iter()
        .filter(|tool| tool.defer_loading)
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();
    let tools = if deferred_dynamic_tools.is_empty() {
        router.model_visible_specs()
    } else {
        router
            .model_visible_specs()
            .into_iter()
            .filter(|spec| !deferred_dynamic_tools.contains(spec.name()))
            .collect()
    };

    Prompt {
        input,
        tools,
        parallel_tool_calls: turn_context.model_info.supports_parallel_tool_calls,
        base_instructions,
        personality: turn_context.personality,
        output_schema: turn_context.final_output_json_schema.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
#[instrument(level = "trace",
    skip_all,
    fields(
        turn_id = %turn_context.sub_id,
        model = %turn_context.model_info.slug,
        cwd = %turn_context.cwd.display()
    )
)]
pub(super) async fn run_sampling_request(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    turn_diff_tracker: SharedTurnDiffTracker,
    client_session: &mut ModelClientSession,
    progress: &mut TurnProgressTracker,
    turn_metadata_header: Option<&str>,
    input: Vec<chaos_ipc::models::ResponseItem>,
    server_model_warning_emitted_for_turn: &mut bool,
    cancellation_token: CancellationToken,
) -> ChaosResult<SamplingRequestResult> {
    let router = super::super::built_tools(
        sess.as_ref(),
        turn_context.as_ref(),
        &input,
        &cancellation_token,
    )
    .await?;

    let mut base_instructions = sess.get_base_instructions().await;
    let server_instructions = sess
        .services
        .mcp_registry
        .current_manager()
        .server_instructions()
        .await;
    append_mcp_server_instructions(&mut base_instructions, &server_instructions);

    let prompt = build_prompt(
        input,
        router.as_ref(),
        turn_context.as_ref(),
        base_instructions,
    );
    let tool_runtime = ToolCallRuntime::new(
        Arc::clone(&router),
        Arc::clone(&sess),
        Arc::clone(&turn_context),
        Arc::clone(&turn_diff_tracker),
    );
    let mut retries = 0;
    let mut last_server_model: Option<String> = None;
    loop {
        let err = match try_run_sampling_request(
            tool_runtime.clone(),
            Arc::clone(&sess),
            Arc::clone(&turn_context),
            client_session,
            turn_metadata_header,
            Arc::clone(&turn_diff_tracker),
            progress,
            server_model_warning_emitted_for_turn,
            &mut last_server_model,
            &prompt,
            cancellation_token.child_token(),
        )
        .await
        {
            Ok(output) => {
                return Ok(output);
            }
            Err(ChaosErr::ContextWindowExceeded) => {
                sess.set_total_tokens_full(&turn_context).await;
                return Err(ChaosErr::ContextWindowExceeded);
            }
            Err(ChaosErr::UsageLimitReached(e)) => {
                let rate_limits = e.rate_limits.clone();
                if let Some(rate_limits) = rate_limits {
                    sess.update_rate_limits(&turn_context, *rate_limits).await;
                }
                return Err(ChaosErr::UsageLimitReached(e));
            }
            Err(err) => err,
        };

        if !err.is_retryable() {
            return Err(err);
        }

        // Use the configured provider-specific stream retry budget.
        let max_retries = turn_context.provider.stream_max_retries();
        if retries < max_retries {
            retries += 1;
            let delay = match &err {
                ChaosErr::Stream(_, requested_delay) => {
                    requested_delay.unwrap_or_else(|| backoff(retries))
                }
                _ => backoff(retries),
            };
            warn!(
                "stream disconnected - retrying sampling request ({retries}/{max_retries} in {delay:?})...",
            );

            // Surface retry information to any UI/front-end so the
            // user understands what is happening instead of staring
            // at a seemingly frozen screen.
            sess.notify_stream_error(
                &turn_context,
                format!("Reconnecting... {retries}/{max_retries}"),
                err,
            )
            .await;
            tokio::time::sleep(delay).await;
        } else {
            return Err(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_labeled_mcp_server_instructions() {
        let mut base = chaos_ipc::models::BaseInstructions {
            text: "Base instructions.".to_string(),
        };

        append_mcp_server_instructions(
            &mut base,
            &[
                McpServerInstructions {
                    server_name: "alpha".to_string(),
                    instructions: "Use alpha carefully.".to_string(),
                },
                McpServerInstructions {
                    server_name: "beta".to_string(),
                    instructions: "Prefer beta resources.".to_string(),
                },
            ],
        );

        assert!(base.text.starts_with("Base instructions."));
        assert!(base.text.contains("# MCP server instructions"));
        assert!(base.text.contains("## alpha\n\nUse alpha carefully."));
        assert!(base.text.contains("## beta\n\nPrefer beta resources."));
    }

    #[test]
    fn leaves_base_instructions_unchanged_without_mcp_instructions() {
        let mut base = chaos_ipc::models::BaseInstructions {
            text: "Base instructions.".to_string(),
        };

        append_mcp_server_instructions(&mut base, &[]);

        assert_eq!(base.text, "Base instructions.");
    }
}
