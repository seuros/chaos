use std::sync::Arc;

use chaos_ipc::config_types::CollaborationMode;
use chaos_ipc::config_types::ModeKind;
use chaos_ipc::config_types::Settings;
use chaos_ipc::protocol::ChaosErrorInfo;
use chaos_ipc::protocol::ErrorEvent;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::McpServerRefreshConfig;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::ReviewRequest;
use chaos_ipc::protocol::TurnAbortReason;
use tracing::info;

use crate::chaos::Session;
use crate::chaos::SessionSettingsUpdate;
use crate::chaos::SteerInputError;
use crate::config::Config;
use crate::context_manager::is_user_turn_boundary;
use crate::review_prompts::resolve_review_request;

pub async fn refresh_mcp_servers(sess: &Arc<Session>, refresh_config: McpServerRefreshConfig) {
    let mut guard = sess.pending_mcp_server_refresh_config.lock().await;
    *guard = Some(refresh_config);
}

pub async fn reload_user_config(sess: &Arc<Session>) {
    sess.reload_user_config_layer().await;
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
                chaos_error_info: Some(chaos_ipc::protocol::ChaosErrorInfo::BadRequest),
            }),
        })
        .await;
    }
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
                        minion_instructions: None,
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
        // new_turn_with_sub_id already emits the error event.
        return;
    };
    current_context.session_telemetry.user_prompt(&items);

    // Attempt to inject input into current task.
    if let Err(SteerInputError::NoActiveTurn(items)) =
        sess.steer_input(items, /*expected_turn_id*/ None).await
    {
        sess.refresh_mcp_servers_if_requested(&current_context)
            .await;
        sess.spawn_task(
            Arc::clone(&current_context),
            items,
            crate::tasks::RegularTask,
        )
        .await;
    }
}

pub async fn shutdown(sess: &Arc<Session>, sub_id: String) -> bool {
    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;
    sess.services
        .unified_exec_manager
        .terminate_all_processes()
        .await;
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

    // Gracefully flush and shutdown the session history recorder on session end.
    let recorder_opt = {
        let mut guard = sess.services.rollout.lock().await;
        guard.take()
    };
    if let Some(rec) = recorder_opt
        && let Err(e) = rec.shutdown().await
    {
        tracing::warn!("failed to shutdown rollout recorder: {e}");
        let event = Event {
            id: sub_id.clone(),
            msg: EventMsg::Error(ErrorEvent {
                message: "Failed to shutdown rollout recorder".to_string(),
                chaos_error_info: Some(ChaosErrorInfo::Other),
            }),
        };
        sess.send_event_raw(event).await;
    }

    let event = Event {
        id: sub_id,
        msg: EventMsg::ShutdownComplete,
    };
    sess.send_event_raw(event).await;
    true
}

pub async fn review(
    sess: &Arc<Session>,
    config: &Arc<Config>,
    sub_id: String,
    review_request: ReviewRequest,
) {
    let turn_context = sess.new_default_turn_with_sub_id(sub_id.clone()).await;
    sess.refresh_mcp_servers_if_requested(&turn_context).await;
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
            let event = Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: err.to_string(),
                    chaos_error_info: Some(ChaosErrorInfo::Other),
                }),
            };
            sess.send_event(&turn_context, event.msg).await;
        }
    }
}
