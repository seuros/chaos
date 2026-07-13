//! Pure history-distillation logic: summary detection, replacement-history
//! construction, and initial-context placement. Session orchestration (the
//! model round-trips that produce the summary) lives in the kernel.

use crate::allotment::TruncationPolicy;
use crate::allotment::approx_token_count;
use crate::allotment::truncate_text;
use crate::event_mapping::parse_turn_item;
use chaos_ipc::items::TurnItem;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::FunctionCallOutputPayload;
use chaos_ipc::models::ResponseItem;

pub const SUMMARIZATION_PROMPT: &str = include_str!("../templates/distill/prompt.md");
pub const SUMMARY_PREFIX: &str = include_str!("../templates/distill/summary_prefix.md");
pub const COMPACT_USER_MESSAGE_MAX_TOKENS: usize = 20_000;

/// Controls whether compaction replacement history must include initial context.
///
/// Pre-turn/manual compaction variants use `DoNotInject`: they replace history with a summary and
/// clear `reference_context_item`, so the next regular turn will fully reinject initial context
/// after compaction.
///
/// Mid-turn compaction must use `BeforeLastUserMessage` because the model is trained to see the
/// compaction summary as the last item in history after mid-turn compaction; we therefore inject
/// initial context into the replacement history just above the last real user message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitialContextInjection {
    BeforeLastUserMessage,
    DoNotInject,
}

pub fn content_items_to_text(content: &[ContentItem]) -> Option<String> {
    let mut pieces = Vec::new();
    for item in content {
        match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                if !text.is_empty() {
                    pieces.push(text.as_str());
                }
            }
            ContentItem::InputImage { .. } => {}
            ContentItem::Document { text, .. } => {
                if !text.is_empty() {
                    pieces.push(text.as_str());
                }
            }
        }
    }
    if pieces.is_empty() {
        None
    } else {
        Some(pieces.join("\n"))
    }
}

pub fn collect_user_messages(items: &[ResponseItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match parse_turn_item(item) {
            Some(TurnItem::UserMessage(user)) => {
                if is_summary_message(&user.message()) {
                    None
                } else {
                    Some(user.message())
                }
            }
            _ => None,
        })
        .collect()
}

pub fn is_summary_message(message: &str) -> bool {
    message.starts_with(format!("{SUMMARY_PREFIX}\n").as_str())
}

/// Inserts canonical initial context into compacted replacement history at the
/// model-expected boundary.
///
/// Placement rules:
/// - Prefer immediately before the last real user message.
/// - If no real user messages remain, insert before the compaction summary so
///   the summary stays last.
/// - If there are no user messages, insert before the last compaction item so
///   that item remains last (remote compaction may return only compaction items).
/// - If there are no user messages or compaction items, append the context.
pub fn insert_initial_context_before_last_real_user_or_summary(
    mut compacted_history: Vec<ResponseItem>,
    initial_context: Vec<ResponseItem>,
) -> Vec<ResponseItem> {
    let mut last_user_or_summary_index = None;
    let mut last_real_user_index = None;
    for (i, item) in compacted_history.iter().enumerate().rev() {
        let Some(TurnItem::UserMessage(user)) = parse_turn_item(item) else {
            continue;
        };
        // Compaction summaries are encoded as user messages, so track both:
        // the last real user message (preferred insertion point) and the last
        // user-message-like item (fallback summary insertion point).
        last_user_or_summary_index.get_or_insert(i);
        if !is_summary_message(&user.message()) {
            last_real_user_index = Some(i);
            break;
        }
    }
    let last_compaction_index = compacted_history
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, item)| matches!(item, ResponseItem::Compaction { .. }).then_some(i));
    let insertion_index = last_real_user_index
        .or(last_user_or_summary_index)
        .or(last_compaction_index);

    // Re-inject canonical context from the current session since we stripped it
    // from the pre-compaction history. Prefer placing it before the last real
    // user message; if there is no real user message left, place it before the
    // summary or compaction item so the compaction item remains last.
    if let Some(insertion_index) = insertion_index {
        compacted_history.splice(insertion_index..insertion_index, initial_context);
    } else {
        compacted_history.extend(initial_context);
    }

    compacted_history
}

