//! Tiling manager backed by [`HypertileRuntime`].
//!
//! [`PaneKind`] variants are registered as plugin types so the runtime owns
//! the pane-to-kind mapping. A `pane_ids` set is kept in sync with every
//! structural mutation so helpers that enumerate panes never touch the layout
//! cache (`core().panes()` is only valid after `compute_layout`).

use crate::panes::chat_plugin::ChatPlugin;
use crate::panes::inspector::{InspectorPane, InspectorPlugin};
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
    HypertilePlugin, HypertileRuntime, HypertileRuntimeBuilder, InputMode, PaletteBehavior,
    PaletteConfig, mouse_event_from_crossterm,
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

    pub(crate) fn from_str(s: &str) -> Option<Self> {
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

#[derive(Clone, Copy)]
enum LayoutDrag {
    Resize,
    Swap,
}

/// Wraps [`HypertileRuntime`] and exposes a [`PaneKind`]-aware API.
///
/// `pane_ids` mirrors the registry and is updated on every structural
/// mutation so enumeration helpers never depend on the layout cache.
pub(crate) struct TileManager {
    pub(crate) runtime: HypertileRuntime,
    /// Registry-accurate set of live pane ids.
    pane_ids: HashSet<PaneId>,
    pub(crate) inspector: Rc<RefCell<InspectorPane>>,
    inspector_enabled: bool,
    layout_drag: Option<LayoutDrag>,
    rendered_full_viewport: bool,
    pub(crate) chat_history: Vec<ratatui::text::Line<'static>>,
    pub(crate) chat_history_key: Option<(u16, u16, usize, usize)>,
}

impl TileManager {
    pub fn new(
        tool_list_state: Rc<RefCell<ToolListPane>>,
        tool_list_close: Rc<Cell<bool>>,
    ) -> Self {
        let mut runtime = HypertileRuntimeBuilder::default()
            .with_gap(0)
            .with_palette_config(PaletteConfig {
                allowed_plugins: Some(
                    [PANE_CHAT, PANE_TOOL_LIST, PANE_INSPECTOR]
                        .map(str::to_string)
                        .to_vec(),
                ),
                behavior: PaletteBehavior::EmitSelection,
            })
            .build();
        runtime.set_mode(InputMode::PluginInput);
        let inspector = Rc::new(RefCell::new(InspectorPane::default()));

        runtime.register_plugin_type(PANE_CHAT, || ChatPlugin);
        runtime.register_plugin_type(PANE_TOOL_LIST, move || {
            ToolListPlugin::new(tool_list_state.clone(), tool_list_close.clone())
        });
        runtime.register_plugin_type(PANE_MCP_ACTIVITY, || EmptyPlugin);
        runtime.register_plugin_type(PANE_MCP_MANAGEMENT, || EmptyPlugin);
        runtime.register_plugin_type(PANE_INSPECTOR, {
            let inspector = inspector.clone();
            move || InspectorPlugin(inspector.clone())
        });

        // ROOT is created with the default "block" placeholder — replace with Chat.
        let _ = runtime.replace_pane_plugin(PaneId::ROOT, PANE_CHAT);

        let mut pane_ids = HashSet::new();
        pane_ids.insert(PaneId::ROOT);

        Self {
            runtime,
            pane_ids,
            inspector,
            inspector_enabled: false,
            layout_drag: None,
            rendered_full_viewport: false,
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

    pub fn open_inspector(&mut self, width: u16) {
        self.inspector_enabled = true;
        self.sync_inspector(width);
        if let Some(id) = self.find_pane(PaneKind::Inspector) {
            let _ = self.runtime.focus_pane(id);
        }
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

    /// Input defaults to chat until an auxiliary pane has focus.
    pub fn chat_focused(&self) -> bool {
        self.focused().is_none_or(|id| id == PaneId::ROOT)
    }

    pub fn focused_kind(&self) -> Option<PaneKind> {
        self.focused().and_then(|id| self.kind(id))
    }

    /// Returns true when only the chat pane exists (no splits).
    pub fn is_single_pane(&self) -> bool {
        self.pane_ids.len() == 1
    }

    pub fn uses_full_viewport(&self) -> bool {
        !self.is_single_pane() || self.runtime.is_palette_open()
    }

    pub fn needs_inline_history_restore(&self) -> bool {
        self.rendered_full_viewport && !self.uses_full_viewport()
    }

    pub fn mark_inline_history_restored(&mut self) {
        self.rendered_full_viewport = false;
    }

    /// Split the focused pane and assign the new pane a kind.
    pub fn split_focused(&mut self, direction: Direction, kind: PaneKind) -> Option<PaneId> {
        self.cancel_layout_drag();
        let new_id = self.runtime.split_focused(direction, kind.as_str()).ok()?;
        self.pane_ids.insert(new_id);
        Some(new_id)
    }

    /// Open (or focus) a pane of the given kind. If one already exists,
    /// focus it instead of creating a duplicate.
    pub fn open_or_focus(&mut self, kind: PaneKind, direction: Direction) -> PaneId {
        self.cancel_layout_drag();
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
        self.cancel_layout_drag();
        let kind = self.kind(id);
        let _ = self.runtime.focus_pane(id);
        self.runtime.close_focused().ok()?;
        self.pane_ids.remove(&id);
        if kind == Some(PaneKind::Inspector) {
            self.inspector_enabled = false;
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
        self.cancel_layout_drag();
        self.runtime.handle_event(HypertileEvent::Action(action))
    }

    /// Render all panes through the runtime's plugin registry.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        self.rendered_full_viewport = self.uses_full_viewport();
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

    pub fn handle_focused_plugin_key(&mut self, chord: KeyChord) -> EventOutcome {
        self.cancel_layout_drag();
        let outcome = self.runtime.handle_event(HypertileEvent::Key(chord));
        // Escape can switch the runtime to Layout; never leave its bare-letter
        // split/close bindings active for subsequent pane input.
        self.runtime.set_mode(InputMode::PluginInput);
        outcome
    }

    /// Cancel capture as well as the runtime's pending drag, without rolling
    /// back already-applied resize steps.
    pub fn cancel_layout_drag(&mut self) -> bool {
        if self.layout_drag.take().is_none() {
            return false;
        }
        self.runtime.set_mode(InputMode::PluginInput);
        true
    }

    /// Delegate layout gestures and pane-local input to Hypertile. Only an
    /// exact divider hit or Alt+left press starts a layout gesture.
    pub fn handle_pane_mouse(&mut self, mouse: crossterm::event::MouseEvent) -> bool {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
        let event = HypertileEvent::Mouse(mouse_event_from_crossterm(mouse));
        if self.runtime.is_palette_open() {
            self.runtime.handle_event(event);
            return true;
        }
        if self.is_single_pane() {
            self.cancel_layout_drag();
            return false;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            self.cancel_layout_drag();
            let alt = mouse.modifiers == KeyModifiers::ALT;
            // Match the runtime's one-cell tolerance for Alt gestures, but
            // don't steal ordinary clicks beside the divider from a plugin.
            let resize = self
                .runtime
                .core()
                .split_at(mouse.column, mouse.row, u16::from(alt))
                .is_some();
            if alt || (resize && mouse.modifiers.is_empty()) {
                self.runtime.set_mode(InputMode::Layout);
                if self.runtime.handle_event(event).is_consumed() {
                    self.layout_drag = Some(if resize {
                        LayoutDrag::Resize
                    } else {
                        LayoutDrag::Swap
                    });
                    return true;
                }
                self.runtime.set_mode(InputMode::PluginInput);
                return false;
            }
        }
        if let Some(drag) = self.layout_drag {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) if matches!(drag, LayoutDrag::Resize) => {
                    self.runtime.handle_event(event);
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.runtime.handle_event(event);
                    self.cancel_layout_drag();
                }
                // The runtime swaps on release, including its drag threshold.
                // Omit floating previews: App paints chat outside the registry.
                _ => {}
            }
            return true;
        }
        let target = self.runtime.core().pane_at(mouse.column, mouse.row);
        let outcome = self.runtime.handle_event(event);
        // Even ignored auxiliary input must not open the chat transcript.
        outcome.is_consumed() || target.is_some_and(|id| id != PaneId::ROOT)
    }
}

#[cfg(test)]
mod tests;
