//! One text-only request on the owning session's provider/accounting path.
//! Never run the normal model/tool loop or a clamped CLI conversation here.

mod output;

use super::dispatch::Caller;
use crate::client_common::Prompt;
use chaos_ipc::models::{ContentItem, ResponseItem};
use mcp_host::protocol::types::{
    ContextInclusion, CreateMessageParams, CreateMessageResult, Role, SamplingContent,
    SamplingMessageContent,
};
use std::num::NonZeroU32;

const MAX_SAMPLE_TOKENS: u32 = 16_384;
const MAX_SAMPLE_INPUT_BYTES: usize = 128 * 1024;

/// SDK native Sample currently constructs one user text message. Reject
/// unsupported extensions rather than silently ignoring requested semantics.
fn validate(params: &CreateMessageParams) -> Result<NonZeroU32, String> {
    params.validate().map_err(|error| error.to_string())?;
    if params.model_preferences.is_some()
        || !matches!(params.include_context, None | Some(ContextInclusion::None))
        || params.temperature.is_some()
        || params.stop_sequences.is_some()
        || params.metadata.is_some()
        || params.meta.is_some()
        || params.task.is_some()
        || params.tools.is_some()
        || params.tool_choice.is_some()
        || params.messages.len() != 1
        || params.messages[0].role != Role::User
        || params.messages[0].meta.is_some()
    {
        return Err(
            "native Sample supports only isolated user text and an optional system prompt".into(),
        );
    }
    if params.max_tokens > MAX_SAMPLE_TOKENS {
        return Err("native Sample max_tokens must be in 1..=16384".into());
    }
    let limit = NonZeroU32::new(params.max_tokens)
        .ok_or("native Sample max_tokens must be in 1..=16384")?;
    if !matches!(
        params.messages[0].content.blocks(),
        [SamplingContent::Text {
            annotations: None,
            meta: None,
            ..
        }]
    ) {
        return Err("native Sample requires exactly one plain text block".into());
    }
    let input_bytes = params
        .system_prompt
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(
            params.messages[0]
                .content
                .blocks()
                .iter()
                .map(|block| match block {
                    SamplingContent::Text { text, .. } => text.len(),
                    _ => 0,
                })
                .sum::<usize>(),
        );
    if input_bytes > MAX_SAMPLE_INPUT_BYTES {
        return Err("native Sample input exceeds 128 KiB".into());
    }
    Ok(limit)
}

pub(super) async fn sample(
    caller: &Caller,
    params: CreateMessageParams,
) -> Result<CreateMessageResult, String> {
    let limit = validate(&params)?;
    let cancellation = &caller.cancellation;
    // Use the existing session scheduler, not a separate provider executor.
    let _permit = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err("workflow sampling cancelled".into()),
        permit = caller.invocation.session.tool_scheduler().acquire(false) =>
            permit.map_err(|error| error.to_string())?,
    };
    let original = &caller.invocation;
    let session = &original.session;
    let turn = session
        .try_effective_turn_context(&original.turn)
        .await
        .map_err(|error| error.to_string())?;
    let router = crate::chaos::built_tools(session, &turn, &[], cancellation)
        .await
        .map_err(|error| error.to_string())?;
    if !router
        .visible_specs()
        .iter()
        .any(|spec| spec.name() == "run_workflow")
    {
        return Err("workflows are no longer enabled for this caller".into());
    }
    {
        let state = session.state.lock().await;
        if state.machine_recovery.parked() || state.machine_recovery.requested_turn().is_some() {
            return Err("machine recovery prevents workflow sampling".into());
        }
    }
    let mut base = session.get_base_instructions().await;
    base.text.push_str(
        "\nThis is an isolated, tool-free workflow text sample. \
         No tools or conversation history are available.",
    );
    if let Some(system) = params.system_prompt {
        base.text.push_str("\nRequested sample instructions:\n");
        base.text.push_str(&system);
    }
    let prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText {
                text: params
                    .messages
                    .into_iter()
                    .next()
                    .ok_or("missing sampling message")?
                    .content
                    .into_text(),
            }],
            end_turn: None,
            phase: None,
        }],
        base_instructions: base,
        ..Default::default()
    };
    if prompt.base_instructions.text.len().saturating_add(
        prompt
            .input
            .iter()
            .map(|item| match item {
                ResponseItem::Message { content, .. } => content
                    .iter()
                    .map(|content| match content {
                        ContentItem::InputText { text } => text.len(),
                        _ => 0,
                    })
                    .sum::<usize>(),
                _ => 0,
            })
            .sum::<usize>(),
    ) > MAX_SAMPLE_INPUT_BYTES
    {
        return Err("native Sample instructions and input exceed 128 KiB".into());
    }
    let mut client = session
        .services
        .model_client
        .new_text_sampling_session(limit);
    if session.services.model_client.is_clamped() {
        return Err("tool-free workflow sampling is unsupported on clamped transports".into());
    }
    let run = async {
        session.record_provider_request_started(&turn).await;
        let metadata = turn.turn_metadata_state.current_header_value();
        let stream = client
            .stream_accounted(
                &turn.sub_id,
                chaos_journald::InvocationPurpose::Sampling,
                &prompt,
                &turn.model_info,
                &turn.session_telemetry,
                turn.reasoning_effort,
                turn.reasoning_summary,
                turn.config.service_tier,
                Some(metadata.as_str()),
            )
            .await
            .map_err(|error| error.to_string())?;
        let output = output::collect(stream).await?;
        session
            .record_auxiliary_token_usage(&turn, output.usage.as_ref())
            .await;
        for limits in output.rate_limits {
            session.update_rate_limits(&turn, limits).await;
        }
        Ok(CreateMessageResult {
            role: Role::Assistant,
            content: SamplingMessageContent::Single(SamplingContent::Text {
                text: output.text,
                annotations: None,
                meta: None,
            }),
            model: output.model.unwrap_or_else(|| turn.model_info.slug.clone()),
            // The normalized provider stream does not expose a reliable stop
            // reason across all adapters; never invent endTurn/maxTokens.
            stop_reason: None,
            meta: None,
        })
    };
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err("workflow sampling cancelled".into()),
        result = run => result,
    }
}

#[cfg(test)]
mod tests;
