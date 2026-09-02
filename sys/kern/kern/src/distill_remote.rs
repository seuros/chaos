use std::sync::Arc;

use crate::Prompt;
use crate::chaos::Session;
use crate::chaos::TurnContext;
use crate::chaos::built_tools;
use crate::client_common::ResponseEvent;
use crate::context_manager::ContextManager;
use crate::context_manager::TotalTokenUsageBreakdown;
use crate::context_manager::estimate_response_item_model_visible_bytes;
use crate::distill::InitialContextInjection;
use crate::distill::insert_initial_context_before_last_real_user_or_summary;
use crate::error::ChaosErr;
use crate::error::Result as ChaosResult;
use crate::protocol::CompactedItem;
use crate::protocol::EventMsg;
use crate::protocol::TurnStartedEvent;
use chaos_context::allotment::TruncationPolicy;
use chaos_context::allotment::truncate_text;
use chaos_context::distill::should_keep_compacted_history_item;
use chaos_context::distill::trim_tool_output_item;
use chaos_ipc::items::ContextCompactionItem;
use chaos_ipc::items::TurnItem;
use chaos_ipc::models::BaseInstructions;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::RateLimitSnapshot;
use chaos_ipc::protocol::TokenUsage;
use futures::Stream;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::error;
use tracing::info;
use tracing::warn;

const RETAINED_MESSAGE_TOKEN_BUDGET: usize = 64_000;
const APPROX_BYTES_PER_TOKEN: usize = 4;

pub(crate) async fn run_inline_remote_auto_distill_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    initial_context_injection: InitialContextInjection,
) -> ChaosResult<()> {
    run_remote_distill_task_inner(&sess, &turn_context, initial_context_injection).await?;
    Ok(())
}

pub(crate) async fn run_remote_distill_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
) -> ChaosResult<()> {
    let start_event = EventMsg::TurnStarted(TurnStartedEvent {
        turn_id: turn_context.sub_id.clone(),
        model_context_window: turn_context.model_context_window(),
        collaboration_mode_kind: turn_context.collaboration_mode.mode,
    });
    sess.send_event(&turn_context, start_event).await;

    run_remote_distill_task_inner(&sess, &turn_context, InitialContextInjection::DoNotInject).await
}

async fn run_remote_distill_task_inner(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    initial_context_injection: InitialContextInjection,
) -> ChaosResult<()> {
    if let Err(err) =
        run_remote_distill_task_inner_impl(sess, turn_context, initial_context_injection).await
    {
        let event = EventMsg::Error(
            err.to_error_event(Some("Error running remote compact task".to_string())),
        );
        sess.send_event(turn_context, event).await;
        return Err(err);
    }
    Ok(())
}

async fn run_remote_distill_task_inner_impl(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    initial_context_injection: InitialContextInjection,
) -> ChaosResult<()> {
    let compaction_item = TurnItem::ContextCompaction(ContextCompactionItem::new());
    sess.emit_turn_item_started(turn_context, &compaction_item)
        .await;
    let mut history = sess.clone_history().await;
    let base_instructions = sess.get_base_instructions().await;
    let trimmed_items = trim_function_call_history_to_fit_context_window(
        &mut history,
        turn_context.as_ref(),
        &base_instructions,
    );
    if trimmed_items > 0 {
        info!(
            turn_id = %turn_context.sub_id,
            trimmed_items,
            "trimmed tool outputs before remote compaction"
        );
    }
    let prompt_input = history.for_prompt(&turn_context.model_info.input_modalities);
    let tool_router = built_tools(
        sess.as_ref(),
        turn_context.as_ref(),
        &prompt_input,
        &CancellationToken::new(),
    )
    .await?;
    let mut prompt = Prompt {
        input: prompt_input,
        tools: tool_router.model_visible_specs(),
        parallel_tool_calls: turn_context.model_info.supports_parallel_tool_calls,
        base_instructions,
        personality: turn_context.personality,
        output_schema: None,
    };
    let replacement_input = prompt.input.clone();
    prompt.input.push(ResponseItem::CompactionTrigger {});

    let compaction_output = match request_remote_compaction_v2(sess, turn_context, &prompt).await {
        Ok(output) => output,
        Err(err) => {
            let total_usage_breakdown = sess.get_total_token_usage_breakdown().await;
            let compact_request_log_data =
                build_compact_request_log_data(&prompt.input, &prompt.base_instructions.text);
            log_remote_compact_failure(
                turn_context,
                &compact_request_log_data,
                total_usage_breakdown,
                &err,
            );
            return Err(err);
        }
    };
    let new_history = build_v2_compacted_history(replacement_input, compaction_output);
    let new_history = process_compacted_history(
        sess.as_ref(),
        turn_context.as_ref(),
        new_history,
        initial_context_injection,
    )
    .await;

    let reference_context_item = match initial_context_injection {
        InitialContextInjection::DoNotInject => None,
        InitialContextInjection::BeforeLastUserMessage => Some(turn_context.to_turn_context_item()),
    };
    let compacted_item = CompactedItem {
        message: String::new(),
        replacement_history: Some(new_history.clone()),
    };
    sess.replace_compacted_history(new_history, reference_context_item, compacted_item)
        .await;
    sess.recompute_token_usage(turn_context).await;

    sess.emit_turn_item_completed(turn_context, compaction_item)
        .await;
    Ok(())
}

