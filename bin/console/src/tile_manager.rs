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
use ratatui_hypertile::{EventOutcome, HypertileAction, HypertileEvent, KeyChord, PaneId};
use ratatui_hypertile_extras::{
    HypertilePlugin, HypertileRuntime, HypertileRuntimeBuilder, InputMode,
};

// Plugin-type name constants — string keys in the runtime registry.
const PANE_CHAT: &str = "chat";
const PANE_TOOL_LIST: &str = "tool_list";
const PANE_MCP_ACTIVITY: &str = "mcp_activity";
const PANE_MCP_MANAGEMENT: &str = "mcp_management";
const PANE_INSPECTOR: &str = "inspector";

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
    /// Read-only MCP resources.
    Inspector,
}

impl PaneKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Chat => PANE_CHAT,
            Self::ToolList => PANE_TOOL_LIST,
            Self::McpActivity => PANE_MCP_ACTIVITY,
            Self::McpManagement => PANE_MCP_MANAGEMENT,
            Self::Inspector => PANE_INSPECTOR,
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            PANE_CHAT => Some(Self::Chat),
            PANE_TOOL_LIST => Some(Self::ToolList),
            PANE_MCP_ACTIVITY => Some(Self::McpActivity),
            PANE_MCP_MANAGEMENT => Some(Self::McpManagement),
            PANE_INSPECTOR => Some(Self::Inspector),
            _ => None,
        }
    }
}

struct EmptyPlugin;

impl HypertilePlugin for EmptyPlugin {
    fn render(&mut self, _area: Rect, _buf: &mut Buffer, _focused: bool) {}
}

/// Wraps [`HypertileRuntime`] and exposes a [`PaneKind`]-aware API.
///
/// `pane_ids` mirrors the registry and is updated on every structural
/// mutation so enumeration helpers never depend on the layout cache.
pub(crate) struct TileManager {
    pub(crate) runtime: HypertileRuntime,
    /// Registry-accurate set of live pane ids.
    pane_ids: HashSet<PaneId>,
    pub(crate) inspector: crate::panes::inspector::InspectorPane,
    inspector_enabled: bool,
    rendered_tiled_history: bool,
    pub(crate) chat_history: Vec<ratatui::text::Line<'static>>,
    pub(crate) chat_history_key: Option<(u16, u16, usize, usize)>,
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
        runtime.register_plugin_type(PANE_INSPECTOR, || EmptyPlugin);

        // ROOT is created with the default "block" placeholder — replace with Chat.
        let _ = runtime.replace_pane_plugin(PaneId::ROOT, PANE_CHAT);

        let mut pane_ids = HashSet::new();
        pane_ids.insert(PaneId::ROOT);

