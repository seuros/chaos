//! Miscellaneous state-mutation event handlers: undo, review mode, plan
//! updates, diffs, shutdown, user messages, collab events, and replay helpers.

use chaos_ipc::protocol::DeprecationNoticeEvent;
use chaos_ipc::protocol::ExitedReviewModeEvent;
use chaos_ipc::protocol::ReviewRequest;
use chaos_ipc::protocol::UndoCompletedEvent;
use chaos_ipc::protocol::UndoStartedEvent;
use chaos_ipc::protocol::UserMessageEvent;
use chaos_ipc::user_input::TextElement;

use crate::app_event::AppEvent;
use crate::history_cell;
use crate::history_cell::AgentMessageCell;
use crate::history_cell::PlainHistoryCell;
use crate::markdown::append_markdown;

use super::super::super::ChatWidget;
use super::super::super::core::RenderedUserMessageEvent;
use super::super::super::core::hook_event_label;

impl ChatWidget {
    // ── Misc small events ─────────────────────────────────────────────────────

    pub(crate) fn on_collab_event(&mut self, cell: PlainHistoryCell) {
        self.flush_answer_stream_with_separator();
        self.add_to_history(cell);
        self.request_redraw();
    }

    pub(crate) fn on_get_history_entry_response(
        &mut self,
        event: chaos_ipc::protocol::GetHistoryEntryResponseEvent,
    ) {
        let chaos_ipc::protocol::GetHistoryEntryResponseEvent {
            offset,
            log_id,
            entry,
        } = event;
        self.bottom_pane
            .on_history_entry_response(log_id, offset, entry.map(|e| e.text));
    }

    pub(crate) fn on_shutdown_complete(&mut self) {
        self.request_immediate_exit();
    }

    pub(crate) fn on_turn_diff(&mut self, unified_diff: String) {
        tracing::debug!("TurnDiffEvent: {unified_diff}");
        self.refresh_status_line();
    }

    pub(crate) fn on_deprecation_notice(&mut self, event: DeprecationNoticeEvent) {
        let DeprecationNoticeEvent { summary, details } = event;
        self.add_to_history(history_cell::new_deprecation_notice(summary, details));
        self.request_redraw();
    }

    pub(crate) fn on_background_event(&mut self, message: String) {
        tracing::debug!("BackgroundEvent: {message}");
        self.bottom_pane.ensure_status_indicator();
        self.bottom_pane
            .set_interrupt_hint_visible(/*visible*/ true);
        self.set_status_header(message);
    }

    pub(crate) fn on_hook_started(&mut self, event: chaos_ipc::protocol::HookStartedEvent) {
        let label = hook_event_label(event.run.event_name);
        let mut message = format!("Running {label} hook");
        if let Some(status_message) = event.run.status_message
            && !status_message.is_empty()
        {
            message.push_str(": ");
            message.push_str(&status_message);
        }
        self.add_to_history(history_cell::new_info_event(message, /*hint*/ None));
        self.request_redraw();
    }

    pub(crate) fn on_hook_completed(&mut self, event: chaos_ipc::protocol::HookCompletedEvent) {
        let status = format!("{:?}", event.run.status).to_lowercase();
        let header = format!("{} hook ({status})", hook_event_label(event.run.event_name));
        let mut lines: Vec<ratatui::text::Line<'static>> = vec![header.into()];
        for entry in event.run.entries {
            let prefix = match entry.kind {
                chaos_ipc::protocol::HookOutputEntryKind::Warning => "warning: ",
                chaos_ipc::protocol::HookOutputEntryKind::Stop => "stop: ",
                chaos_ipc::protocol::HookOutputEntryKind::Feedback => "feedback: ",
                chaos_ipc::protocol::HookOutputEntryKind::Context => "hook context: ",
                chaos_ipc::protocol::HookOutputEntryKind::Error => "error: ",
            };
            lines.push(format!("  {prefix}{}", entry.text).into());
        }
        self.add_to_history(PlainHistoryCell::new(lines));
        self.request_redraw();
    }

    pub(crate) fn on_undo_started(&mut self, event: UndoStartedEvent) {
        self.bottom_pane.ensure_status_indicator();
        self.bottom_pane
            .set_interrupt_hint_visible(/*visible*/ false);
        let message = event
            .message
            .unwrap_or_else(|| "Undo in progress...".to_string());
        self.set_status_header(message);
    }

    pub(crate) fn on_undo_completed(&mut self, event: UndoCompletedEvent) {
        let UndoCompletedEvent { success, message } = event;
        self.bottom_pane.hide_status_indicator();
        let message = message.unwrap_or_else(|| {
            if success {
                "Undo completed successfully.".to_string()
            } else {
                "Undo failed.".to_string()
            }
        });
        if success {
            self.add_info_message(message, /*hint*/ None);
        } else {
            self.add_error_message(message);
        }
    }

    pub(crate) fn on_stream_error(&mut self, message: String, additional_details: Option<String>) {
        if self.retry_status_header.is_none() {
            self.retry_status_header = Some(self.current_status.header.clone());
        }
        self.bottom_pane.ensure_status_indicator();
        self.set_status(
            message,
            additional_details,
            crate::status_indicator_widget::StatusDetailsCapitalization::CapitalizeFirst,
            crate::status_indicator_widget::STATUS_DETAILS_DEFAULT_MAX_LINES,
        );
    }

    pub(crate) fn on_plan_update(&mut self, update: chaos_ipc::plan_tool::UpdatePlanArgs) {
        self.saw_plan_update_this_turn = true;
        self.add_to_history(history_cell::new_plan_update(update));
    }

