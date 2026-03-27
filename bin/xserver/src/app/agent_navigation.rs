//! Multi-agent picker navigation and labeling state for the TUI app.
//!
//! This module exists to keep the pure parts of multi-agent navigation out of [`crate::app::App`].
//! It owns the stable spawn-order cache used by the `/agent` picker, keyboard next/previous
//! navigation, and the contextual footer label for the process currently being watched.
//!
//! Responsibilities here are intentionally narrow:
//! - remember picker entries and their first-seen order
//! - answer traversal questions like "what is the next process?"
//! - derive user-facing picker/footer text from cached process metadata
//!
//! Responsibilities that stay in `App`:
//! - discovering processes from the backend
//! - deciding which process is currently displayed
//! - mutating UI state such as switching processes or updating the footer widget
//!
//! The key invariant is that traversal follows first-seen spawn order rather than process-id sort
//! order. Once a process id is observed it keeps its place in the cycle even if the entry is later
//! updated or marked closed.

use crate::multi_agents::AgentPickerProcessEntry;
use crate::multi_agents::format_agent_picker_item_name;
use crate::multi_agents::next_agent_shortcut;
use crate::multi_agents::previous_agent_shortcut;
use chaos_ipc::ProcessId;
use ratatui::text::Span;
use std::collections::HashMap;

/// Small state container for multi-agent picker ordering and labeling.
///
/// `App` owns process lifecycle and UI side effects. This type keeps the pure rules for stable
/// spawn-order traversal, picker copy, and active-agent labels together and separately testable.
///
/// The core invariant is that `order` records first-seen process ids exactly once, while `processes`
/// stores the latest metadata for those ids. Mutation is intentionally funneled through `upsert`,
/// `mark_closed`, and `clear` so those two collections do not drift semantically even if they are
/// temporarily out of sync during teardown races.
#[derive(Debug, Default)]
pub(crate) struct AgentNavigationState {
    /// Latest picker metadata for each tracked process id.
    processes: HashMap<ProcessId, AgentPickerProcessEntry>,
    /// Stable first-seen traversal order for picker rows and keyboard cycling.
    order: Vec<ProcessId>,
}

/// Direction of keyboard traversal through the stable picker order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentNavigationDirection {
    /// Move toward the entry that was seen earlier in spawn order, wrapping at the front.
    Previous,
    /// Move toward the entry that was seen later in spawn order, wrapping at the end.
    Next,
}

impl AgentNavigationState {
    /// Returns the cached picker entry for a specific process id.
    ///
    /// Callers use this when they already know which process they care about and need the last
    /// metadata captured for picker or footer rendering. If a caller assumes every tracked process
    /// must be present here, shutdown races can turn that assumption into a panic elsewhere, so
    /// this stays optional.
    pub(crate) fn get(&self, process_id: &ProcessId) -> Option<&AgentPickerProcessEntry> {
        self.processes.get(process_id)
    }

