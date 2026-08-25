use chaos_context::pressure::CompactRequest;
use chaos_context::pressure::Control;
use chaos_context::pressure::Deferral;
use chaos_ipc::protocol::CompactionControlAction;
use chaos_ipc::protocol::CompactionControlItem;
use chaos_ipc::protocol::RolloutItem;
use serde::Deserialize;
use serde::Serialize;

use crate::chaos::session::tokens::compaction_deferral_ceiling;
use crate::config::AgentCompactionControl;
use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

use super::extract_function_arguments;
use super::parse_arguments;

pub struct CompactionControlHandler;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    CompactNow,
    DeferOnce,
}

#[derive(Debug, Deserialize)]
struct Args {
    action: Action,
    #[serde(default)]
    window_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct Output {
    accepted: bool,
    action: &'static str,
    window_number: u64,
    window_id: String,
    active_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    deferral_ceiling: Option<i64>,
    idempotent: bool,
}

impl ToolHandler for CompactionControlHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            tool_name,
            payload,
            ..
        } = invocation;
        if turn.config.agent_compaction_control != AgentCompactionControl::Bounded {
            return Err(FunctionCallError::RespondToModel(
                "agent compaction control is disabled for this session".to_string(),
            ));
        }
        let arguments = extract_function_arguments(payload, &tool_name)?;
        let args: Args = parse_arguments(&arguments)?;
        let effective_context_window = turn.model_context_window().ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "the current model has no known effective context window".to_string(),
            )
        })?;

        let (
            window_number,
            live_window_id,
            active_tokens,
            existing_control,
            reminder_claimed,
            deferral_used,
        ) = {
            let state = session.state.lock().await;
            (
                state.pressure.window_number(),
                state.pressure.window_id().to_string(),
                state.get_total_token_usage(state.server_reasoning_included()),
                state.pressure.control().clone(),
                state.pressure.reminder_claimed(),
                state.pressure.deferral_used(),
            )
        };
        if let Some(window_id) = args.window_id.as_deref()
            && window_id != live_window_id
        {
            return Err(FunctionCallError::RespondToModel(format!(
                "stale compaction window: current window_id is {live_window_id}"
            )));
        }

        let (action, action_name, deferral_ceiling, new_control, idempotent) = match args.action {
            Action::CompactNow => {
                let request = CompactRequest {
                    model: turn.model_info.slug.clone(),
                    effective_context_window,
                };
                let idempotent = matches!(
                    &existing_control,
                    Control::CompactRequested(existing) if existing == &request
                );
                (
                    CompactionControlAction::CompactNow,
                    "compact_now",
                    None,
                    Control::CompactRequested(request),
                    idempotent,
                )
            }
            Action::DeferOnce => {
                let supplied_window_id = args.window_id.as_deref().ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "defer_once requires the window_id from the current compaction reflex"
                            .to_string(),
                    )
                })?;
                if supplied_window_id != live_window_id || !reminder_claimed {
                    return Err(FunctionCallError::RespondToModel(
                        "defer_once is only available after the current window's compaction reflex"
                            .to_string(),
                    ));
                }
                let ceiling = compaction_deferral_ceiling(&turn).ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "no safe deferral ceiling is available for the current model".to_string(),
                    )
                })?;
                let soft_limit = turn.model_info.auto_compact_token_limit().ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "the current model has no automatic compaction threshold".to_string(),
                    )
                })?;
                if ceiling <= soft_limit {
                    return Err(FunctionCallError::RespondToModel(
                        "the current model has no safe extension beyond its automatic compaction threshold"
                            .to_string(),
                    ));
                }
                if active_tokens >= ceiling {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "the safe deferral ceiling ({ceiling} tokens) has already been reached"
                    )));
                }
                let deferral = Deferral {
                    model: turn.model_info.slug.clone(),
                    effective_context_window,
                    ceiling,
                };
                let idempotent = matches!(&existing_control, Control::Deferred(existing) if existing == &deferral);
                if deferral_used && !idempotent {
                    return Err(FunctionCallError::RespondToModel(
                        "the one-time deferral for this pressure window has already been used"
                            .to_string(),
                    ));
                }
                (
                    CompactionControlAction::DeferOnce,
                    "defer_once",
                    Some(ceiling),
                    Control::Deferred(deferral),
                    idempotent,
                )
            }
        };

        if !idempotent {
            let item = RolloutItem::CompactionControl(CompactionControlItem {
                window_number,
                window_id: live_window_id.clone(),
                action,
                model: turn.model_info.slug.clone(),
                effective_context_window,
                deferral_ceiling,
                active_tokens,
            });
            let recorder = {
                let guard = session.services.rollout.lock().await;
                guard.clone()
            }
            .ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "this session has no durable journal for compaction control".to_string(),
                )
            })?;
            recorder.record_items(&[item]).await.map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to persist compaction decision: {err}"
                ))
            })?;

            let mut state = session.state.lock().await;
            if state.pressure.window_number() != window_number
                || state.pressure.window_id().to_string() != live_window_id
            {
                return Err(FunctionCallError::RespondToModel(
                    "the compaction window changed while recording the decision; inspect the new reflex before retrying"
                        .to_string(),
                ));
            }
            match new_control {
                Control::Deferred(deferral) => state.pressure.defer(deferral),
                control => state.pressure.restore_control(control),
            }
        }
        session.services.session_telemetry.counter(
            "chaos.compaction.control",
            1,
            &[
                ("action", action_name),
                ("idempotent", if idempotent { "true" } else { "false" }),
            ],
        );
        tracing::info!(
            action = action_name,
            window_number,
            %live_window_id,
            active_tokens,
            ?deferral_ceiling,
            idempotent,
            "accepted agent compaction control"
        );

        let output = serde_json::to_string_pretty(&Output {
            accepted: true,
            action: action_name,
            window_number,
            window_id: live_window_id,
            active_tokens,
            deferral_ceiling,
            idempotent,
        })
        .map_err(|err| FunctionCallError::Fatal(err.to_string()))?;
        Ok(FunctionToolOutput::from_text(output, Some(true)))
    }
}
