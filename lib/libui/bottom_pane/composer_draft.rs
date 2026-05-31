use std::path::PathBuf;

use chaos_ipc::user_input::TextElement;

use crate::bottom_pane::ChatComposer;

#[derive(Default, Clone, PartialEq)]
pub(super) struct ComposerDraft {
    pub(super) text: String,
    pub(super) text_elements: Vec<TextElement>,
    pub(super) local_image_paths: Vec<PathBuf>,
    pub(super) pending_pastes: Vec<(String, String)>,
}

impl ComposerDraft {
    pub(super) fn text_with_pending(&self) -> String {
        if self.pending_pastes.is_empty() {
            return self.text.clone();
        }
        debug_assert!(
            !self.text_elements.is_empty(),
            "pending pastes should always have matching text elements"
        );
        let (expanded, _) = ChatComposer::expand_pending_pastes(
            &self.text,
            self.text_elements.clone(),
            &self.pending_pastes,
        );
        expanded
    }
}
