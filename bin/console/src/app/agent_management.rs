use super::{
    AgentNavigationState, App, AppEvent, BacktrackState, ChatWidget, EventMsg, PathBuf,
    ProcessEventSnapshot, ProcessId, Result, SelectionItem, SelectionViewParams,
    agent_picker_status_dot_spans, format_agent_picker_item_name, standard_popup_hint_line, tui,
    unbounded_channel,
};
use chaos_kern::Process;
use std::sync::Arc;

impl App {
    pub(super) fn process_label(&self, process_id: ProcessId) -> String {
        let is_primary = self.primary_process_id == Some(process_id);
        let fallback_label = if is_primary {
            "Main [default]".to_string()
        } else {
            let process_id = process_id.to_string();
            let short_id: String = process_id.chars().take(8).collect();
            format!("Agent ({short_id})")
        };
        if let Some(entry) = self.agent_navigation.get(&process_id) {
            let label = format_agent_picker_item_name(
                entry.agent_nickname.as_deref(),
                entry.agent_role.as_deref(),
                is_primary,
            );
            if label == "Agent" {
                let process_id = process_id.to_string();
                let short_id: String = process_id.chars().take(8).collect();
                format!("{label} ({short_id})")
            } else {
                label
            }
        } else {
            fallback_label
        }
    }

    /// Returns the thread whose transcript is currently on screen.
    ///
    /// `active_process_id` is the source of truth during steady state, but the widget can briefly
    /// lag behind thread bookkeeping during transitions. The footer label and adjacent-thread
    /// navigation both follow what the user is actually looking at, not whichever thread most
    /// recently began switching.
    pub(super) fn current_displayed_process_id(&self) -> Option<ProcessId> {
        self.active_process_id.or(self.chat_widget.process_id())
    }

    /// Mirrors the visible thread into the contextual footer row.
    ///
    /// The footer sometimes shows ambient context instead of an instructional hint. In multi-agent
    /// sessions, that contextual row includes the currently viewed agent label. The label is
    /// intentionally hidden until there is more than one known thread so single-thread sessions do
    /// not spend footer space restating that the user is already on the main conversation.
    pub(super) fn sync_active_agent_label(&mut self) {
        let label = self
            .agent_navigation
            .active_agent_label(self.current_displayed_process_id(), self.primary_process_id);
        self.chat_widget.set_active_agent_label(label);
    }

    pub(super) async fn process_cwd(&self, process_id: ProcessId) -> Option<PathBuf> {
        let channel = self.process_event_channels.get(&process_id)?;
        let store = channel.store.lock().await;
        match store.session_configured.as_ref().map(|event| &event.msg) {
            Some(EventMsg::SessionConfigured(session)) => Some(session.cwd.clone()),
            _ => None,
        }
    }

