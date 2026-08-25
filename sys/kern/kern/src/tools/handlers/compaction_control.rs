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

fn validate_window_id(supplied: Option<&str>, live: &str) -> Result<(), String> {
    if supplied.is_some_and(|window_id| window_id != live) {
        Err(format!(
            "stale compaction window: current window_id is {live}"
        ))
    } else {
        Ok(())
    }
}

fn validate_defer_once(
    existing_control: &Control,
    reminder_claimed: bool,
    deferral_used: bool,
    active_tokens: i64,
    soft_limit: i64,
    deferral: &Deferral,
) -> Result<bool, String> {
    if matches!(existing_control, Control::CompactRequested(_)) {
        return Err("compact_now is already pending for this pressure window".to_string());
    }
    if !reminder_claimed {
        return Err(
            "defer_once is only available after the current window's compaction reflex".to_string(),
        );
    }
    let idempotent =
        matches!(existing_control, Control::Deferred(existing) if existing == deferral);
    if idempotent {
        return Ok(true);
    }
    if deferral.ceiling <= soft_limit {
        return Err(
            "the current model has no safe extension beyond its automatic compaction threshold"
                .to_string(),
        );
    }
    if active_tokens >= deferral.ceiling {
        return Err(format!(
            "the safe deferral ceiling ({} tokens) has already been reached",
            deferral.ceiling
        ));
    }
    if deferral_used {
        return Err(
            "the one-time deferral for this pressure window has already been used".to_string(),
        );
    }
    Ok(false)
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
        validate_window_id(args.window_id.as_deref(), &live_window_id)
            .map_err(FunctionCallError::RespondToModel)?;

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
                args.window_id.as_deref().ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "defer_once requires the window_id from the current compaction reflex"
                            .to_string(),
                    )
                })?;
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
                let deferral = Deferral {
                    model: turn.model_info.slug.clone(),
                    effective_context_window,
                    ceiling,
                };
                let idempotent = validate_defer_once(
                    &existing_control,
                    reminder_claimed,
                    deferral_used,
                    active_tokens,
                    soft_limit,
                    &deferral,
                )
                .map_err(FunctionCallError::RespondToModel)?;
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
            recorder.flush().await.map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to commit compaction decision to the journal: {err}"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn deferral() -> Deferral {
        Deferral {
            model: "model".to_string(),
            effective_context_window: 380_000,
            ceiling: 360_000,
        }
    }

    #[test]
    fn stale_window_ids_are_rejected() {
        assert!(validate_window_id(Some("old"), "current").is_err());
        assert!(validate_window_id(Some("current"), "current").is_ok());
        assert!(validate_window_id(None, "current").is_ok());
    }

    #[test]
    fn defer_once_requires_the_reflex() {
        assert!(
            validate_defer_once(
                &Control::Normal,
                false,
                false,
                320_000,
                350_000,
                &deferral()
            )
            .is_err()
        );
    }

    #[test]
    fn repeated_defer_once_is_idempotent_even_at_the_ceiling() {
        let deferral = deferral();
        assert_eq!(
            validate_defer_once(
                &Control::Deferred(deferral.clone()),
                true,
                true,
                deferral.ceiling,
                350_000,
                &deferral,
            ),
            Ok(true)
        );
    }

    #[test]
    fn second_distinct_deferral_is_refused() {
        assert!(
            validate_defer_once(&Control::Normal, true, true, 320_000, 350_000, &deferral())
                .is_err()
        );
    }

    #[test]
    fn defer_once_cannot_replace_pending_compaction() {
        let request = CompactRequest {
            model: "model".to_string(),
            effective_context_window: 380_000,
        };
        assert!(
            validate_defer_once(
                &Control::CompactRequested(request),
                true,
                false,
                320_000,
                350_000,
                &deferral(),
            )
            .is_err()
        );
    }

    #[test]
    fn defer_once_is_refused_at_the_ceiling() {
        let deferral = deferral();
        assert!(
            validate_defer_once(
                &Control::Normal,
                true,
                false,
                deferral.ceiling,
                350_000,
                &deferral,
            )
            .is_err()
        );
    }
}