#[derive(Debug)]
struct RemoteCompactionOutput {
    item: ResponseItem,
    token_usage: Option<TokenUsage>,
    server_reasoning_included: Option<bool>,
    rate_limits: Vec<RateLimitSnapshot>,
}

async fn request_remote_compaction_v2(
    sess: &Session,
    turn_context: &TurnContext,
    prompt: &Prompt,
) -> ChaosResult<ResponseItem> {
    let mut client_session = sess.services.model_client.new_session();
    let turn_metadata_header = turn_context.turn_metadata_state.current_header_value();
    sess.record_provider_request_started(turn_context).await;
    let stream = client_session
        .stream(
            prompt,
            &turn_context.model_info,
            &turn_context.session_telemetry,
            turn_context.reasoning_effort,
            turn_context.reasoning_summary,
            turn_context.config.service_tier,
            turn_metadata_header.as_deref(),
        )
        .await?;
    let output = collect_compaction_output(stream).await?;

    if let Some(included) = output.server_reasoning_included {
        sess.set_server_reasoning_included(included).await;
    }
    for snapshot in output.rate_limits {
        sess.update_rate_limits(turn_context, snapshot).await;
    }
    sess.update_token_usage_info(turn_context, output.token_usage.as_ref())
        .await;

    Ok(output.item)
}

async fn collect_compaction_output<S>(mut stream: S) -> ChaosResult<RemoteCompactionOutput>
where
    S: Stream<Item = ChaosResult<ResponseEvent>> + Unpin,
{
    let mut output_item_count = 0usize;
    let mut compaction_count = 0usize;
    let mut compaction_output = None;
    let mut server_reasoning_included = None;
    let mut rate_limits = Vec::new();

    while let Some(event) = stream.next().await {
        match event? {
            ResponseEvent::OutputItemDone(item) => {
                output_item_count += 1;
                if matches!(item, ResponseItem::Compaction { .. }) {
                    compaction_count += 1;
                    if compaction_output.is_none() {
                        compaction_output = Some(item);
                    }
                }
            }
            ResponseEvent::ServerReasoningIncluded(included) => {
                server_reasoning_included = Some(included);
            }
            ResponseEvent::RateLimits(snapshot) => rate_limits.push(snapshot),
            ResponseEvent::Completed { token_usage, .. } => {
                return match (compaction_count, compaction_output) {
                    (1, Some(item)) => Ok(RemoteCompactionOutput {
                        item,
                        token_usage,
                        server_reasoning_included,
                        rate_limits,
                    }),
                    _ => Err(ChaosErr::Fatal(format!(
                        "remote compaction v2 expected exactly one compaction output item, got {compaction_count} from {output_item_count} output items"
                    ))),
                };
            }
            _ => {}
        }
    }

    Err(ChaosErr::Stream(
        "remote compaction v2 stream closed before response.completed".into(),
        None,
    ))
}

fn build_v2_compacted_history(
    prompt_input: Vec<ResponseItem>,
    compaction_output: ResponseItem,
) -> Vec<ResponseItem> {
    build_v2_compacted_history_with_budget(
        prompt_input,
        compaction_output,
        RETAINED_MESSAGE_TOKEN_BUDGET,
    )
}

fn build_v2_compacted_history_with_budget(
    prompt_input: Vec<ResponseItem>,
    compaction_output: ResponseItem,
    token_budget: usize,
) -> Vec<ResponseItem> {
    let mut retained_reversed = Vec::new();
    let mut remaining_tokens = token_budget;

    for item in prompt_input.into_iter().rev() {
        if remaining_tokens == 0 {
            break;
        }
        let is_retained_message = matches!(
            &item,
            ResponseItem::Message { role, .. }
                if matches!(role.as_str(), "user" | "developer" | "system")
        );
        if !is_retained_message {
            continue;
        }

        let item_bytes = usize::try_from(estimate_response_item_model_visible_bytes(&item))
            .unwrap_or(usize::MAX);
        let item_tokens = item_bytes
            .saturating_add(APPROX_BYTES_PER_TOKEN - 1)
            .checked_div(APPROX_BYTES_PER_TOKEN)
            .unwrap_or(usize::MAX);
        if item_tokens <= remaining_tokens {
            remaining_tokens = remaining_tokens.saturating_sub(item_tokens);
            retained_reversed.push(item);
        } else if let Some(truncated) =
            truncate_retained_message_to_token_budget(item, remaining_tokens)
        {
            retained_reversed.push(truncated);
            remaining_tokens = 0;
        }
    }

    retained_reversed.reverse();
    retained_reversed.push(compaction_output);
    retained_reversed
}

