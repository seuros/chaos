use super::*;

impl App {
    pub(super) fn abort_process_event_listener(&mut self, process_id: ProcessId) {
        if let Some(handle) = self.process_event_listener_tasks.remove(&process_id) {
            handle.abort();
        }
    }

    pub(super) fn abort_all_process_event_listeners(&mut self) {
        for handle in self
            .process_event_listener_tasks
            .drain()
            .map(|(_, handle)| handle)
        {
            handle.abort();
        }
    }

    pub(super) fn ensure_process_channel(
        &mut self,
        process_id: ProcessId,
    ) -> &mut ProcessEventChannel {
        self.process_event_channels
            .entry(process_id)
            .or_insert_with(|| ProcessEventChannel::new(PROCESS_EVENT_CHANNEL_CAPACITY))
    }

    pub(super) async fn set_process_active(&mut self, process_id: ProcessId, active: bool) {
        if let Some(channel) = self.process_event_channels.get_mut(&process_id) {
            let mut store = channel.store.lock().await;
            store.active = active;
        }
    }

    pub(super) async fn activate_process_channel(&mut self, process_id: ProcessId) {
        if self.active_process_id.is_some() {
            return;
        }
        self.set_process_active(process_id, /*active*/ true).await;
        let receiver = if let Some(channel) = self.process_event_channels.get_mut(&process_id) {
            channel.receiver.take()
        } else {
            None
        };
        self.active_process_id = Some(process_id);
        self.active_process_rx = receiver;
        self.refresh_pending_process_approvals().await;
    }

    pub(super) async fn store_active_process_receiver(&mut self) {
        let Some(active_id) = self.active_process_id else {
            return;
        };
        let input_state = self.chat_widget.capture_process_input_state();
        if let Some(channel) = self.process_event_channels.get_mut(&active_id) {
            let receiver = self.active_process_rx.take();
            let mut store = channel.store.lock().await;
            store.active = false;
            store.input_state = input_state;
            if let Some(receiver) = receiver {
                channel.receiver = Some(receiver);
            }
        }
    }

    pub(super) async fn activate_process_for_replay(
        &mut self,
        process_id: ProcessId,
    ) -> Option<(mpsc::Receiver<Event>, ProcessEventSnapshot)> {
        let channel = self.process_event_channels.get_mut(&process_id)?;
        let receiver = channel.receiver.take()?;
        let mut store = channel.store.lock().await;
        store.active = true;
        let snapshot = store.snapshot();
        Some((receiver, snapshot))
    }

    pub(super) async fn clear_active_thread(&mut self) {
        if let Some(active_id) = self.active_process_id.take() {
            self.set_process_active(active_id, /*active*/ false).await;
        }
        self.active_process_rx = None;
        self.refresh_pending_process_approvals().await;
    }

    pub(super) async fn note_process_outbound_op(&mut self, process_id: ProcessId, op: &Op) {
        let Some(channel) = self.process_event_channels.get(&process_id) else {
            return;
        };
        let mut store = channel.store.lock().await;
        store.note_outbound_op(op);
    }

    pub(super) async fn note_active_process_outbound_op(&mut self, op: &Op) {
        if !ProcessEventStore::op_can_change_pending_replay_state(op) {
            return;
        }
        let Some(process_id) = self.active_process_id else {
            return;
        };
        self.note_process_outbound_op(process_id, op).await;
    }

    pub(super) async fn enqueue_process_event(
        &mut self,
        process_id: ProcessId,
        event: Event,
    ) -> Result<()> {
        let refresh_pending_process_approvals =
            ProcessEventStore::event_can_change_pending_process_approvals(&event);
        let inactive_interactive_request = if self.active_process_id != Some(process_id) {
            self.interactive_request_for_process_event(process_id, &event)
                .await
        } else {
            None
        };
        let (sender, store) = {
            let channel = self.ensure_process_channel(process_id);
            (channel.sender.clone(), Arc::clone(&channel.store))
        };

        let should_send = {
            let mut guard = store.lock().await;
            guard.push_event(event.clone());
            guard.active
        };

        if should_send {
            // Never await a bounded channel send on the main TUI loop: if the receiver falls behind,
            // `send().await` can block and the UI stops drawing. If the channel is full, wait in a
            // spawned task instead.
            match sender.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    tokio::spawn(async move {
                        if let Err(err) = sender.send(event).await {
                            tracing::warn!("process {process_id} event channel closed: {err}");
                        }
                    });
                }
                Err(TrySendError::Closed(_)) => {
                    tracing::warn!("process {process_id} event channel closed");
                }
            }
        } else if let Some(request) = inactive_interactive_request {
            match request {
                ProcessInteractiveRequest::Approval(request) => {
                    self.chat_widget.push_approval_request(request);
                }
                ProcessInteractiveRequest::McpServerElicitation(request) => {
                    self.chat_widget
                        .push_mcp_server_elicitation_request(request);
                }
            }
        }
        if refresh_pending_process_approvals {
            self.refresh_pending_process_approvals().await;
        }
        Ok(())
    }

    pub(super) async fn handle_routed_process_event(
        &mut self,
        process_id: ProcessId,
        event: Event,
    ) -> Result<()> {
        if !self.process_event_channels.contains_key(&process_id) {
            tracing::debug!("dropping stale event for untracked process {process_id}");
            return Ok(());
        }

        self.enqueue_process_event(process_id, event).await
    }

    pub(super) async fn enqueue_primary_event(&mut self, event: Event) -> Result<()> {
        if let Some(process_id) = self.primary_process_id {
            return self.enqueue_process_event(process_id, event).await;
        }

        if let EventMsg::SessionConfigured(session) = &event.msg {
            let process_id = session.session_id;
            self.primary_process_id = Some(process_id);
            self.primary_session_configured = Some(session.clone());
            self.upsert_agent_picker_thread(
                process_id, /*agent_nickname*/ None, /*agent_role*/ None,
                /*is_closed*/ false,
            );
            self.ensure_process_channel(process_id);
            self.activate_process_channel(process_id).await;
            self.enqueue_process_event(process_id, event).await?;

            let pending = std::mem::take(&mut self.pending_primary_events);
            for pending_event in pending {
                self.enqueue_process_event(process_id, pending_event)
                    .await?;
            }
        } else {
            self.pending_primary_events.push_back(event);
        }
        Ok(())
    }

    pub(super) async fn drain_active_process_events(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let Some(mut rx) = self.active_process_rx.take() else {
            return Ok(());
        };

        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(event) => self.handle_codex_event_now(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if !disconnected {
            self.active_process_rx = Some(rx);
        } else {
            self.clear_active_thread().await;
        }

        if self.backtrack_render_pending {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }
}
