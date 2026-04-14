use std::sync::Arc;

use chaos_ipc::protocol::ChaosErrorInfo;
use chaos_ipc::protocol::ErrorEvent;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::InitialHistory;
use chaos_ipc::protocol::ProcessNameUpdatedEvent;
use chaos_ipc::protocol::ProcessRolledBackEvent;
use chaos_ipc::protocol::ResumedHistory;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::user_input::UserInput;
use tracing::warn;

use crate::chaos::Session;
use crate::config::Config;
use crate::rollout::RolloutRecorder;
use crate::rollout::process_names;
use crate::tasks::CompactTask;
use crate::tasks::UndoTask;
use crate::tasks::UserShellCommandMode;
use crate::tasks::UserShellCommandTask;
use crate::tasks::execute_user_shell_command;

pub async fn undo(sess: &Arc<Session>, sub_id: String) {
    let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;
    sess.spawn_task(turn_context, Vec::new(), UndoTask::new())
        .await;
}

pub async fn compact(sess: &Arc<Session>, sub_id: String) {
    let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;

    sess.spawn_task(
        Arc::clone(&turn_context),
        vec![UserInput::Text {
            text: turn_context.compact_prompt().to_string(),
            // Compaction prompt is synthesized; no UI element ranges to preserve.
            text_elements: Vec::new(),
        }],
        CompactTask,
    )
    .await;
}

pub async fn process_rollback(sess: &Arc<Session>, sub_id: String, num_turns: u32) {
    if num_turns == 0 {
        sess.send_event_raw(Event {
            id: sub_id,
            msg: EventMsg::Error(ErrorEvent {
                message: "num_turns must be >= 1".to_string(),
                chaos_error_info: Some(ChaosErrorInfo::ProcessRollbackFailed),
            }),
        })
        .await;
        return;
    }

    let has_active_turn = { sess.active_turn.lock().await.is_some() };
    if has_active_turn {
        sess.send_event_raw(Event {
            id: sub_id,
            msg: EventMsg::Error(ErrorEvent {
                message: "Cannot rollback while a turn is in progress.".to_string(),
                chaos_error_info: Some(ChaosErrorInfo::ProcessRollbackFailed),
            }),
        })
        .await;
        return;
    }

    let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;
    let recorder = {
        let guard = sess.services.rollout.lock().await;
        guard.clone()
    };
    let Some(recorder) = recorder else {
        sess.send_event_raw(Event {
            id: turn_context.sub_id.clone(),
            msg: EventMsg::Error(ErrorEvent {
                message: "thread rollback requires persisted session history".to_string(),
                chaos_error_info: Some(ChaosErrorInfo::ProcessRollbackFailed),
            }),
        })
        .await;
        return;
    };
    if let Err(err) = recorder.flush().await {
        sess.send_event_raw(Event {
            id: turn_context.sub_id.clone(),
            msg: EventMsg::Error(ErrorEvent {
                message: format!(
                    "failed to flush persisted session history for rollback replay: {err}"
                ),
                chaos_error_info: Some(ChaosErrorInfo::ProcessRollbackFailed),
            }),
        })
        .await;
        return;
    }

    let initial_history =
        match RolloutRecorder::get_rollout_history_for_process(sess.conversation_id).await {
            Ok(history) => history,
            Err(err) => {
                let live_rollout_items = recorder.snapshot_rollout_items();
                if live_rollout_items.is_empty() {
                    sess.send_event_raw(Event {
                    id: turn_context.sub_id.clone(),
                    msg: EventMsg::Error(ErrorEvent {
                        message: format!(
                            "failed to load persisted session history for rollback replay: {err}"
                        ),
                        chaos_error_info: Some(ChaosErrorInfo::ProcessRollbackFailed),
                    }),
                })
                .await;
                    return;
                }
                InitialHistory::Resumed(ResumedHistory {
                    conversation_id: sess.conversation_id,
                    history: live_rollout_items,
                })
            }
        };

    let rollback_event = ProcessRolledBackEvent { num_turns };
    let rollback_msg = EventMsg::ProcessRolledBack(rollback_event.clone());
    let replay_items = initial_history
        .get_rollout_items()
        .into_iter()
        .chain(std::iter::once(RolloutItem::EventMsg(rollback_msg.clone())))
        .collect::<Vec<_>>();
    sess.persist_rollout_items(&[RolloutItem::EventMsg(rollback_msg.clone())])
        .await;
    sess.flush_rollout().await;
    sess.apply_rollout_reconstruction(turn_context.as_ref(), replay_items.as_slice())
        .await;
    sess.recompute_token_usage(turn_context.as_ref()).await;

    sess.deliver_event_raw(Event {
        id: turn_context.sub_id.clone(),
        msg: rollback_msg,
    })
    .await;
}