        Self {
            runtime,
            pane_ids,
            inspector: Default::default(),
            inspector_enabled: false,
            rendered_tiled_history: false,
            chat_history: Vec::new(),
            chat_history_key: None,
        }
    }

    /// Hide on narrow terminals without forgetting the user's toggle.
    pub fn sync_inspector(&mut self, width: u16) {
        let existing = self.find_pane(PaneKind::Inspector);
        if self.inspector_enabled && width >= 110 {
            if existing.is_none() {
                let focused = self.focused().unwrap_or(PaneId::ROOT);
                let id = self.open_or_focus(PaneKind::Inspector, Direction::Horizontal);
                let _ = self.runtime.focus_pane(id);
                self.apply_action(HypertileAction::ResizeFocused { delta: -0.18 });
                let _ = self.runtime.focus_pane(focused);
            }
        } else if let Some(id) = existing {
            let enabled = self.inspector_enabled;
            let focused = self.focused().unwrap_or(PaneId::ROOT);
            self.close_pane(id);
            self.inspector_enabled = enabled;
            let _ = self.runtime.focus_pane(focused);
        }
    }

    pub fn toggle_inspector(&mut self, width: u16) {
        self.inspector_enabled = !self.inspector_enabled;
        self.sync_inspector(width);
    }

    /// The inspector must be escapable even when the retained composer has a popup.
    pub fn leave_inspector(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyEventKind};
        if self.focused_kind() == Some(PaneKind::Inspector)
            && key.kind == KeyEventKind::Press
            && matches!(key.code, KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab)
        {
            let _ = self.runtime.focus_pane(PaneId::ROOT);
            return true;
        }
        false
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

    pub fn focused_kind(&self) -> Option<PaneKind> {
        self.focused().and_then(|id| self.kind(id))
    }

    /// Returns true when only the chat pane exists (no splits).
    pub fn is_single_pane(&self) -> bool {
        self.pane_ids.len() == 1
    }

    pub fn needs_inline_history_restore(&self) -> bool {
        self.rendered_tiled_history && self.is_single_pane()
    }

    pub fn mark_inline_history_restored(&mut self) {
        self.rendered_tiled_history = false;
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
        if let Some(id) = self.find_pane(kind) {
            let _ = self.runtime.focus_pane(id);
            return id;
        }

        let _ = self.runtime.focus_pane(PaneId::ROOT);
        self.split_focused(direction, kind).unwrap_or(PaneId::ROOT)
    }

    /// Close the focused pane. Chat (ROOT) is never closed.
    pub fn close_focused(&mut self) -> Option<PaneKind> {
        self.close_pane(self.focused()?)
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
        if kind == Some(PaneKind::Inspector) {
            self.inspector_enabled = false;
            self.inspector.pause();
        }
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
        self.rendered_tiled_history = !self.is_single_pane();
        self.runtime.render(area, buf);
        if let Some(id) = self.find_pane(PaneKind::Inspector)
            && let Some(rect) = self.pane_rect(id)
        {
            self.inspector.render(rect, buf, self.focused() == Some(id));
        }
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

    pub fn handle_focused_plugin_key(&mut self, chord: KeyChord) -> EventOutcome {
        let previous_mode = self.runtime.mode();
        self.runtime.set_mode(InputMode::PluginInput);
        let outcome = self.runtime.handle_event(HypertileEvent::Key(chord));
        self.runtime.set_mode(previous_mode);
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspector_split_preserves_chat_focus_and_respects_visibility() {
        let mut tiles = TileManager::new(
            Rc::new(RefCell::new(ToolListPane::new())),
            Rc::new(Cell::new(false)),
        );
        let area = Rect::new(0, 0, 140, 40);
        tiles.sync_inspector(area.width);
        assert!(tiles.is_single_pane(), "the inspector starts closed");
        tiles.toggle_inspector(area.width);
        tiles.render(area, &mut Buffer::empty(area));
        let inspector = tiles.find_pane(PaneKind::Inspector).expect("inspector");
        let chat_rect = tiles.pane_rect(PaneId::ROOT).expect("chat rect");
        let inspector_rect = tiles.pane_rect(inspector).expect("inspector rect");
        assert_eq!(tiles.focused(), Some(PaneId::ROOT));
        assert!(chat_rect.right() <= inspector_rect.x);
        assert!(chat_rect.width > inspector_rect.width);

        for code in [
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyCode::Tab,
        ] {
            tiles
                .runtime
                .focus_pane(inspector)
                .expect("focus inspector");
            assert!(tiles.leave_inspector(crossterm::event::KeyEvent::new(
                code,
                crossterm::event::KeyModifiers::NONE
            )));
            assert_eq!(tiles.focused(), Some(PaneId::ROOT));
            assert_eq!(tiles.find_pane(PaneKind::Inspector), Some(inspector));
        }

        tiles.sync_inspector(80);
        assert!(tiles.is_single_pane());
        assert!(tiles.needs_inline_history_restore());
        tiles.mark_inline_history_restored();
        assert!(!tiles.needs_inline_history_restore());
        tiles.sync_inspector(area.width);
        assert!(tiles.find_pane(PaneKind::Inspector).is_some());
        tiles.render(area, &mut Buffer::empty(area));
        tiles.toggle_inspector(area.width);
        tiles.sync_inspector(area.width);
        assert!(
            tiles.is_single_pane(),
            "a manually hidden panel must stay hidden"
        );
        assert!(tiles.needs_inline_history_restore());
    }
}
