//! BSP tiling manager wrapping [`ratatui_hypertile::Hypertile`].
//!
//! Owns the tiling state and maps each [`PaneId`] to a [`PaneKind`] so the
//! render loop knows which widget to paint in each rectangle.

use std::collections::HashMap;

use ratatui::layout::Direction;
use ratatui_hypertile::{EventOutcome, Hypertile, HypertileAction, PaneId, PaneSnapshot};

/// What lives in a given tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Variants used incrementally as pane types land.
pub(crate) enum PaneKind {
    /// Main chat — always present, never closeable.
    Chat,
    /// `/tools` — scrollable list of all model-visible tools.
    ToolList,
    /// Live MCP call monitor.
    McpActivity,
    /// `/mcp` management screen.
    McpManagement,
}

/// Wraps [`Hypertile`] with a pane-kind registry.
#[allow(dead_code)] // Methods used incrementally as pane types land.
pub(crate) struct TileManager {
    pub(crate) hypertile: Hypertile,
    pub(crate) pane_map: HashMap<PaneId, PaneKind>,
}

#[allow(dead_code)]
impl TileManager {
    pub fn new() -> Self {
        let hypertile = Hypertile::builder().with_gap(0).build();
        let mut pane_map = HashMap::new();
        pane_map.insert(PaneId::ROOT, PaneKind::Chat);
        Self {
            hypertile,
            pane_map,
        }
    }

    /// Returns the kind for a pane, if it exists.
    pub fn kind(&self, id: PaneId) -> Option<PaneKind> {
        self.pane_map.get(&id).copied()
    }

    /// Returns the currently focused pane id.
    pub fn focused(&self) -> Option<PaneId> {
        self.hypertile.focused_pane()
    }

    /// Returns true when only the chat pane exists (no splits).
    pub fn is_single_pane(&self) -> bool {
        self.pane_map.len() == 1
    }

    /// Split the focused pane and assign the new pane a kind.
    /// Returns the new `PaneId` or `None` if the split failed.
    pub fn split_focused(&mut self, direction: Direction, kind: PaneKind) -> Option<PaneId> {
        match self.hypertile.split_focused(direction) {
            Ok(new_id) => {
                self.pane_map.insert(new_id, kind);
                Some(new_id)
            }
            Err(_) => None,
        }
    }

    /// Open (or focus) a pane of the given kind. If one already exists, focus it
    /// instead of creating a duplicate.
    pub fn open_or_focus(&mut self, kind: PaneKind, direction: Direction) -> PaneId {
        // If a pane of this kind already exists, just focus it.
        if let Some((id, _)) = self.pane_map.iter().find(|(_, k)| **k == kind) {
            let _ = self.hypertile.focus_pane(*id);
            return *id;
        }
        // Otherwise split from chat (focus chat first so the split is adjacent).
        let _ = self.hypertile.focus_pane(PaneId::ROOT);
        self.split_focused(direction, kind)
            .unwrap_or(PaneId::ROOT)
    }

    /// Close the focused pane. Returns the closed pane's kind, or `None` if it
    /// was the chat pane (which cannot be closed).
    pub fn close_focused(&mut self) -> Option<PaneKind> {
        let focused = self.hypertile.focused_pane()?;
        if focused == PaneId::ROOT {
            return None; // Never close chat.
        }
        match self.hypertile.close_focused() {
            Ok(closed_id) => self.pane_map.remove(&closed_id),
            Err(_) => None,
        }
    }

    /// Close a pane by id. Returns its kind, or `None` if it was chat or not found.
    pub fn close_pane(&mut self, id: PaneId) -> Option<PaneKind> {
        if id == PaneId::ROOT {
            return None;
        }
        // Focus it first, then close.
        let _ = self.hypertile.focus_pane(id);
        match self.hypertile.close_focused() {
            Ok(closed_id) => self.pane_map.remove(&closed_id),
            Err(_) => None,
        }
    }

    /// Close all panes of a given kind.
    pub fn close_kind(&mut self, kind: PaneKind) {
        let ids: Vec<PaneId> = self
            .pane_map
            .iter()
            .filter(|(_, k)| **k == kind)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.close_pane(id);
        }
    }

    /// Close the last-opened auxiliary pane (any non-Chat pane).
    pub fn close_last_auxiliary(&mut self) {
        // Find any non-ROOT pane and close it.
        let aux_id = self
            .pane_map
            .keys()
            .find(|id| **id != PaneId::ROOT)
            .copied();
        if let Some(id) = aux_id {
            self.close_pane(id);
        }
    }

    /// Close all auxiliary panes, returning to single-pane chat.
    pub fn close_all_auxiliary(&mut self) {
        let aux_ids: Vec<PaneId> = self
            .pane_map
            .keys()
            .filter(|id| **id != PaneId::ROOT)
            .copied()
            .collect();
        for id in aux_ids {
            self.close_pane(id);
        }
        // Ensure focus returns to chat.
        let _ = self.hypertile.focus_pane(PaneId::ROOT);
    }

    /// Dispatch a tiling action (focus, resize, move).
    pub fn apply_action(&mut self, action: HypertileAction) -> EventOutcome {
        self.hypertile.apply_action(action)
    }

    /// Get a mutable reference to the underlying engine (needed for
    /// `StatefulWidget::render`).
    pub fn hypertile_mut(&mut self) -> &mut Hypertile {
        &mut self.hypertile
    }

    /// Iterate over all pane snapshots. Requires a prior `compute_layout` call
    /// (automatically done by `HypertileWidget`).
    pub fn panes(&self) -> Vec<PaneSnapshot> {
        self.hypertile.panes()
    }

    /// Find the PaneId for a given kind (first match).
    pub fn find_pane(&self, kind: PaneKind) -> Option<PaneId> {
        self.pane_map
            .iter()
            .find(|(_, k)| **k == kind)
            .map(|(id, _)| *id)
    }
}
