//! Tool call event handlers: MCP tools, web search, image generation, and
//! view-image tool calls.

use chaos_ipc::protocol::ImageGenerationBeginEvent;
use chaos_ipc::protocol::ImageGenerationEndEvent;
use chaos_ipc::protocol::McpToolCallBeginEvent;
use chaos_ipc::protocol::McpToolCallEndEvent;
use chaos_ipc::protocol::ViewImageToolCallEvent;
use chaos_ipc::protocol::WebSearchBeginEvent;
use chaos_ipc::protocol::WebSearchEndEvent;

use crate::history_cell;
use crate::history_cell::WebSearchCell;

use super::super::super::ChatWidget;

impl ChatWidget {
    // ── MCP tool call events ──────────────────────────────────────────────────

    pub(crate) fn on_mcp_tool_call_begin(&mut self, ev: McpToolCallBeginEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(|q| q.push_mcp_begin(ev), |s| s.handle_mcp_begin_now(ev2));
    }

    pub(crate) fn on_mcp_tool_call_end(&mut self, ev: McpToolCallEndEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(|q| q.push_mcp_end(ev), |s| s.handle_mcp_end_now(ev2));
    }

    // ── Web search events ─────────────────────────────────────────────────────

    pub(crate) fn on_web_search_begin(&mut self, ev: WebSearchBeginEvent) {
        self.flush_answer_stream_with_separator();
        self.flush_active_cell();
        self.active_cell = Some(Box::new(history_cell::new_active_web_search_call(
            ev.call_id,
            String::new(),
            self.config.animations,
        )));
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    pub(crate) fn on_web_search_end(&mut self, ev: WebSearchEndEvent) {
        self.flush_answer_stream_with_separator();
        let WebSearchEndEvent {
            call_id,
            query,
            action,
        } = ev;
        let mut handled = false;
        if let Some(cell) = self
            .active_cell
            .as_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<WebSearchCell>())
            && cell.call_id() == call_id
        {
            cell.update(action.clone(), query.clone());
            cell.complete();
            self.bump_active_cell_revision();
            self.flush_active_cell();
            handled = true;
        }

        if !handled {
            self.add_to_history(history_cell::new_web_search_call(call_id, query, action));
        }
        self.had_work_activity = true;
    }

    // ── Image events ──────────────────────────────────────────────────────────

    pub(crate) fn on_view_image_tool_call(&mut self, event: ViewImageToolCallEvent) {
        self.flush_answer_stream_with_separator();
        self.add_to_history(history_cell::new_view_image_tool_call(
            event.path,
            &self.config.cwd,
        ));
        self.request_redraw();
    }

    pub(crate) fn on_image_generation_begin(&mut self, _event: ImageGenerationBeginEvent) {
        self.flush_answer_stream_with_separator();
    }

    pub(crate) fn on_image_generation_end(&mut self, event: ImageGenerationEndEvent) {
        self.flush_answer_stream_with_separator();
        let saved_to = event.saved_path.as_deref().and_then(|saved_path| {
            std::path::Path::new(saved_path)
                .parent()
                .map(|parent| parent.display().to_string())
        });
        self.add_to_history(history_cell::new_image_generation_call(
            event.call_id,
            event.revised_prompt,
            saved_to,
        ));
        self.request_redraw();
    }
}