    /// Returns whether the picker cache currently knows about any processes.
    ///
    /// This is the cheapest way for `App` to decide whether opening the picker should show "No
    /// agents available yet." rather than constructing picker rows from an empty state.
    pub(crate) fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }

    /// Inserts or updates a picker entry while preserving first-seen traversal order.
    ///
    /// The key invariant of this module is enforced here: a process id is appended to `order` only
    /// the first time it is seen. Later updates may change nickname, role, or closed state, but
    /// they must not move the process in the cycle or keyboard navigation would feel unstable.
    pub(crate) fn upsert(
        &mut self,
        process_id: ProcessId,
        agent_nickname: Option<String>,
        agent_role: Option<String>,
        is_closed: bool,
    ) {
        if !self.processes.contains_key(&process_id) {
            self.order.push(process_id);
        }
        self.processes.insert(
            process_id,
            AgentPickerProcessEntry {
                agent_nickname,
                agent_role,
                is_closed,
            },
        );
    }

    /// Marks a process as closed without removing it from the traversal cache.
    ///
    /// Closed processes stay in the picker and in spawn order so users can still review them and so
    /// next/previous navigation does not reshuffle around disappearing entries. If a caller "cleans
    /// this up" by deleting the entry instead, wraparound navigation will silently change shape
    /// mid-session.
    pub(crate) fn mark_closed(&mut self, process_id: ProcessId) {
        if let Some(entry) = self.processes.get_mut(&process_id) {
            entry.is_closed = true;
        } else {
            self.upsert(
                process_id, /*agent_nickname*/ None, /*agent_role*/ None,
                /*is_closed*/ true,
            );
        }
    }

    /// Drops all cached picker state.
    ///
    /// This is used when `App` tears down thread event state and needs the picker cache to return
    /// to a pristine single-session state.
    pub(crate) fn clear(&mut self) {
        self.processes.clear();
        self.order.clear();
    }

    /// Returns whether there is at least one tracked process other than the primary one.
    ///
    /// `App` uses this to decide whether the picker should be available even when the collaboration
    /// feature flag is currently disabled, because already-existing sub-agent processes should remain
    /// inspectable.
    pub(crate) fn has_non_primary_process(&self, primary_process_id: Option<ProcessId>) -> bool {
        self.processes
            .keys()
            .any(|process_id| Some(*process_id) != primary_process_id)
    }

    /// Returns live picker rows in the same order users cycle through them.
    ///
    /// The `order` vector is intentionally historical and may briefly contain process ids that no
    /// longer have cached metadata, so this filters through the map instead of assuming both
    /// collections are perfectly synchronized.
    pub(crate) fn ordered_processes(&self) -> Vec<(ProcessId, &AgentPickerProcessEntry)> {
        self.order
            .iter()
            .filter_map(|process_id| self.processes.get(process_id).map(|entry| (*process_id, entry)))
            .collect()
    }

    /// Returns the adjacent process id for keyboard navigation in stable spawn order.
    ///
    /// The caller must pass the process whose transcript is actually being shown to the user, not
    /// just whichever process bookkeeping most recently marked active. If the wrong current process
    /// is supplied, next/previous navigation will jump in a way that feels nondeterministic even
    /// though the cache itself is correct.
    pub(crate) fn adjacent_process_id(
        &self,
        current_displayed_process_id: Option<ProcessId>,
        direction: AgentNavigationDirection,
    ) -> Option<ProcessId> {
        let ordered_processes = self.ordered_processes();
        if ordered_processes.len() < 2 {
            return None;
        }

        let current_process_id = current_displayed_process_id?;
        let current_idx = ordered_processes
            .iter()
            .position(|(process_id, _)| *process_id == current_process_id)?;
        let next_idx = match direction {
            AgentNavigationDirection::Next => (current_idx + 1) % ordered_processes.len(),
            AgentNavigationDirection::Previous => {
                if current_idx == 0 {
                    ordered_processes.len() - 1
                } else {
                    current_idx - 1
                }
            }
        };
        Some(ordered_processes[next_idx].0)
    }

    /// Derives the contextual footer label for the currently displayed process.
    ///
    /// This intentionally returns `None` until there is more than one tracked process so
    /// single-process sessions do not waste footer space restating the obvious. When metadata for
    /// the displayed process is missing, the label falls back to the same generic naming rules used
    /// by the picker.
    pub(crate) fn active_agent_label(
        &self,
        current_displayed_process_id: Option<ProcessId>,
        primary_process_id: Option<ProcessId>,
    ) -> Option<String> {
        if self.processes.len() <= 1 {
            return None;
        }

        let process_id = current_displayed_process_id?;
        let is_primary = primary_process_id == Some(process_id);
        Some(
            self.processes
                .get(&process_id)
                .map(|entry| {
                    format_agent_picker_item_name(
                        entry.agent_nickname.as_deref(),
                        entry.agent_role.as_deref(),
                        is_primary,
                    )
                })
                .unwrap_or_else(|| {
                    format_agent_picker_item_name(
                        /*agent_nickname*/ None, /*agent_role*/ None, is_primary,
                    )
                }),
        )
    }

    /// Builds the `/agent` picker subtitle from the same canonical bindings used by key handling.
    ///
    /// Keeping this text derived from the actual shortcut helpers prevents the picker copy from
    /// drifting if the bindings ever change on one platform.
    pub(crate) fn picker_subtitle() -> String {
        let previous: Span<'static> = previous_agent_shortcut().into();
        let next: Span<'static> = next_agent_shortcut().into();
        format!(
            "Select an agent to watch. {} previous, {} next.",
            previous.content, next.content
        )
    }

    #[cfg(test)]
    /// Returns only the ordered process ids for focused tests of traversal invariants.
    ///
    /// This helper exists so tests can assert on ordering without embedding the full picker entry
    /// payload in every expectation.
    pub(crate) fn ordered_process_ids(&self) -> Vec<ProcessId> {
        self.ordered_processes()
            .into_iter()
            .map(|(process_id, _)| process_id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn populated_state() -> (AgentNavigationState, ProcessId, ProcessId, ProcessId) {
        let mut state = AgentNavigationState::default();
        let main_process_id =
            ProcessId::from_string("00000000-0000-0000-0000-000000000101").expect("valid thread");
        let first_agent_id =
            ProcessId::from_string("00000000-0000-0000-0000-000000000102").expect("valid thread");
        let second_agent_id =
            ProcessId::from_string("00000000-0000-0000-0000-000000000103").expect("valid thread");

        state.upsert(main_process_id, None, None, false);
        state.upsert(
            first_agent_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            false,
        );
        state.upsert(
            second_agent_id,
            Some("Bob".to_string()),
            Some("worker".to_string()),
            false,
        );

        (state, main_process_id, first_agent_id, second_agent_id)
    }

    #[test]
    fn upsert_preserves_first_seen_order() {
        let (mut state, main_process_id, first_agent_id, second_agent_id) = populated_state();

        state.upsert(
            first_agent_id,
            Some("Robie".to_string()),
            Some("worker".to_string()),
            true,
        );

        assert_eq!(
            state.ordered_process_ids(),
            vec![main_process_id, first_agent_id, second_agent_id]
        );
    }

    #[test]
    fn adjacent_process_id_wraps_in_spawn_order() {
        let (state, main_process_id, first_agent_id, second_agent_id) = populated_state();

        assert_eq!(
            state.adjacent_process_id(Some(second_agent_id), AgentNavigationDirection::Next),
            Some(main_process_id)
        );
        assert_eq!(
            state.adjacent_process_id(Some(second_agent_id), AgentNavigationDirection::Previous),
            Some(first_agent_id)
        );
        assert_eq!(
            state.adjacent_process_id(Some(main_process_id), AgentNavigationDirection::Previous),
            Some(second_agent_id)
        );
    }

    #[test]
    fn picker_subtitle_mentions_shortcuts() {
        let previous: Span<'static> = previous_agent_shortcut().into();
        let next: Span<'static> = next_agent_shortcut().into();
        let subtitle = AgentNavigationState::picker_subtitle();

        assert!(subtitle.contains(previous.content.as_ref()));
        assert!(subtitle.contains(next.content.as_ref()));
    }

    #[test]
    fn active_agent_label_tracks_current_thread() {
        let (state, main_process_id, first_agent_id, _) = populated_state();

        assert_eq!(
            state.active_agent_label(Some(first_agent_id), Some(main_process_id)),
            Some("Robie [explorer]".to_string())
        );
        assert_eq!(
            state.active_agent_label(Some(main_process_id), Some(main_process_id)),
            Some("Main [default]".to_string())
        );
    }
}
