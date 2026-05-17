//! Protocol response handlers: all_tools, mcp_tools, custom_prompts.
use super::super::*;

impl ChatWidget {
    pub(crate) fn on_all_tools_response(&mut self, ev: chaos_ipc::protocol::AllToolsResponseEvent) {
        // Forward to the app layer where the TileManager can open/populate the tools pane.
        self.app_event_tx.send(AppEvent::AllToolsReceived(ev));
    }

    pub(crate) fn on_list_mcp_tools(&mut self, ev: McpListToolsResponseEvent) {
        self.add_to_history(history_cell::new_mcp_tools_output(
            &self.config,
            ev.tools,
            ev.resources,
            ev.resource_templates,
            &ev.auth_statuses,
        ));
    }

    pub(crate) fn on_list_custom_prompts(&mut self, ev: ListCustomPromptsResponseEvent) {
        let len = ev.custom_prompts.len();
        debug!("received {len} custom prompts");
        // Forward to bottom pane so the slash popup can show them now.
        self.bottom_pane.set_custom_prompts(ev.custom_prompts);
    }
}