pub async fn run_user_shell_command(sess: &Arc<Session>, sub_id: String, command: String) {
    if let Some((turn_context, cancellation_token)) =
        sess.active_turn_context_and_cancellation_token().await
    {
        let session = Arc::clone(sess);
        tokio::spawn(async move {
            execute_user_shell_command(
                session,
                turn_context,
                command,
                cancellation_token,
                UserShellCommandMode::ActiveTurnAuxiliary,
            )
            .await;
        });
        return;
    }

    let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;
    sess.spawn_task(
        Arc::clone(&turn_context),
        Vec::new(),
        UserShellCommandTask::new(command),
    )
    .await;
}

/// Persists the explicit process name in SQLite, updates in-memory state, and emits
/// a `ProcessNameUpdated` event on success.
/// It then updates `SessionConfiguration::process_name`.
/// Returns an error event if the name is empty or session persistence is disabled.
pub async fn set_process_name(sess: &Arc<Session>, sub_id: String, name: String) {
    let Some(name) = crate::util::normalize_process_name(&name) else {
        let event = Event {
            id: sub_id,
            msg: EventMsg::Error(ErrorEvent {
                message: "Process name cannot be empty.".to_string(),
                chaos_error_info: Some(ChaosErrorInfo::BadRequest),
            }),
        };
        sess.send_event_raw(event).await;
        return;
    };

    let persistence_enabled = {
        let rollout = sess.services.rollout.lock().await;
        rollout.is_some()
    };
    if !persistence_enabled {
        let event = Event {
            id: sub_id,
            msg: EventMsg::Error(ErrorEvent {
                message: "Session persistence is disabled; cannot rename process.".to_string(),
                chaos_error_info: Some(ChaosErrorInfo::Other),
            }),
        };
        sess.send_event_raw(event).await;
        return;
    };

    let chaos_home = sess.chaos_home().await;
    if let Err(e) =
        process_names::append_process_name(&chaos_home, sess.conversation_id, &name).await
    {
        let event = Event {
            id: sub_id,
            msg: EventMsg::Error(ErrorEvent {
                message: format!("Failed to set process name: {e}"),
                chaos_error_info: Some(ChaosErrorInfo::Other),
            }),
        };
        sess.send_event_raw(event).await;
        return;
    }

    {
        let mut state = sess.state.lock().await;
        state.session_configuration.process_name = Some(name.clone());
    }

    sess.send_event_raw(Event {
        id: sub_id,
        msg: EventMsg::ProcessNameUpdated(ProcessNameUpdatedEvent {
            process_id: sess.conversation_id,
            process_name: Some(name),
        }),
    })
    .await;
}

pub async fn add_to_history(sess: &Arc<Session>, config: &Arc<Config>, text: String) {
    let id = sess.conversation_id;
    let config = Arc::clone(config);
    let runtime_db = sess.services.runtime_db.clone();
    tokio::spawn(async move {
        if let Err(e) =
            crate::message_history::append_entry(&text, &id, runtime_db.as_ref(), &config).await
        {
            warn!("failed to append to message history: {e}");
        }
    });
}

pub async fn get_history_entry_request(
    sess: &Arc<Session>,
    config: &Arc<Config>,
    sub_id: String,
    offset: usize,
    log_id: u64,
) {
    let sess_clone = Arc::clone(sess);
    let runtime_db = sess.services.runtime_db.clone();
    let _config = Arc::clone(config);

    tokio::spawn(async move {
        let entry_opt = crate::message_history::lookup(log_id, offset, runtime_db.as_ref()).await;

        let event = Event {
            id: sub_id,
            msg: EventMsg::GetHistoryEntryResponse(crate::protocol::GetHistoryEntryResponseEvent {
                offset,
                log_id,
                entry: entry_opt,
            }),
        };

        sess_clone.send_event_raw(event).await;
    });
}
