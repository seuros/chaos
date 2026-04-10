use super::{
    App, Instant, LOG_PANEL_BACKFILL_LIMIT, LogQuery, Overlay, ProcessId, StateRuntime, tui,
};
use std::sync::Arc;

impl App {
    pub(super) async fn toggle_log_panel(&mut self, tui: &mut tui::Tui) {
        // Sync the process id and backfill before snapshotting lines.
        let current_process_id = self.current_displayed_process_id();
        self.log_panel.set_process_id(current_process_id);
        self.reload_log_panel_backfill().await;

        let lines = self.log_panel.render_lines();
        let _ = tui.enter_alt_screen();
        self.overlay = Some(Overlay::new_static_with_lines(lines, "L O G S".to_string()));
        tui.frame_requester().schedule_frame();
    }

    pub(super) async fn reload_log_panel_backfill(&mut self) {
        let Some(state_db) = self.ensure_log_state_db().await else {
            return;
        };
        let Some(process_id) = self.log_panel.process_id() else {
            self.log_panel
                .replace_batch(chaos_proc::LogTailBatch::default());
            self.log_panel.schedule_next_poll(Instant::now());
            return;
        };
        match state_db
            .tail_backfill(
                &Self::log_query_for_process(process_id),
                LOG_PANEL_BACKFILL_LIMIT,
            )
            .await
        {
            Ok(batch) => {
                self.log_panel.replace_batch(batch);
                self.log_panel.schedule_next_poll(Instant::now());
            }
            Err(err) => {
                self.log_panel
                    .set_error(format!("Failed to load logs: {err}"));
            }
        }
    }

    pub(super) fn log_query_for_process(process_id: ProcessId) -> LogQuery {
        LogQuery {
            related_to_process_id: Some(process_id.to_string()),
            include_related_processless: true,
            ..Default::default()
        }
    }

    pub(super) async fn ensure_log_state_db(&mut self) -> Option<Arc<StateRuntime>> {
        if let Some(state_db) = self.log_state_db.clone() {
            return Some(state_db);
        }

        let sqlite_home = self.config.sqlite_home.clone();
        let model_provider_id = self.config.model_provider_id.clone();
        match StateRuntime::init(sqlite_home.clone(), model_provider_id).await {
            Ok(state_db) => {
                self.log_state_db_init_error = None;
                self.log_state_db = Some(state_db.clone());
                Some(state_db)
            }
            Err(err) => {
                let message = format!(
                    "Failed to initialize logs DB at {}: {err}",
                    sqlite_home.display()
                );
                tracing::warn!(
                    error = %err,
                    sqlite_home = %sqlite_home.display(),
                    "failed to lazily initialize log/state runtime for console"
                );
                self.log_state_db_init_error = Some(message.clone());
                self.log_panel.set_error(message);
                None
            }
        }
    }
}
