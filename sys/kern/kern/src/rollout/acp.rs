//! Conversation-only ACP restoration, checked after native writer ownership.

use std::io;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::{EventMsg, InitialHistory, ResumedHistory, RolloutItem, SessionSource};

use super::recorder::{RolloutRecorder, journal_client_for_mounted_backend};
use crate::config::Config;

pub(crate) async fn restore(
    config: &Config,
    id: ProcessId,
    expected: &InitialHistory,
) -> io::Result<InitialHistory> {
    let client = journal_client_for_mounted_backend()
        .await
        .map_err(io::Error::other)?;
    let record = client
        .get_process(id)
        .await
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::other("ACP resume metadata is missing"))?;
    if record.source != SessionSource::Acp
        || record.archived_at.is_some()
        || record.model_provider != config.model_provider_id
        || tokio::fs::canonicalize(record.cwd).await?
            != tokio::fs::canonicalize(&config.cwd).await?
    {
        return Err(io::Error::other("ACP resume provenance does not match"));
    }
    let history: Vec<_> = RolloutRecorder::get_journal_for_process(id)
        .await?
        .items
        .into_iter()
        .map(|entry| entry.item)
        .collect();
    // A preceding reader cannot authorize a later journal head. This also keeps
    // the public replay snapshot identical to the context admitted under lease.
    if serde_json::to_vec(&history)? != serde_json::to_vec(&expected.get_rollout_items())? {
        return Err(io::Error::other(
            "ACP journal changed before writer ownership; reconnect to reload",
        ));
    }
    validate(id, &history)?;
    Ok(InitialHistory::Resumed(ResumedHistory {
        conversation_id: id,
        history: history
            .into_iter()
            .filter(|item| {
                !matches!(
                    item,
                    RolloutItem::BackgroundTask(_) | RolloutItem::GoalCheckpoint(_)
                )
            })
            .collect(),
    }))
}

fn validate(id: ProcessId, history: &[RolloutItem]) -> io::Result<()> {
    let mut active = None;
    let mut confirmed = false;
    let mut metadata = false;
    for item in history {
        match item {
            RolloutItem::SessionMeta(line) => {
                metadata |= line.meta.id == id && line.meta.source == SessionSource::Acp;
            }
            RolloutItem::EventMsg(EventMsg::SessionModeChanged(event)) => {
                confirmed |= event.session_id == id;
            }
            RolloutItem::EventMsg(EventMsg::TurnStarted(event)) => {
                active = Some(event.turn_id.as_str());
            }
            RolloutItem::EventMsg(EventMsg::TurnComplete(event))
                if active == Some(event.turn_id.as_str()) =>
            {
                active = None;
            }
            RolloutItem::EventMsg(EventMsg::TurnAborted(event))
                if event.turn_id.is_none() || event.turn_id.as_deref() == active =>
            {
                active = None;
            }
            _ => {}
        }
    }
    if !metadata || !confirmed || active.is_some() {
        return Err(io::Error::other(
            "ACP journal lacks a confirmed idle conversation checkpoint",
        ));
    }
    Ok(())
}
