//! Tiling manager backed by [`HypertileRuntime`].
//!
//! [`PaneKind`] variants are registered as plugin types so the runtime owns
//! the pane-to-kind mapping. A `pane_ids` set is kept in sync with every
//! structural mutation so helpers that enumerate panes never touch the layout
//! cache (`core().panes()` is only valid after `compute_layout`).

use crate::panes::chat_plugin::ChatPlugin;
use crate::panes::tool_list::ToolListPane;
use crate::panes::tool_list_plugin::ToolListPlugin;
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use ratatui::buffer::Buffer;
use ratatui::layout::{Direction, Rect};
use ratatui_hypertile::{EventOutcome, HypertileAction, HypertileEvent, PaneId};
use ratatui_hypertile_extras::{HypertilePlugin, HypertileRuntime, HypertileRuntimeBuilder};

// Plugin-type name constants — string keys in the runtime registry.
const PANE_CHAT: &str = "chat";
const PANE_TOOL_LIST: &str = "tool_list";
const PANE_MCP_ACTIVITY: &str = "mcp_activity";
const PANE_MCP_MANAGEMENT: &str = "mcp_management";

/// What lives in a given tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl PaneKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Chat => PANE_CHAT,
            Self::ToolList => PANE_TOOL_LIST,
            Self::McpActivity => PANE_MCP_ACTIVITY,
            Self::McpManagement => PANE_MCP_MANAGEMENT,
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            PANE_CHAT => Some(Self::Chat),
            PANE_TOOL_LIST => Some(Self::ToolList),
            PANE_MCP_ACTIVITY => Some(Self::McpActivity),
            PANE_MCP_MANAGEMENT => Some(Self::McpManagement),
            _ => None,
        }
    }
}

struct EmptyPlugin;

impl HypertilePlugin for EmptyPlugin {
    fn render(&self, _area: Rect, _buf: &mut Buffer, _focused: bool) {}
}

/// Wraps [`HypertileRuntime`] and exposes a [`PaneKind`]-aware API.
///
/// `pane_ids` mirrors the registry and is updated on every structural
/// mutation so enumeration helpers never depend on the layout cache.
pub(crate) struct TileManager {
    pub(crate) runtime: HypertileRuntime,
    /// Registry-accurate set of live pane ids.
    pane_ids: HashSet<PaneId>,
}

impl TileManager {
    pub fn new(
        tool_list_state: Rc<RefCell<ToolListPane>>,
        tool_list_close: Rc<Cell<bool>>,
    ) -> Self {
        let mut runtime = HypertileRuntimeBuilder::default().with_gap(0).build();

        runtime.register_plugin_type(PANE_CHAT, || ChatPlugin);
        runtime.register_plugin_type(PANE_TOOL_LIST, move || {
            ToolListPlugin::new(tool_list_state.clone(), tool_list_close.clone())
        });
        runtime.register_plugin_type(PANE_MCP_ACTIVITY, || EmptyPlugin);
        runtime.register_plugin_type(PANE_MCP_MANAGEMENT, || EmptyPlugin);

        // ROOT is created with the default "block" placeholder — replace with Chat.
        let _ = runtime.replace_pane_plugin(PaneId::ROOT, PANE_CHAT);

        let mut pane_ids = HashSet::new();
        pane_ids.insert(PaneId::ROOT);

        Self { runtime, pane_ids }
    }

    /// Returns the kind for a pane, queried from the registry (always accurate).
    pub fn kind(&self, id: PaneId) -> Option<PaneKind> {
        self.runtime
            .registry()
            .plugin_type_for(id)
            .and_then(PaneKind::from_str)
    }

    /// Returns the currently focused pane id.
    pub fn focused(&self) -> Option<PaneId> {
        self.runtime.focused_pane()
    }

    /// Returns true when only the chat pane exists (no splits).
    pub fn is_single_pane(&self) -> bool {
        self.pane_ids.len() == 1
    }

    /// Split the focused pane and assign the new pane a kind.
    pub fn split_focused(&mut self, direction: Direction, kind: PaneKind) -> Option<PaneId> {
        let new_id = self.runtime.split_focused(direction, kind.as_str()).ok()?;
        self.pane_ids.insert(new_id);
        Some(new_id)
    }

    /// Open (or focus) a pane of the given kind. If one already exists,
    /// focus it instead of creating a duplicate.
    pub fn open_or_focus(&mut self, kind: PaneKind, direction: Direction) -> PaneId {
        // Query registry — always accurate, no layout dependency.
        let existing = self
            .pane_ids
            .iter()
            .copied()
            .find(|&id| self.kind(id) == Some(kind));

        if let Some(id) = existing {
            let _ = self.runtime.focus_pane(id);
            return id;
        }

        let _ = self.runtime.focus_pane(PaneId::ROOT);
        self.split_focused(direction, kind).unwrap_or(PaneId::ROOT)
    }

    /// Close the focused pane. Chat (ROOT) is never closed.
    pub fn close_focused(&mut self) -> Option<PaneKind> {
        let focused = self.runtime.focused_pane()?;
        if focused == PaneId::ROOT {
            return None;
        }
        let kind = self.kind(focused);
        self.runtime.close_focused().ok()?;
        self.pane_ids.remove(&focused);
        kind
    }

    /// Close a specific pane by id. Chat is never closed.
    pub fn close_pane(&mut self, id: PaneId) -> Option<PaneKind> {
        if id == PaneId::ROOT {
            return None;
        }
        let kind = self.kind(id);
        let _ = self.runtime.focus_pane(id);
        self.runtime.close_focused().ok()?;
        self.pane_ids.remove(&id);
        kind
    }

    /// Close all panes of a given kind.
    pub fn close_kind(&mut self, kind: PaneKind) {
        let ids: Vec<PaneId> = self
            .pane_ids
            .iter()
            .copied()
            .filter(|&id| self.kind(id) == Some(kind))
            .collect();
        for id in ids {
            self.close_pane(id);
        }
    }

    /// Close the last auxiliary (non-Chat) pane.
    pub fn close_last_auxiliary(&mut self) {
        let aux = self.pane_ids.iter().copied().find(|&id| id != PaneId::ROOT);
        if let Some(id) = aux {
            self.close_pane(id);
        }
    }

    /// Close all auxiliary panes, returning to single-pane chat.
    pub fn close_all_auxiliary(&mut self) {
        let aux_ids: Vec<PaneId> = self
            .pane_ids
            .iter()
            .copied()
            .filter(|&id| id != PaneId::ROOT)
            .collect();
        for id in aux_ids {
            self.close_pane(id);
        }
        let _ = self.runtime.focus_pane(PaneId::ROOT);
    }

    /// Dispatch a tiling action (focus, resize, move).
    pub fn apply_action(&mut self, action: HypertileAction) -> EventOutcome {
        self.runtime.handle_event(HypertileEvent::Action(action))
    }

    /// Render all panes through the runtime's plugin registry.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        self.runtime.render(area, buf);
    }

    /// Pane rect after layout (valid after a render_with call).
    pub fn pane_rect(&self, id: PaneId) -> Option<Rect> {
        self.runtime.pane_rect(id)
    }

    /// Find the first pane of a given kind (registry-accurate).
    pub fn find_pane(&self, kind: PaneKind) -> Option<PaneId> {
        self.pane_ids
            .iter()
            .copied()
            .find(|&id| self.kind(id) == Some(kind))
    }

    pub fn plugin_mut(&mut self, id: PaneId) -> Option<&mut (dyn HypertilePlugin + 'static)> {
        self.runtime.registry_mut().plugin_mut(id)
    }
}