    // ── Review mode events ────────────────────────────────────────────────────

    pub(crate) fn on_entered_review_mode(&mut self, review: ReviewRequest, from_replay: bool) {
        if self.pre_review_token_info.is_none() {
            self.pre_review_token_info = Some(self.token_info.clone());
        }
        if !from_replay && !self.bottom_pane.is_task_running() {
            self.bottom_pane.set_task_running(/*running*/ true);
        }
        self.is_review_mode = true;
        let hint = review
            .user_facing_hint
            .unwrap_or_else(|| chaos_kern::review_prompts::user_facing_hint(&review.target));
        let banner = format!(">> Code review started: {hint} <<");
        self.add_to_history(history_cell::new_review_status_line(banner));
        self.request_redraw();
    }

    pub(crate) fn on_exited_review_mode(&mut self, review: ExitedReviewModeEvent) {
        if let Some(output) = review.review_output {
            self.flush_answer_stream_with_separator();
            self.flush_interrupt_queue();
            self.flush_active_cell();

            if output.findings.is_empty() {
                let explanation = output.overall_explanation.trim().to_string();
                if explanation.is_empty() {
                    tracing::error!("Reviewer failed to output a response.");
                    self.add_to_history(history_cell::new_error_event(
                        "Reviewer failed to output a response.".to_owned(),
                    ));
                } else {
                    let mut rendered: Vec<ratatui::text::Line<'static>> = vec!["".into()];
                    append_markdown(
                        &explanation,
                        /*width*/ None,
                        Some(self.config.cwd.as_path()),
                        &mut rendered,
                    );
                    let body_cell = AgentMessageCell::new(rendered, /*is_first_line*/ false);
                    self.app_event_tx
                        .send(AppEvent::InsertHistoryCell(Box::new(body_cell)));
                }
            }
        }

        self.is_review_mode = false;
        self.restore_pre_review_token_info();
        self.add_to_history(history_cell::new_review_status_line(
            "<< Code review finished >>".to_string(),
        ));
        self.request_redraw();
    }

    // ── User message helpers ──────────────────────────────────────────────────

    pub(crate) fn rendered_user_message_event_from_parts(
        message: String,
        text_elements: Vec<TextElement>,
        local_images: Vec<std::path::PathBuf>,
        remote_image_urls: Vec<String>,
    ) -> RenderedUserMessageEvent {
        RenderedUserMessageEvent {
            message,
            remote_image_urls,
            local_images,
            text_elements,
        }
    }

    pub(crate) fn rendered_user_message_event_from_event(
        event: &UserMessageEvent,
    ) -> RenderedUserMessageEvent {
        Self::rendered_user_message_event_from_parts(
            event.message.clone(),
            event.text_elements.clone(),
            event.local_images.clone(),
            event.images.clone().unwrap_or_default(),
        )
    }

    pub(crate) fn pending_steer_compare_key_from_items(
        items: &[chaos_ipc::user_input::UserInput],
    ) -> super::super::super::core::PendingSteerCompareKey {
        let mut message = String::new();
        let mut image_count = 0;
        for item in items {
            match item {
                chaos_ipc::user_input::UserInput::Text { text, .. } => message.push_str(text),
                chaos_ipc::user_input::UserInput::Image { .. }
                | chaos_ipc::user_input::UserInput::LocalImage { .. } => image_count += 1,
                _ => {}
            }
        }
        super::super::super::core::PendingSteerCompareKey {
            message,
            image_count,
        }
    }

    pub(crate) fn pending_steer_compare_key_from_item(
        item: &chaos_ipc::items::UserMessageItem,
    ) -> super::super::super::core::PendingSteerCompareKey {
        Self::pending_steer_compare_key_from_items(&item.content)
    }

    #[cfg(test)]
    pub(crate) fn rendered_user_message_event_from_inputs(
        items: &[chaos_ipc::user_input::UserInput],
    ) -> RenderedUserMessageEvent {
        use super::super::super::core::append_text_with_rebased_elements;

        let mut message = String::new();
        let mut remote_image_urls = Vec::new();
        let mut local_images = Vec::new();
        let mut text_elements = Vec::new();

        for item in items {
            match item {
                chaos_ipc::user_input::UserInput::Text {
                    text,
                    text_elements: current_text_elements,
                } => append_text_with_rebased_elements(
                    &mut message,
                    &mut text_elements,
                    text,
                    current_text_elements.iter().map(|element| {
                        TextElement::new(
                            element.byte_range,
                            element.placeholder(text).map(str::to_string),
                        )
                    }),
                ),
                chaos_ipc::user_input::UserInput::Image { image_url } => {
                    remote_image_urls.push(image_url.clone())
                }
                chaos_ipc::user_input::UserInput::LocalImage { path } => {
                    local_images.push(path.clone())
                }
                _ => {}
            }
        }

        Self::rendered_user_message_event_from_parts(
            message,
            text_elements,
            local_images,
            remote_image_urls,
        )
    }

    pub(crate) fn on_user_message_event(&mut self, event: UserMessageEvent) {
        self.last_rendered_user_message_event =
            Some(Self::rendered_user_message_event_from_event(&event));
        let remote_image_urls = event.images.unwrap_or_default();
        if !event.message.trim().is_empty()
            || !event.text_elements.is_empty()
            || !remote_image_urls.is_empty()
        {
            self.add_to_history(history_cell::new_user_prompt(
                event.message,
                event.text_elements,
                event.local_images,
                remote_image_urls,
            ));
        }

        self.needs_final_message_separator = false;
    }
}
