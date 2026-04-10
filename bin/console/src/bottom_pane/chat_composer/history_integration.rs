use super::ChatComposer;

use crate::bottom_pane::chat_composer_history::HistoryEntry;

impl ChatComposer {
    pub(crate) fn set_history_metadata(&mut self, log_id: u64, entry_count: usize) {
        self.history.set_metadata(log_id, entry_count);
    }

    /// Integrate an asynchronous response to an on-demand history lookup.
    ///
    /// If the entry is present and the offset still matches the active history cursor, the
    /// composer rehydrates the entry immediately. This path intentionally routes through
    /// [`Self::apply_history_entry`] so cursor placement remains aligned with keyboard history
    /// recall semantics.
    pub(crate) fn on_history_entry_response(
        &mut self,
        log_id: u64,
        offset: usize,
        entry: Option<String>,
    ) -> bool {
        let Some(entry) = self.history.on_entry_response(log_id, offset, entry) else {
            return false;
        };
        // Persistent ↑/↓ history is text-only (backwards-compatible and avoids persisting
        // attachments), but local in-session ↑/↓ history can rehydrate elements and image paths.
        self.apply_history_entry(entry);
        true
    }

    /// Rehydrate a history entry into the composer with shell-like cursor placement.
    ///
    /// This path restores text, elements, images, mention bindings, and pending paste payloads,
    /// then moves the cursor to end-of-line. If a caller reused
    /// [`Self::set_text_content_with_mention_bindings`] directly for history recall and forgot the
    /// final cursor move, repeated Up/Down would stop navigating history because cursor-gating
    /// treats interior positions as normal editing mode.
    pub(super) fn apply_history_entry(&mut self, entry: HistoryEntry) {
        let HistoryEntry {
            text,
            text_elements,
            local_image_paths,
            remote_image_urls,
            mention_bindings,
            pending_pastes,
        } = entry;
        self.set_remote_image_urls(remote_image_urls);
        self.set_text_content_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
        );
        self.set_pending_pastes(pending_pastes);
        self.move_cursor_to_end();
    }

    pub(crate) fn clear_for_ctrl_c(&mut self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let previous = self.current_text();
        let text_elements = self.textarea.text_elements();
        let local_image_paths = self
            .attached_images
            .iter()
            .map(|img| img.path.clone())
            .collect();
        let pending_pastes = std::mem::take(&mut self.pending_pastes);
        let remote_image_urls = self.remote_image_urls.clone();
        let mention_bindings = self.snapshot_mention_bindings();
        self.set_text_content(String::new(), Vec::new(), Vec::new());
        self.remote_image_urls.clear();
        self.selected_remote_image_index = None;
        self.history.reset_navigation();
        self.history.record_local_submission(HistoryEntry {
            text: previous.clone(),
            text_elements,
            local_image_paths,
            remote_image_urls,
            mention_bindings,
            pending_pastes,
        });
        Some(previous)
    }
}
