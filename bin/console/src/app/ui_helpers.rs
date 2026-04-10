use super::*;

impl App {
    pub(super) fn open_url_in_browser(&mut self, url: String) -> bool {
        if let Err(err) = webbrowser::open(&url) {
            self.chat_widget
                .add_error_message(format!("Failed to open browser for {url}: {err}"));
            return false;
        }

        self.chat_widget
            .add_info_message(format!("Opened {url} in your browser."), /*hint*/ None);
        true
    }

    pub(super) fn clear_ui_header_lines(&self, _width: u16) -> Vec<Line<'static>> {
        Vec::new()
    }

    pub(super) fn queue_clear_ui_header(&mut self, tui: &mut tui::Tui) {
        let width = tui.terminal.last_known_screen_size.width;
        let header_lines = self.clear_ui_header_lines(width);
        if !header_lines.is_empty() {
            tui.insert_history_lines(header_lines);
            self.has_emitted_history_lines = true;
        }
    }

    pub(super) fn clear_terminal_ui(
        &mut self,
        tui: &mut tui::Tui,
        redraw_header: bool,
    ) -> Result<()> {
        let is_alt_screen_active = tui.is_alt_screen_active();

        // Drop queued history insertions so stale transcript lines cannot be flushed after /clear.
        tui.clear_pending_history_lines();

        if is_alt_screen_active {
            tui.terminal.clear_visible_screen()?;
        } else {
            // Some terminals (Terminal.app, Warp) do not reliably drop scrollback when purge and
            // clear are emitted as separate backend commands. Prefer a single ANSI sequence.
            tui.terminal.clear_scrollback_and_visible_screen_ansi()?;
        }

        let mut area = tui.terminal.viewport_area;
        if area.y > 0 {
            // After a full clear, anchor the inline viewport at the top and redraw a fresh header
            // box. `insert_history_lines()` will shift the viewport down by the rendered height.
            area.y = 0;
            tui.terminal.set_viewport_area(area);
        }
        self.has_emitted_history_lines = false;

        if redraw_header {
            self.queue_clear_ui_header(tui);
        }
        Ok(())
    }

    pub(super) fn reset_app_ui_state_after_clear(&mut self) {
        self.overlay = None;
        self.transcript_cells.clear();
        self.deferred_history_lines.clear();
        self.has_emitted_history_lines = false;
        self.backtrack = BacktrackState::default();
        self.backtrack_render_pending = false;
    }

    pub(super) fn reasoning_label(reasoning_effort: Option<ReasoningEffortConfig>) -> &'static str {
        match reasoning_effort {
            Some(ReasoningEffortConfig::Minimal) => "minimal",
            Some(ReasoningEffortConfig::Low) => "low",
            Some(ReasoningEffortConfig::Medium) => "medium",
            Some(ReasoningEffortConfig::High) => "high",
            Some(ReasoningEffortConfig::XHigh) => "xhigh",
            None | Some(ReasoningEffortConfig::None) => "default",
        }
    }

    pub(super) fn reasoning_label_for(
        model: &str,
        reasoning_effort: Option<ReasoningEffortConfig>,
    ) -> Option<&'static str> {
        (!model.starts_with("codex-auto-")).then(|| Self::reasoning_label(reasoning_effort))
    }

    pub(crate) fn token_usage(&self) -> chaos_ipc::protocol::TokenUsage {
        self.chat_widget.token_usage()
    }

    pub(super) fn on_update_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        // TODO(aibrahim): Remove this and don't use config as a state object.
        // Instead, explicitly pass the stored collaboration mode's effort into new sessions.
        self.config.model_reasoning_effort = effort;
        self.chat_widget.set_reasoning_effort(effort);
    }

    pub(super) fn on_update_personality(&mut self, personality: Personality) {
        self.config.personality = Some(personality);
        self.chat_widget.set_personality(personality);
    }

    pub(super) fn sync_tui_theme_selection(&mut self, name: String) {
        self.config.tui_theme = Some(name.clone());
        self.chat_widget.set_tui_theme(Some(name));
    }

    pub(super) fn restore_runtime_theme_from_config(&self) {
        if let Some(name) = self.config.tui_theme.as_deref()
            && let Some(theme) =
                crate::render::highlight::resolve_theme_by_name(name, Some(&self.config.chaos_home))
        {
            crate::render::highlight::set_syntax_theme(theme);
            return;
        }

        let auto_theme_name = crate::render::highlight::adaptive_default_theme_name();
        if let Some(theme) = crate::render::highlight::resolve_theme_by_name(
            auto_theme_name,
            Some(&self.config.chaos_home),
        ) {
            crate::render::highlight::set_syntax_theme(theme);
        }
    }

    pub(super) fn personality_label(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "None",
            Personality::Friendly => "Friendly",
            Personality::Pragmatic => "Pragmatic",
        }
    }

    pub(super) fn refresh_status_line(&mut self) {
        self.chat_widget.refresh_status_line();
    }

    pub(super) fn on_all_tools_received(
        &mut self,
        tui: &mut tui::Tui,
        ev: chaos_ipc::protocol::AllToolsResponseEvent,
    ) {
        use ratatui::layout::Direction;
        // Toggle: if already open, close it; otherwise open.
        self.tool_list_close.set(false);
        if self.tile_manager.find_pane(PaneKind::ToolList).is_some() {
            self.tile_manager.close_kind(PaneKind::ToolList);
        } else {
            self.tool_list_pane.borrow_mut().set_tools(ev.tools);
            self.tile_manager
                .open_or_focus(PaneKind::ToolList, Direction::Horizontal);
        }
        tui.frame_requester().schedule_frame();
    }
}
