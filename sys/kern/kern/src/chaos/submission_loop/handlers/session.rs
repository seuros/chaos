//! Owner input, session settings, and process lifecycle operations.
use crate::chaos::{Session, SessionSettingsUpdate, SteerInputError};
use crate::config::Config;
use crate::context_manager::is_user_turn_boundary;
use crate::review_prompts::resolve_review_request;
use chaos_ipc::background_tasks::WakePolicy;
use chaos_ipc::config_types::{CollaborationMode, ModeKind, Settings};
use chaos_ipc::protocol::{
    ChaosErrorInfo, ErrorEvent, Event, EventMsg, Op, ReviewRequest, TurnAbortReason,
};
use std::sync::Arc;
use tracing::{info, warn};

pub async fn reload_user_config(sess: &Arc<Session>) {
    sess.reload_user_config_layer().await;
    sess.services.model_client.reset_auth_breaker();
}

pub async fn interrupt(sess: &Arc<Session>) {
    sess.interrupt_task().await;
}

pub async fn clean_background_terminals(sess: &Arc<Session>) {
    sess.close_unified_exec_processes().await;
}

pub async fn override_turn_context(sess: &Session, sub_id: String, updates: SessionSettingsUpdate) {
    if let Err(err) = sess.update_settings(updates).await {
        sess.send_event_raw(Event {
            id: sub_id,
            msg: EventMsg::Error(ErrorEvent {
                message: err.to_string(),
                chaos_error_info: Some(ChaosErrorInfo::BadRequest),
            }),
        })
        .await;
    }
}

pub async fn set_dynamic_parent_effort(sess: &Session, sub_id: String, enabled: bool) {
    sess.set_dynamic_parent_effort(enabled).await;
    sess.send_event_raw(Event {
        id: sub_id,
        msg: EventMsg::BackgroundEvent(chaos_ipc::protocol::BackgroundEventEvent {
            message: format!(
                "Dynamic parent effort {}. Changes made by the model apply to subsequent turns.",
                if enabled { "enabled" } else { "disabled" }
            ),
        }),
    })
    .await;
}

pub async fn user_input_or_turn(sess: &Arc<Session>, sub_id: String, op: Op) {
    let (items, updates) = match op {
        Op::UserTurn {
            cwd,
            approval_policy,
            sandbox_policy,
            model,
            effort,
            summary,
            service_tier,
            final_output_json_schema,
            items,
            collaboration_mode,
            personality,
        } => {
            let collaboration_mode = collaboration_mode.or_else(|| {
                Some(CollaborationMode {
                    mode: ModeKind::Default,
                    settings: Settings {
                        model: model.clone(),
                        reasoning_effort: effort,
                        developer_instructions: None,
                    },
                })
            });
            (
                items,
                SessionSettingsUpdate {
                    cwd: Some(cwd),
                    approval_policy: Some(approval_policy),
                    approvals_reviewer: None,
                    sandbox_policy: Some(sandbox_policy),
                    collaboration_mode,
                    reasoning_summary: summary,
                    service_tier,
                    final_output_json_schema: Some(final_output_json_schema),
                    personality,
                    app_server_client_name: None,
                },
            )
        }
        Op::UserInput {
            items,
            final_output_json_schema,
        } => (
            items,
            SessionSettingsUpdate {
                final_output_json_schema: Some(final_output_json_schema),
                ..Default::default()
            },
        ),
        _ => unreachable!(),
    };
    let Ok(current_context) = sess.new_turn_with_sub_id(sub_id, updates).await else {
        return;
    };
    current_context.session_telemetry.user_prompt(&items);
    sess.suspend_completion_wakes(WakePolicy::Enabled).await;
    if let Err(SteerInputError::NoActiveTurn(items)) = sess.steer_input(items, None).await {
        sess.spawn_task(
            Arc::clone(&current_context),
            items,
            crate::tasks::RegularTask::Owner,
        )
        .await;
    }
}

pub async fn shutdown(sess: &Arc<Session>, sub_id: String) -> bool {
    if !sess
        .completions
        .releasing
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        sess.suspend_completion_wakes(WakePolicy::Closed).await;
    }
    sess.services.internal_task_store.observer_cancel.cancel();
    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;
    sess.services
        .unified_exec_manager
        .terminate_all_processes()
        .await;
    if let Err(error) = sess.services.mcp_refresh.shutdown().await {
        warn!(%error, "failed to stop MCP refresh actor during session shutdown");
    }
    if let Err(error) = sess.services.mcp_registry.shutdown().await {
        warn!(%error, "failed to shut down MCP registry during session shutdown");
    }
    info!("Shutting down Chaos instance");
    let history = sess.clone_history().await;
    let turn_count = history
        .raw_items()
        .iter()
        .filter(|item| is_user_turn_boundary(item))
        .count();
    sess.services.session_telemetry.counter(
        "chaos.conversation.turn.count",
        i64::try_from(turn_count).unwrap_or(0),
        &[],
    );
    let recorder_opt = sess.services.rollout.lock().await.take();
    if let Some(rec) = recorder_opt
        && let Err(e) = rec.shutdown().await
    {
        warn!("failed to shutdown rollout recorder: {e}");
        sess.send_event_raw(Event {
            id: sub_id.clone(),
            msg: EventMsg::Error(ErrorEvent {
                message: "Failed to shutdown rollout recorder".to_string(),
                chaos_error_info: Some(ChaosErrorInfo::Other),
            }),
        })
        .await;
    }
    sess.send_event_raw(Event {
        id: sub_id,
        msg: EventMsg::ShutdownComplete,
    })
    .await;
    true
}

pub async fn review(
    sess: &Arc<Session>,
    config: &Arc<Config>,
    sub_id: String,
    review_request: ReviewRequest,
) {
    let turn_context = sess.new_default_turn_with_sub_id(sub_id.clone()).await;
    match resolve_review_request(review_request, turn_context.cwd.as_path()) {
        Ok(resolved) => {
            super::super::spawn_review_thread(
                Arc::clone(sess),
                Arc::clone(config),
                turn_context.clone(),
                sub_id,
                resolved,
            )
            .await;
        }
        Err(err) => {
            sess.send_event(
                &turn_context,
                EventMsg::Error(ErrorEvent {
                    message: err.to_string(),
                    chaos_error_info: Some(ChaosErrorInfo::Other),
                }),
            )
            .await;
        }
    }
}
