use super::{App, PaneKind};

impl App {
    pub(super) async fn refresh_inspector(&mut self, tui: &mut crate::tui::Tui, width: u16) {
        self.tile_manager.sync_inspector(width);
        let process_id = self.current_displayed_process_id();
        let inspector = self.tile_manager.inspector.clone();
        inspector.borrow_mut().set_process(process_id);
        if self.overlay.is_some() {
            inspector.borrow_mut().pause();
            return;
        }
        if self.tile_manager.find_pane(PaneKind::Inspector).is_none() {
            return;
        }
        if let Some(id) = process_id
            && let Ok(process) = self.server.get_process(id).await
        {
            inspector.borrow_mut().tick(process, tui.frame_requester());
        }
    }
}
