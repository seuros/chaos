use chaos_ipc::items::TurnItem;
use chaos_ipc::items::UserMessageItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ItemCompletedEvent;
use chaos_ipc::protocol::ItemStartedEvent;
use chaos_ipc::protocol::RawResponseItemEvent;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::user_input::UserInput;
use tracing::debug;

use crate::minions::agent_status_from_event;
use crate::parse_turn_item;
use crate::turn_timing::record_turn_ttfm_metric;

use super::Session;
use crate::chaos::TurnContext;

impl Session {
    /// Persist the event to rollout and send it to clients.
    pub(crate) async fn send_event(&self, turn_context: &TurnContext, msg: EventMsg) {
        let event = Event {
            id: turn_context.sub_id.clone(),
            msg,
        };
        self.send_event_raw(event).await;
    }

    pub(crate) async fn send_event_raw(&self, event: Event) {
        let rollout_items = vec![RolloutItem::EventMsg(event.msg.clone())];
        self.persist_rollout_items(&rollout_items).await;
        self.deliver_event_raw(event).await;
    }

    pub(crate) async fn deliver_event_raw(&self, event: Event) {
        if let Some(status) = agent_status_from_event(&event.msg) {
            self.agent_status.send_replace(status);
        }
        if let Err(e) = self.tx_event.send(event).await {
            debug!("dropping event because channel is closed: {e}");
        }
    }

    pub(crate) async fn emit_turn_item_started(&self, turn_context: &TurnContext, item: &TurnItem) {
        self.send_event(
            turn_context,
            EventMsg::ItemStarted(ItemStartedEvent {
                process_id: self.conversation_id,
                turn_id: turn_context.sub_id.clone(),
                item: item.clone(),
            }),
        )
        .await;
    }

    pub(crate) async fn emit_turn_item_completed(
        &self,
        turn_context: &TurnContext,
        item: TurnItem,
    ) {
        record_turn_ttfm_metric(turn_context, &item).await;
        self.send_event(
            turn_context,
            EventMsg::ItemCompleted(ItemCompletedEvent {
                process_id: self.conversation_id,
                turn_id: turn_context.sub_id.clone(),
                item,
            }),
        )
        .await;
    }

    pub(crate) async fn record_response_item_and_emit_turn_item(
        &self,
        turn_context: &TurnContext,
        response_item: ResponseItem,
    ) {
        self.record_conversation_items(turn_context, std::slice::from_ref(&response_item))
            .await;
        if let Some(item) = parse_turn_item(&response_item) {
            self.emit_turn_item_started(turn_context, &item).await;
            self.emit_turn_item_completed(turn_context, item).await;
        }
    }

    pub(crate) async fn record_user_prompt_and_emit_turn_item(
        &self,
        turn_context: &TurnContext,
        input: &[UserInput],
        response_item: ResponseItem,
    ) {
        self.record_conversation_items(turn_context, std::slice::from_ref(&response_item))
            .await;
        let turn_item = TurnItem::UserMessage(UserMessageItem::new(input));
        self.emit_turn_item_started(turn_context, &turn_item).await;
        self.emit_turn_item_completed(turn_context, turn_item).await;
        self.ensure_rollout_materialized().await;
    }

    pub(crate) async fn notify_background_event(
        &self,
        turn_context: &TurnContext,
        message: impl Into<String>,
    ) {
        let event = EventMsg::BackgroundEvent(crate::protocol::BackgroundEventEvent {
            message: message.into(),
        });
        self.send_event(turn_context, event).await;
    }

    pub(crate) async fn notify_stream_error(
        &self,
        turn_context: &TurnContext,
        message: impl Into<String>,
        codex_error: crate::error::ChaosErr,
    ) {
        use chaos_ipc::protocol::ChaosErrorInfo;
        let additional_details = codex_error.to_string();
        let chaos_error_info = ChaosErrorInfo::ResponseStreamDisconnected {
            http_status_code: codex_error.http_status_code_value(),
        };
        let event = EventMsg::StreamError(crate::protocol::StreamErrorEvent {
            message: message.into(),
            chaos_error_info: Some(chaos_error_info),
            additional_details: Some(additional_details),
        });
        self.send_event(turn_context, event).await;
    }

    pub(crate) async fn maybe_warn_on_server_model_mismatch(
        self: &std::sync::Arc<Self>,
        turn_context: &std::sync::Arc<TurnContext>,
        server_model: String,
    ) -> bool {
        use chaos_ipc::protocol::WarningEvent;
        use tracing::info;
        use tracing::warn;

        let requested_model = turn_context.model_info.slug.clone();
        let server_model_normalized = server_model.to_ascii_lowercase();
        let requested_model_normalized = requested_model.to_ascii_lowercase();
        if server_model_normalized == requested_model_normalized {
            info!("server reported model {server_model} (matches requested model)");
            return false;
        }

        warn!("server reported model {server_model} while requested model was {requested_model}");

        let warning_message = format!(
            "Upstream rerouted this request from {requested_model} to {server_model}. The vendor did not honor your model selection."
        );

        self.send_event(
            turn_context,
            EventMsg::ModelReroute(crate::protocol::ModelRerouteEvent {
                from_model: requested_model.clone(),
                to_model: server_model.clone(),
                reason: crate::protocol::ModelRerouteReason::VendorDeclinedSelection,
            }),
        )
        .await;

        self.send_event(
            turn_context,
            EventMsg::Warning(WarningEvent {
                message: warning_message.clone(),
            }),
        )
        .await;
        self.record_model_warning(warning_message, turn_context)
            .await;
        true
    }

    pub(super) async fn send_raw_response_items(
        &self,
        turn_context: &TurnContext,
        items: &[ResponseItem],
    ) {
        for item in items {
            self.send_event(
                turn_context,
                EventMsg::RawResponseItem(RawResponseItemEvent { item: item.clone() }),
            )
            .await;
        }
    }
}
