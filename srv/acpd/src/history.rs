use std::path::Path;

use acp_host::protocol;
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::{EventMsg, RolloutItem, SessionSource};
use chaos_kern::config::Config;
use chaos_kern::{
    ProcessSortKey, RolloutRecorder, journal_client_for_mounted_backend, parse_cursor, runtime_db,
};
use protocol::schema::v1::{ListSessionsResponse, SessionId, SessionInfo};

use crate::{internal, invalid};

pub(crate) struct Saved {
    pub id: ProcessId,
    pub items: Vec<RolloutItem>,
    pub model: String,
    pub mode: String,
}

pub(crate) async fn check_record(config: &Config, id: ProcessId) -> protocol::Result<()> {
    let journal = journal_client_for_mounted_backend()
        .await
        .map_err(|_| internal("native ACP journal is unavailable"))?;
    let record = journal
        .get_process(id)
        .await
        .map_err(|_| internal("native ACP journal metadata could not be read"))?
        .ok_or_else(|| invalid("saved ACP session does not exist"))?;
    let cwd = tokio::fs::canonicalize(&record.cwd)
        .await
        .map_err(|_| invalid("saved ACP workspace is unavailable"))?;
    if record.source != SessionSource::Acp
        || record.archived_at.is_some()
        || record.model_provider != config.model_provider_id
        || cwd != config.cwd
    {
        return Err(invalid(
            "saved session does not belong to this ACP provider and workspace",
        ));
    }
    Ok(())
}

pub(crate) async fn load(config: &Config, session_id: &SessionId) -> protocol::Result<Saved> {
    let id = ProcessId::from_string(&session_id.0)
        .map_err(|_| invalid("invalid saved ACP session id"))?;
    check_record(config, id).await?;
    let journal = RolloutRecorder::get_journal_for_process(id)
        .await
        .map_err(|_| internal("saved ACP transcript could not be loaded"))?;
    let items = journal
        .items
        .into_iter()
        .map(|entry| entry.item)
        .collect::<Vec<_>>();
    let mut active_turn = None;
    for item in &items {
        match item {
            RolloutItem::EventMsg(EventMsg::TurnStarted(event)) => {
                active_turn = Some(event.turn_id.as_str())
            }
            RolloutItem::EventMsg(EventMsg::TurnComplete(event))
                if active_turn == Some(event.turn_id.as_str()) =>
            {
                active_turn = None
            }
            RolloutItem::EventMsg(EventMsg::TurnAborted(event))
                if event.turn_id.is_none() || event.turn_id.as_deref() == active_turn =>
            {
                active_turn = None
            }
            _ => {}
        }
    }
    if active_turn.is_some() {
        return Err(invalid(
            "saved ACP turn outcome is unknown; execution will not be replayed",
        ));
    }
    let controls = items
        .iter()
        .rev()
        .find_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::SessionModeChanged(event))
                if event.session_id == id =>
            {
                Some((event.model.clone(), event.mode_id.clone()))
            }
            _ => None,
        })
        .ok_or_else(|| invalid("saved ACP session has no confirmed control checkpoint"))?;
    Ok(Saved {
        id,
        items,
        model: controls.0,
        mode: controls.1,
    })
}

pub(crate) async fn list(
    config: &Config,
    cwd: Option<&Path>,
    cursor: Option<String>,
) -> protocol::Result<ListSessionsResponse> {
    list_page(config, cwd, cursor, 50).await
}

pub(crate) async fn list_page(
    config: &Config,
    cwd: Option<&Path>,
    cursor: Option<String>,
    page_size: usize,
) -> protocol::Result<ListSessionsResponse> {
    let cursor = cursor
        .map(|value| parse_cursor(&value).ok_or_else(|| invalid("invalid ACP session cursor")))
        .transpose()?;
    let runtime = runtime_db::get_runtime_db_for(&config.storage_home, &config.model_provider_id);
    let providers = [config.model_provider_id.clone()];
    let page = runtime_db::list_processes_db(
        runtime.as_ref(),
        &config.storage_home,
        page_size.clamp(1, 50),
        cursor.as_ref(),
        ProcessSortKey::UpdatedAt,
        &[SessionSource::Acp],
        Some(&providers),
        false,
        None,
    )
    .await
    .ok_or_else(|| internal("native ACP session index is unavailable"))?;
    let mut sessions = Vec::new();
    for record in page.items {
        let Ok(canonical) = tokio::fs::canonicalize(&record.cwd).await else {
            continue;
        };
        if cwd.is_some_and(|cwd| cwd != canonical) {
            continue;
        }
        sessions.push(
            SessionInfo::new(record.id.to_string(), canonical)
                .title(record.title.chars().take(256).collect::<String>())
                .updated_at(record.updated_at.to_string()),
        );
    }
    Ok(ListSessionsResponse::new(sessions).next_cursor(
        page.next_anchor
            .map(|anchor| format!("{}|{}", anchor.ts, anchor.id)),
    ))
}