    /// Opens the `/agent` picker after refreshing cached labels for known threads.
    ///
    /// The picker state is derived from long-lived thread channels plus best-effort metadata
    /// refreshes from the backend. Refresh failures are treated as "thread is only inspectable by
    /// historical id now" and converted into closed picker entries instead of deleting them, so
    /// the stable traversal order remains intact for review and keyboard navigation.
    pub(super) async fn open_agent_picker(&mut self) {
        let process_ids: Vec<ProcessId> = self.process_event_channels.keys().cloned().collect();
        for process_id in process_ids {
            match self.server.get_process(process_id).await {
                Ok(thread) => {
                    let session_source = thread.config_snapshot().await.session_source;
                    self.upsert_agent_picker_thread(
                        process_id,
                        session_source.get_nickname(),
                        session_source.get_agent_role(),
                        /*is_closed*/ false,
                    );
                }
                Err(_) => {
                    self.mark_agent_picker_process_closed(process_id);
                }
            }
        }

        if self.agent_navigation.is_empty() {
            self.chat_widget
                .add_info_message("No agents available yet.".to_string(), /*hint*/ None);
            return;
        }

        let mut initial_selected_idx = None;
        let items: Vec<SelectionItem> = self
            .agent_navigation
            .ordered_processes()
            .iter()
            .enumerate()
            .map(|(idx, (process_id, entry))| {
                if self.active_process_id == Some(*process_id) {
                    initial_selected_idx = Some(idx);
                }
                let id = *process_id;
                let is_primary = self.primary_process_id == Some(*process_id);
                let name = format_agent_picker_item_name(
                    entry.agent_nickname.as_deref(),
                    entry.agent_role.as_deref(),
                    is_primary,
                );
                let uuid = process_id.to_string();
                SelectionItem {
                    name: name.clone(),
                    name_prefix_spans: agent_picker_status_dot_spans(entry.is_closed),
                    description: Some(uuid.clone()),
                    is_current: self.active_process_id == Some(*process_id),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::SelectAgentProcess(id));
                    })],
                    dismiss_on_select: true,
                    search_value: Some(format!("{name} {uuid}")),
                    ..Default::default()
                }
            })
            .collect();

        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Subagents".to_string()),
            subtitle: Some(AgentNavigationState::picker_subtitle()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx,
            ..Default::default()
        });
    }

    /// Updates cached picker metadata and then mirrors any visible-label change into the footer.
    ///
    /// These two writes stay paired so the picker rows and contextual footer continue to describe
    /// the same displayed thread after nickname or role updates.
    pub(super) fn upsert_agent_picker_thread(
        &mut self,
        process_id: ProcessId,
        agent_nickname: Option<String>,
        agent_role: Option<String>,
        is_closed: bool,
    ) {
        self.agent_navigation
            .upsert(process_id, agent_nickname, agent_role, is_closed);
        self.sync_active_agent_label();
    }

    /// Marks a cached picker thread closed and recomputes the contextual footer label.
    ///
    /// Closing a thread is not the same as removing it: users can still inspect finished agent
    /// transcripts, and the stable next/previous traversal order should not collapse around them.
    pub(super) fn mark_agent_picker_process_closed(&mut self, process_id: ProcessId) {
        self.agent_navigation.mark_closed(process_id);
        self.sync_active_agent_label();
    }

    async fn resolve_process_switch_target(
        &mut self,
        process_id: ProcessId,
    ) -> Option<Arc<Process>> {
        match self.server.get_process(process_id).await {
            Ok(thread) => Some(thread),
            Err(err) => {
                if self.process_event_channels.contains_key(&process_id) {
                    self.mark_agent_picker_process_closed(process_id);
                    None
                } else {
                    self.chat_widget.add_error_message(format!(
                        "Failed to attach to agent process {process_id}: {err}"
                    ));
                    None
                }
            }
        }
    }

    async fn begin_process_switch(
        &mut self,
        process_id: ProcessId,
    ) -> Option<ProcessEventSnapshot> {
        let previous_process_id = self.active_process_id;
        self.store_active_process_receiver().await;
        self.active_process_id = None;
        let Some((receiver, snapshot)) = self.activate_process_for_replay(process_id).await else {
            self.chat_widget
                .add_error_message(format!("Agent process {process_id} is already active."));
            if let Some(previous_process_id) = previous_process_id {
                self.activate_process_channel(previous_process_id).await;
            }
            return None;
        };

        self.active_process_id = Some(process_id);
        self.active_process_rx = Some(receiver);
        Some(snapshot)
    }

    fn rebuild_chat_widget_for_process_switch(
        &mut self,
        tui: &mut tui::Tui,
        live_process: Option<Arc<Process>>,
    ) {
        let init = self.chatwidget_init_for_forked_or_resumed_process(tui, self.config.clone());
        let chaos_op_tx = live_process
            .map(crate::chatwidget::spawn_op_forwarder)
            .unwrap_or_else(|| {
                // No live kernel process to forward to: hand the widget a
                // dangling channel wrapped in a no-op forwarder so .send()
                // calls quietly error against a dead receiver instead of
                // panicking.
                let (tx, _rx) = unbounded_channel();
                chaos_session::OpForwarder::from_sender(tx)
            });
        self.chat_widget = ChatWidget::new_with_op_sender(init, chaos_op_tx);
        self.sync_active_agent_label();
    }

    async fn finalize_process_switch(
        &mut self,
        tui: &mut tui::Tui,
        process_id: ProcessId,
        snapshot: ProcessEventSnapshot,
        replay_only: bool,
    ) -> Result<()> {
        self.reset_for_process_switch(tui)?;
        self.replay_process_snapshot(snapshot, !replay_only);
        if replay_only {
            self.chat_widget.add_info_message(
                format!("Agent process {process_id} is closed. Replaying saved transcript."),
                /*hint*/ None,
            );
        }
        self.drain_active_process_events(tui).await?;
        self.refresh_pending_process_approvals().await;
        Ok(())
    }

    pub(super) async fn select_agent_process(
        &mut self,
        tui: &mut tui::Tui,
        process_id: ProcessId,
    ) -> Result<()> {
        if self.active_process_id == Some(process_id) {
            return Ok(());
        }

        let live_process = self.resolve_process_switch_target(process_id).await;
        if live_process.is_none() && !self.process_event_channels.contains_key(&process_id) {
            return Ok(());
        }
        let replay_only = live_process.is_none();

        let Some(snapshot) = self.begin_process_switch(process_id).await else {
            return Ok(());
        };
        self.rebuild_chat_widget_for_process_switch(tui, live_process);
        self.finalize_process_switch(tui, process_id, snapshot, replay_only)
            .await
    }

    pub(super) fn reset_for_process_switch(&mut self, tui: &mut tui::Tui) -> Result<()> {
        self.overlay = None;
        self.transcript_cells.clear();
        self.deferred_history_lines.clear();
        self.has_emitted_history_lines = false;
        self.backtrack = BacktrackState::default();
        self.backtrack_render_pending = false;
        tui.terminal.clear_scrollback()?;
        tui.terminal.clear()?;
        Ok(())
    }
}
