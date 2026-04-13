//! Draft and submission flow: plan prompts, message restoration, and process
//! state capture/restore helpers.
use super::super::*;

impl ChatWidget {
    pub(crate) fn emit_forked_process_event(&self, forked_from_id: ProcessId) {
        let app_event_tx = self.app_event_tx.clone();
        let chaos_home = self.config.chaos_home.clone();
        tokio::spawn(async move {
            let forked_from_id_text = forked_from_id.to_string();
            let send_name_and_id = |name: String| {
                let line: Line<'static> = vec![
                    "• ".dim(),
                    "Process forked from ".into(),
                    name.cyan(),
                    " (".into(),
                    forked_from_id_text.clone().cyan(),
                    ")".into(),
                ]
                .into();
                app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    PlainHistoryCell::new(vec![line]),
                )));
            };
            let send_id_only = || {
                let line: Line<'static> = vec![
                    "• ".dim(),
                    "Process forked from ".into(),
                    forked_from_id_text.clone().cyan(),
                ]
                .into();
                app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    PlainHistoryCell::new(vec![line]),
                )));
            };

            match find_process_name_by_id(&chaos_home, &forked_from_id).await {
                Ok(Some(name)) if !name.trim().is_empty() => {
                    send_name_and_id(name);
                }
                Ok(_) => send_id_only(),
                Err(err) => {
                    tracing::warn!("Failed to read forked process name: {err}");
                    send_id_only();
                }
            }
        });
    }

    pub fn open_app_link_view(&mut self, params: crate::bottom_pane::AppLinkViewParams) {
        let view = crate::bottom_pane::AppLinkView::new(params, self.app_event_tx.clone());
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }

    // Raw reasoning uses the same flow as summarized reasoning

    pub(crate) fn maybe_prompt_plan_implementation(&mut self) {
        if !self.collaboration_modes_enabled() {
            return;
        }
        if !self.queued_user_messages.is_empty() {
            return;
        }
        if self.active_mode_kind() != ModeKind::Plan {
            return;
        }
        if !self.saw_plan_item_this_turn {
            return;
        }
        if !self.bottom_pane.no_modal_or_popup_active() {
            return;
        }

        if matches!(
            self.rate_limit_switch_prompt,
            RateLimitSwitchPromptState::Pending
        ) {
            return;
        }

        self.open_plan_implementation_prompt();
    }

    pub(crate) fn open_plan_implementation_prompt(&mut self) {
        let default_mask = collaboration_modes::default_mode_mask(self.models_manager.as_ref());
        let (implement_actions, implement_disabled_reason) = match default_mask {
            Some(mask) => {
                let user_text = PLAN_IMPLEMENTATION_CODING_MESSAGE.to_string();
                let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                    tx.send(AppEvent::SubmitUserMessageWithMode {
                        text: user_text.clone(),
                        collaboration_mode: mask.clone(),
                    });
                })];
                (actions, None)
            }
            None => (Vec::new(), Some("Default mode unavailable".to_string())),
        };
        let items = vec![
            SelectionItem {
                name: PLAN_IMPLEMENTATION_YES.to_string(),
                description: Some("Switch to Default and start coding.".to_string()),
                selected_description: None,
                is_current: false,
                actions: implement_actions,
                disabled_reason: implement_disabled_reason,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: PLAN_IMPLEMENTATION_NO.to_string(),
                description: Some("Continue planning with the model.".to_string()),
                selected_description: None,
                is_current: false,
                actions: Vec::new(),
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(PLAN_IMPLEMENTATION_TITLE.to_string()),
            subtitle: None,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
        self.notify(Notification::PlanModePrompt {
            title: PLAN_IMPLEMENTATION_TITLE.to_string(),
        });
    }

    /// Merge pending steers, queued drafts, and the current composer state into a single message.
    ///
    /// Each pending message numbers attachments from `[Image #1]` relative to its own remote
    /// images. When we concatenate multiple messages after interrupt, we must renumber local-image
    /// placeholders in a stable order and rebase text element byte ranges so the restored composer
    /// state stays aligned with the merged attachment list. Returns `None` when there is nothing to
    /// restore.
    pub(crate) fn drain_pending_messages_for_restore(&mut self) -> Option<UserMessage> {
        if self.pending_steers.is_empty() && self.queued_user_messages.is_empty() {
            return None;
        }

        let existing_message = UserMessage {
            text: self.bottom_pane.composer_text(),
            text_elements: self.bottom_pane.composer_text_elements(),
            local_images: self.bottom_pane.composer_local_images(),
            remote_image_urls: self.bottom_pane.remote_image_urls(),
            mention_bindings: self.bottom_pane.composer_mention_bindings(),
        };

        let mut to_merge: Vec<UserMessage> = self
            .pending_steers
            .drain(..)
            .map(|steer| steer.user_message)
            .collect();
        to_merge.extend(self.queued_user_messages.drain(..));
        if !existing_message.text.is_empty()
            || !existing_message.local_images.is_empty()
            || !existing_message.remote_image_urls.is_empty()
        {
            to_merge.push(existing_message);
        }

        Some(merge_user_messages(to_merge))
    }

    pub(crate) fn restore_user_message_to_composer(&mut self, user_message: UserMessage) {
        let UserMessage {
            text,
            local_images,
            remote_image_urls,
            text_elements,
            mention_bindings,
        } = user_message;
        let local_image_paths = local_images.into_iter().map(|img| img.path).collect();
        self.set_remote_image_urls(remote_image_urls);
        self.bottom_pane.set_composer_text_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
        );
    }

    pub fn capture_process_input_state(&self) -> Option<ProcessInputState> {
        let composer = ProcessComposerState {
            text: self.bottom_pane.composer_text(),
            text_elements: self.bottom_pane.composer_text_elements(),
            local_images: self.bottom_pane.composer_local_images(),
            remote_image_urls: self.bottom_pane.remote_image_urls(),
            mention_bindings: self.bottom_pane.composer_mention_bindings(),
            pending_pastes: self.bottom_pane.composer_pending_pastes(),
        };
        Some(ProcessInputState {
            composer: composer.has_content().then_some(composer),
            pending_steers: self
                .pending_steers
                .iter()
                .map(|pending| pending.user_message.clone())
                .collect(),
            queued_user_messages: self.queued_user_messages.clone(),
            current_collaboration_mode: self.current_collaboration_mode.clone(),
            active_collaboration_mask: self.active_collaboration_mask.clone(),
            agent_turn_running: self.agent_turn_running,
        })
    }

    pub fn restore_process_input_state(&mut self, input_state: Option<ProcessInputState>) {
        if let Some(input_state) = input_state {
            self.current_collaboration_mode = input_state.current_collaboration_mode;
            self.active_collaboration_mask = input_state.active_collaboration_mask;
            self.agent_turn_running = input_state.agent_turn_running;
            self.update_collaboration_mode_indicator();
            self.refresh_model_display();
            if let Some(composer) = input_state.composer {
                let local_image_paths = composer
                    .local_images
                    .into_iter()
                    .map(|img| img.path)
                    .collect();
                self.set_remote_image_urls(composer.remote_image_urls);
                self.bottom_pane.set_composer_text_with_mention_bindings(
                    composer.text,
                    composer.text_elements,
                    local_image_paths,
                    composer.mention_bindings,
                );
                self.bottom_pane
                    .set_composer_pending_pastes(composer.pending_pastes);
            } else {
                self.set_remote_image_urls(Vec::new());
                self.bottom_pane.set_composer_text_with_mention_bindings(
                    String::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                );
                self.bottom_pane.set_composer_pending_pastes(Vec::new());
            }
            self.pending_steers.clear();
            self.queued_user_messages = input_state.pending_steers;
            self.queued_user_messages
                .extend(input_state.queued_user_messages);
        } else {
            self.agent_turn_running = false;
            self.pending_steers.clear();
            self.set_remote_image_urls(Vec::new());
            self.bottom_pane.set_composer_text_with_mention_bindings(
                String::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            self.bottom_pane.set_composer_pending_pastes(Vec::new());
            self.queued_user_messages.clear();
        }
        self.turn_sleep_inhibitor
            .set_turn_running(self.agent_turn_running);
        self.update_task_running_state();
        self.refresh_pending_input_preview();
        self.request_redraw();
    }
}
