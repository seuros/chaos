use super::{App, PaneKind};
use std::time::Duration;

impl App {
    pub(super) async fn refresh_inspector(&mut self, tui: &mut crate::tui::Tui, width: u16) {
        self.tile_manager.sync_inspector(width);
        let process_id = self.current_displayed_process_id();
        self.tile_manager.inspector.set_process(process_id);
        if self.overlay.is_some() {
            self.tile_manager.inspector.pause();
            return;
        }
        if self.tile_manager.find_pane(PaneKind::Inspector).is_none() {
            return;
        }
        let mut servers: Vec<_> = self
            .config
            .mcp_servers
            .get()
            .iter()
            .filter(|(_, config)| config.enabled)
            .map(|(name, _)| name.clone())
            .collect();
        servers.sort();
        if let Some(id) = process_id
            && let Ok(process) = self.server.get_process(id).await
        {
            self.tile_manager.inspector.tick(process, servers).await;
        }
        if self.tile_manager.inspector.needs_poll() {
            tui.frame_requester()
                .schedule_frame_in(Duration::from_secs(1));
        }
    }
}
