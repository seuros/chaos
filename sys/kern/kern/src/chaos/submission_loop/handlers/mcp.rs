//! MCP refresh operations; session/turn control is owned by `session`.
use crate::chaos::Session;
use chaos_ipc::protocol::{ChaosErrorInfo, ErrorEvent, Event, EventMsg, McpServerRefreshConfig};
use std::sync::Arc;

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
            .unwrap_or_else(|_| panic!("permission actor stopped while removing the refresh turn"));
    }
    let msg = match result {
        Ok(event) => EventMsg::McpServersRefreshed(event),
        Err(err) => EventMsg::Error(ErrorEvent {
            message: err.to_string(),
            chaos_error_info: Some(ChaosErrorInfo::BadRequest),
        }),
    };
    sess.send_event_raw(Event { id: sub_id, msg }).await;
}
