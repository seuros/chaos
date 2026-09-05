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
use chaos_ipc::protocol::PermissionGrantUpdate;
use chaos_ipc::protocol::PermissionUpdateScope;
use chaos_ipc::protocol::ReviewRequest;
use chaos_ipc::protocol::TurnAbortReason;
use tracing::info;
use tracing::warn;

use crate::chaos::Session;
use crate::chaos::SessionSettingsUpdate;
use crate::chaos::SteerInputError;
use crate::config::Config;
use crate::context_manager::is_user_turn_boundary;
use crate::review_prompts::resolve_review_request;

pub async fn refresh_mcp_servers(
    sess: &Arc<Session>,
    sub_id: String,
    refresh_config: McpServerRefreshConfig,
) {
    let weak_session = Arc::downgrade(sess);
    let error_sub_id = sub_id.clone();
    if let Err(err) = sess
        .services
        .mcp_refresh
        .enqueue(async move {
            if let Some(sess) = weak_session.upgrade() {
                refresh_mcp_servers_now(&sess, sub_id, refresh_config).await;
            }
        })
        .await
    {
        sess.send_event_raw(Event {
            id: error_sub_id,
            msg: EventMsg::Error(ErrorEvent {
                message: format!("MCP refresh actor is unavailable: {err}"),
                chaos_error_info: Some(ChaosErrorInfo::Other),
            }),
        })
        .await;
    }
}