fn truncate_retained_message_to_token_budget(
    item: ResponseItem,
    token_budget: usize,
) -> Option<ResponseItem> {
    let ResponseItem::Message {
        id,
        role,
        content,
        end_turn,
        phase,
    } = item
    else {
        return None;
    };

    let mut remaining = token_budget;
    let mut truncated_content = Vec::with_capacity(content.len());
    for mut content_item in content {
        match &mut content_item {
            ContentItem::InputText { text }
            | ContentItem::OutputText { text }
            | ContentItem::Document { text, .. } => {
                if remaining == 0 {
                    continue;
                }
                let item_tokens = text
                    .len()
                    .saturating_add(APPROX_BYTES_PER_TOKEN - 1)
                    .checked_div(APPROX_BYTES_PER_TOKEN)
                    .unwrap_or(usize::MAX);
                if item_tokens <= remaining {
                    remaining = remaining.saturating_sub(item_tokens);
                } else {
                    *text = truncate_text(text, TruncationPolicy::Tokens(remaining));
                    remaining = 0;
                }
                if !text.is_empty() {
                    truncated_content.push(content_item);
                }
            }
            ContentItem::InputImage { .. } => truncated_content.push(content_item),
        }
    }

    (!truncated_content.is_empty()).then_some(ResponseItem::Message {
        id,
        role,
        content: truncated_content,
        end_turn,
        phase,
    })
}

pub(crate) async fn process_compacted_history(
    sess: &Session,
    turn_context: &TurnContext,
    mut compacted_history: Vec<ResponseItem>,
    initial_context_injection: InitialContextInjection,
) -> Vec<ResponseItem> {
    // Mid-turn compaction is the only path that must inject initial context above the last user
    // message in the replacement history. Pre-turn compaction instead injects context after the
    // compaction item, but mid-turn compaction keeps the compaction item last for model training.
    let initial_context = if matches!(
        initial_context_injection,
        InitialContextInjection::BeforeLastUserMessage
    ) {
        sess.build_initial_context(turn_context).await
    } else {
        Vec::new()
    };

    compacted_history.retain(should_keep_compacted_history_item);
    insert_initial_context_before_last_real_user_or_summary(compacted_history, initial_context)
}

#[derive(Debug)]
struct CompactRequestLogData {
    failing_compaction_request_model_visible_bytes: i64,
}

fn build_compact_request_log_data(
    input: &[ResponseItem],
    instructions: &str,
) -> CompactRequestLogData {
    let failing_compaction_request_model_visible_bytes = input
        .iter()
        .map(estimate_response_item_model_visible_bytes)
        .fold(
            i64::try_from(instructions.len()).unwrap_or(i64::MAX),
            i64::saturating_add,
        );

    CompactRequestLogData {
        failing_compaction_request_model_visible_bytes,
    }
}

fn log_remote_compact_failure(
    turn_context: &TurnContext,
    log_data: &CompactRequestLogData,
    total_usage_breakdown: TotalTokenUsageBreakdown,
    err: &ChaosErr,
) {
    error!(
        turn_id = %turn_context.sub_id,
        last_api_response_total_tokens = total_usage_breakdown.last_api_response_total_tokens,
        all_history_items_model_visible_bytes = total_usage_breakdown.all_history_items_model_visible_bytes,
        estimated_tokens_of_items_added_since_last_successful_api_response = total_usage_breakdown.estimated_tokens_of_items_added_since_last_successful_api_response,
        estimated_bytes_of_items_added_since_last_successful_api_response = total_usage_breakdown.estimated_bytes_of_items_added_since_last_successful_api_response,
        model_context_window_tokens = ?turn_context.model_context_window(),
        failing_compaction_request_model_visible_bytes = log_data.failing_compaction_request_model_visible_bytes,
        compact_error = %err,
        "remote compaction failed"
    );
}