pub fn build_compacted_history(
    initial_context: Vec<ResponseItem>,
    user_messages: &[String],
    summary_text: &str,
) -> Vec<ResponseItem> {
    build_compacted_history_with_limit(
        initial_context,
        user_messages,
        summary_text,
        COMPACT_USER_MESSAGE_MAX_TOKENS,
    )
}

fn build_compacted_history_with_limit(
    mut history: Vec<ResponseItem>,
    user_messages: &[String],
    summary_text: &str,
    max_tokens: usize,
) -> Vec<ResponseItem> {
    let mut selected_messages: Vec<String> = Vec::new();
    if max_tokens > 0 {
        let mut remaining = max_tokens;
        for message in user_messages.iter().rev() {
            if remaining == 0 {
                break;
            }
            let tokens = approx_token_count(message);
            if tokens <= remaining {
                selected_messages.push(message.clone());
                remaining = remaining.saturating_sub(tokens);
            } else {
                let truncated = truncate_text(message, TruncationPolicy::Tokens(remaining));
                selected_messages.push(truncated);
                break;
            }
        }
        selected_messages.reverse();
    }

    for message in &selected_messages {
        history.push(ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: message.clone(),
            }],
            end_turn: None,
            phase: None,
        });
    }

    let summary_text = if summary_text.is_empty() {
        "(no summary available)".to_string()
    } else {
        summary_text.to_string()
    };

    history.push(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText { text: summary_text }],
        end_turn: None,
        phase: None,
    });

    history
}

/// Returns whether an item from remote compaction output should be preserved.
///
/// Called while processing the model-provided compacted transcript, before we
/// append fresh canonical context from the current session.
///
/// We drop:
/// - `system` messages because remote output can include stale/duplicated
///   instruction content.
/// - non-user-content `user` messages (session prefix/instruction wrappers),
///   keeping only real user messages as parsed by `parse_turn_item`.
///
/// This intentionally keeps:
/// - `assistant` messages (future remote compaction models may emit them)
/// - `user`-role warnings and compaction-generated summary messages because
///   they parse as `TurnItem::UserMessage`.
pub fn should_keep_compacted_history_item(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message { role, .. } if role == "system" => false,
        ResponseItem::Message { role, .. } if role == "user" => {
            matches!(parse_turn_item(item), Some(TurnItem::UserMessage(_)))
        }
        ResponseItem::Message { role, .. } if role == "assistant" => true,
        ResponseItem::Message { .. } => false,
        ResponseItem::Compaction { .. } => true,
        ResponseItem::Reasoning { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::GhostSnapshot { .. }
        | ResponseItem::Other => false,
    }
}

/// Replacement body for tool outputs trimmed to fit the context window.
pub const TRIMMED_TOOL_OUTPUT_MARKER: &str =
    "Output exceeded the available model context and was trimmed.";

/// Rewrites a tool-call output's payload to the trim marker, keeping the
/// call/output pairing intact instead of deleting items. Returns `None` when
/// the item is not a rewritable tool output or is already trimmed.
pub fn trim_tool_output_item(item: &ResponseItem) -> Option<ResponseItem> {
    let trimmed = FunctionCallOutputPayload::from_text(TRIMMED_TOOL_OUTPUT_MARKER.to_string());
    match item {
        ResponseItem::FunctionCallOutput {
            call_id,
            output,
            tool_name,
        } if *output != trimmed => Some(ResponseItem::FunctionCallOutput {
            call_id: call_id.clone(),
            output: trimmed,
            tool_name: tool_name.clone(),
        }),
        ResponseItem::CustomToolCallOutput {
            call_id,
            output,
            tool_name,
        } if *output != trimmed => Some(ResponseItem::CustomToolCallOutput {
            call_id: call_id.clone(),
            output: trimmed,
            tool_name: tool_name.clone(),
        }),
        _ => None,
    }
}

#[cfg(test)]
#[path = "distill_tests.rs"]
mod tests;