async fn refresh_mcp_servers_now(
    sess: &Arc<Session>,
    sub_id: String,
    refresh_config: McpServerRefreshConfig,
) {
    let (turn_context, temporary_turn) =
        match sess.active_turn_context_and_cancellation_token().await {
            Some((turn, _)) => (turn, false),
            None => (
                sess.new_default_turn_with_sub_id(sub_id.clone()).await,
                true,
            ),
        };
    let result = sess
        .refresh_mcp_servers_now(&turn_context, refresh_config)
        .await;
    if temporary_turn {
        sess.permission_actor
            .remove_turn(turn_context.sub_id.clone())
            .await
            .unwrap_or_else(|_| {
                panic!("permission actor stopped while removing the refresh turn");
            });
    }
    match result {
        Ok(event) => {
            sess.send_event_raw(Event {
                id: sub_id,
                msg: EventMsg::McpServersRefreshed(event),
            })
            .await;
        }
        Err(err) => {
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
}

pub async fn reload_user_config(sess: &Arc<Session>) {
    sess.reload_user_config_layer().await;
    // Credentials may have just changed (e.g. an account was connected), so
    // clear any open auth circuit so the next turn probes the fresh state.
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
                chaos_error_info: Some(chaos_ipc::protocol::ChaosErrorInfo::BadRequest),
            }),
        })
        .await;
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn update_permissions(
    sess: &Session,
    sub_id: String,
    scope: PermissionUpdateScope,
    expected_revision: Option<u64>,
    approval_policy: Option<chaos_ipc::protocol::ApprovalPolicy>,
    sandbox_policy: Option<chaos_ipc::protocol::SandboxPolicy>,
    grants: PermissionGrantUpdate,
) {
    let permission_effect_changed = approval_policy.is_some()
        || sandbox_policy.is_some()
        || !matches!(&grants, PermissionGrantUpdate::Unchanged);
    let (cwd, next_session_configuration, target_turn) = match &scope {
        PermissionUpdateScope::Session => {
            let state = sess.state.lock().await;
            let next = state
                .session_configuration
                .clone()
                .apply(&SessionSettingsUpdate {
                    approval_policy,
                    sandbox_policy: sandbox_policy.clone(),
                    ..Default::default()
                });
            match next {
                Ok(next) => (state.session_configuration.cwd.clone(), Some(next), None),
                Err(err) => {
                    drop(state);
                    sess.send_event_raw(Event {
                        id: sub_id,
                        msg: EventMsg::Error(ErrorEvent {
                            message: err.to_string(),
                            chaos_error_info: Some(ChaosErrorInfo::BadRequest),
                        }),
                    })
                    .await;
                    return;
                }
            }
        }
        PermissionUpdateScope::ActiveTurn { turn_id } => {
            let Some(turn) = sess.turn_context_for_sub_id(turn_id).await else {
                sess.send_event_raw(Event {
                    id: sub_id,
                    msg: EventMsg::Error(ErrorEvent {
                        message: format!("active turn `{turn_id}` was not found"),
                        chaos_error_info: Some(ChaosErrorInfo::BadRequest),
                    }),
                })
                .await;
                return;
            };
            if let Some(requested_approval_policy) = approval_policy {
                let mut constrained = turn.approval_policy.clone();
                if let Err(err) = constrained.set(requested_approval_policy) {
                    sess.send_event_raw(Event {
                        id: sub_id,
                        msg: EventMsg::Error(ErrorEvent {
                            message: err.to_string(),
                            chaos_error_info: Some(ChaosErrorInfo::BadRequest),
                        }),
                    })
                    .await;
                    return;
                }
            }
            (turn.cwd.clone(), None, Some(turn))
        }
    };
    let mcp_turn = match &scope {
        PermissionUpdateScope::Session => sess
            .active_turn_context_and_cancellation_token()
            .await
            .map(|(turn, _)| turn),
        PermissionUpdateScope::ActiveTurn { .. } => target_turn,
    };
    let fallback_mcp_runtime = next_session_configuration.as_ref().map(|next| {
        let config = Session::build_per_turn_config(next);
        (next.cwd.clone(), config.alcatraz_exe)
    });

    match sess
        .permission_actor
        .update(
            scope,
            expected_revision,
            approval_policy,
            sandbox_policy,
            grants,
            cwd,
        )
        .await
    {
        Ok(updated) => {
            if let Some(next) = next_session_configuration {
                let mut state = sess.state.lock().await;
                state.session_configuration = next;
            }
            let mcp_permissions = match mcp_turn.as_ref() {
                Some(turn) => Some(sess.permission_snapshot(turn).await),
                None => None,
            };
            let effective_approval = mcp_permissions
                .as_ref()
                .map_or(updated.approval_policy, |snapshot| snapshot.approval_policy);
            if permission_effect_changed {
                let (vfs_policy, socket_policy) = mcp_permissions.as_ref().map_or_else(
                    || {
                        (
                            crate::sandboxing::effective_vfs_policy(
                                &updated.vfs_policy,
                                updated.granted_permissions.as_ref(),
                            ),
                            crate::sandboxing::effective_socket_policy(
                                updated.socket_policy,
                                updated.granted_permissions.as_ref(),
                            ),
                        )
                    },
                    |snapshot| {
                        (
                            snapshot.effective_vfs_policy(),
                            snapshot.effective_socket_policy(),
                        )
                    },
                );
                let runtime = mcp_turn
                    .as_ref()
                    .map(|turn| (turn.cwd.clone(), turn.alcatraz_exe.clone()))
                    .or(fallback_mcp_runtime);
                let Some((sandbox_cwd, alcatraz_exe)) = runtime else {
                    panic!("permission update always has an MCP runtime context");
                };
                let sandbox_state = crate::SandboxState {
                    vfs_policy,
                    socket_policy,
                    alcatraz_exe,
                    sandbox_cwd,
                };
                sess.services
                    .mcp_registry
                    .sync_permission_state(
                        chaos_sysctl::Constrained::allow_any(effective_approval),
                        sandbox_state,
                    )
                    .await
                    .unwrap_or_else(|_| {
                        panic!("MCP registry actor stopped while updating permissions");
                    });
            }
            sess.send_event_raw(Event {
                id: sub_id,
                msg: EventMsg::PermissionsUpdated(updated),
            })
            .await;
        }
        Err(err) => {
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
    if let Err(error) = sess.services.mcp_refresh.shutdown().await {
        warn!(
            %error,
            "failed to stop MCP refresh actor during session shutdown"
        );
    }
    if let Err(error) = sess.services.mcp_registry.shutdown().await {
        warn!(
            %error,
            "failed to shut down MCP registry during session shutdown"
        );
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
