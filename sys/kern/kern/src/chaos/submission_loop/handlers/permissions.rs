use crate::chaos::{Session, SessionSettingsUpdate};
use chaos_ipc::protocol::{
    ChaosErrorInfo, ErrorEvent, Event, EventMsg, PermissionGrantUpdate, PermissionUpdateScope,
};

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
                sess.state.lock().await.session_configuration = next;
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
                sess.services
                    .mcp_registry
                    .sync_permission_state(
                        chaos_sysctl::Constrained::allow_any(effective_approval),
                        crate::SandboxState {
                            vfs_policy,
                            socket_policy,
                            alcatraz_exe,
                            sandbox_cwd,
                        },
                    )
                    .await
                    .unwrap_or_else(|_| {
                        panic!("MCP registry actor stopped while updating permissions")
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