fn trim_function_call_history_to_fit_context_window(
    history: &mut ContextManager,
    turn_context: &TurnContext,
    base_instructions: &BaseInstructions,
) -> usize {
    let mut trimmed_items = 0usize;
    let Some(context_window) = turn_context.model_context_window() else {
        return trimmed_items;
    };

    // Rewrite the oldest tool-call outputs to a short marker until the
    // estimate fits: call/output pairs stay intact and the prefix cache keeps
    // as much of its head as possible, unlike deleting items outright.
    let mut next_index = 0usize;
    while history
        .estimate_token_count_with_base_instructions(base_instructions)
        .is_some_and(|estimated_tokens| estimated_tokens > context_window)
    {
        let Some((index, rewritten)) = history
            .raw_items()
            .iter()
            .enumerate()
            .skip(next_index)
            .find_map(|(index, item)| {
                trim_tool_output_item(item).map(|rewritten| (index, rewritten))
            })
        else {
            // Nothing left to trim; the request may still exceed the window
            // and fail at the provider.
            warn!(
                turn_id = %turn_context.sub_id,
                trimmed_items,
                "history still exceeds the context window after trimming all tool outputs"
            );
            break;
        };
        if !history.rewrite_item(index, rewritten) {
            break;
        }
        next_index = index + 1;
        trimmed_items += 1;
    }

    trimmed_items
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::iter;

    fn completed() -> ResponseEvent {
        ResponseEvent::Completed {
            response_id: "resp_1".to_string(),
            token_usage: None,
        }
    }

    #[tokio::test]
    async fn compaction_stream_requires_exactly_one_item_and_completion() {
        let events = vec![
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Compaction {
                encrypted_content: "encrypted".to_string(),
            })),
            Ok(completed()),
        ];

        let output = collect_compaction_output(iter(events))
            .await
            .expect("valid compaction stream");

        assert_eq!(
            output.item,
            ResponseItem::Compaction {
                encrypted_content: "encrypted".to_string()
            }
        );
    }

    #[tokio::test]
    async fn compaction_stream_rejects_missing_output() {
        let error = collect_compaction_output(iter(vec![Ok(completed())]))
            .await
            .expect_err("missing compaction output must fail");

        assert!(error.to_string().contains("got 0"));
    }

    #[tokio::test]
    async fn compaction_stream_rejects_multiple_outputs() {
        let item = ResponseItem::Compaction {
            encrypted_content: "encrypted".to_string(),
        };
        let error = collect_compaction_output(iter(vec![
            Ok(ResponseEvent::OutputItemDone(item.clone())),
            Ok(ResponseEvent::OutputItemDone(item)),
            Ok(completed()),
        ]))
        .await
        .expect_err("multiple compaction outputs must fail");

        assert!(error.to_string().contains("got 2"));
    }

    #[tokio::test]
    async fn compaction_stream_requires_response_completed() {
        let error = collect_compaction_output(iter(vec![Ok(ResponseEvent::OutputItemDone(
            ResponseItem::Compaction {
                encrypted_content: "encrypted".to_string(),
            },
        ))]))
        .await
        .expect_err("incomplete compaction stream must fail");

        assert!(error.to_string().contains("before response.completed"));
    }

    #[test]
    fn v2_history_retains_messages_but_not_tool_transcript_or_trigger() {
        let user = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "keep me".to_string(),
            }],
            end_turn: None,
            phase: None,
        };
        let compaction = ResponseItem::Compaction {
            encrypted_content: "encrypted".to_string(),
        };
        let history = build_v2_compacted_history(
            vec![
                user.clone(),
                ResponseItem::FunctionCall {
                    id: None,
                    name: "read_file".to_string(),
                    namespace: None,
                    arguments: "{}".to_string(),
                    call_id: "call_1".to_string(),
                    provider_metadata: None,
                },
                ResponseItem::CompactionTrigger {},
            ],
            compaction.clone(),
        );

        assert_eq!(history, vec![user, compaction]);
    }

    #[test]
    fn v2_history_truncates_oversized_newest_message_instead_of_skipping_it() {
        let older = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "older".to_string(),
            }],
            end_turn: None,
            phase: None,
        };
        let newest = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "newest ".repeat(100),
            }],
            end_turn: None,
            phase: None,
        };
        let compaction = ResponseItem::Compaction {
            encrypted_content: "encrypted".to_string(),
        };

        let history =
            build_v2_compacted_history_with_budget(vec![older, newest], compaction.clone(), 8);

        assert_eq!(history.len(), 2);
        let ResponseItem::Message { content, .. } = &history[0] else {
            panic!("expected retained newest user message");
        };
        let ContentItem::InputText { text } = &content[0] else {
            panic!("expected retained user text");
        };
        assert!(text.contains("tokens truncated"));
        assert!(text.starts_with("newest"));
        assert_eq!(history[1], compaction);
    }
}
