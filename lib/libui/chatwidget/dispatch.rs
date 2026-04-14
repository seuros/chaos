//! Slash-command dispatch for [`ChatWidget`].
//!
//! Contains `dispatch_command`, `dispatch_command_with_args`, and the rename
//! prompt helper that both dispatch paths share.
use super::*;

impl ChatWidget {
    pub(super) fn dispatch_command(&mut self, cmd: SlashCommand) {
        if !cmd.available_during_task() && self.bottom_pane.is_task_running() {
            let message = format!(
                "'/{}' is disabled while a task is in progress.",
                cmd.command()
            );
            self.add_to_history(history_cell::new_error_event(message));
            self.bottom_pane.drain_pending_submission_state();
            self.request_redraw();
            return;
        }
        match cmd {
            SlashCommand::New => {
                self.app_event_tx.send(AppEvent::NewSession);
            }
            SlashCommand::Clear => {
                self.app_event_tx.send(AppEvent::ClearUi);
            }
            SlashCommand::Resume => {
                self.app_event_tx.send(AppEvent::OpenResumePicker);
            }
            SlashCommand::Fork => {
                self.app_event_tx.send(AppEvent::ForkCurrentSession);
            }
            SlashCommand::Compact => {
                self.clear_token_usage();
                self.app_event_tx.send(AppEvent::ChaosOp(Op::Compact));
            }
            SlashCommand::Review => {
                self.open_review_popup();
            }
            SlashCommand::Rename => {
                self.session_telemetry
                    .counter("chaos.process.rename", /*inc*/ 1, &[]);
                self.show_rename_prompt();
            }
            SlashCommand::Model => {
                self.open_model_popup();
            }
            SlashCommand::Personality => {
                self.open_personality_popup();
            }
            SlashCommand::Plan => {
                if !self.collaboration_modes_enabled() {
                    self.add_info_message(
                        "Collaboration modes are disabled.".to_string(),
                        Some("Enable collaboration modes to use /plan.".to_string()),
                    );
                    return;
                }
                if let Some(mask) = collaboration_modes::plan_mask(self.models_manager.as_ref()) {
                    self.set_collaboration_mask(mask);
                } else {
                    self.add_info_message(
                        "Plan mode unavailable right now.".to_string(),
                        /*hint*/ None,
                    );
                }
            }
            SlashCommand::Collab => {
                if !self.collaboration_modes_enabled() {
                    self.add_info_message(
                        "Collaboration modes are disabled.".to_string(),
                        Some("Enable collaboration modes to use /collab.".to_string()),
                    );
                    return;
                }
                self.open_collaboration_modes_popup();
            }
            SlashCommand::Agent | SlashCommand::MultiAgents => {
                self.app_event_tx.send(AppEvent::OpenAgentPicker);
            }
            SlashCommand::Approvals => {
                self.open_permissions_popup();
            }
            SlashCommand::Permissions => {
                self.open_permissions_popup();
            }
            SlashCommand::ElevateSandbox => {
                // Not supported on Linux.
            }
            SlashCommand::SandboxReadRoot => {
                self.add_error_message(
                    "Usage: /sandbox-add-read-dir <absolute-directory-path>".to_string(),
                );
            }
            SlashCommand::Quit | SlashCommand::Exit => {
                self.request_quit_without_confirmation();
            }
            SlashCommand::Login => {
                self.app_event_tx.send(AppEvent::OpenLoginPopup);
            }
            SlashCommand::Logout => {
                if let Err(e) = chaos_kern::auth::logout(
                    &self.config.chaos_home,
                    self.config.cli_auth_credentials_store_mode,
                ) {
                    tracing::error!("failed to logout: {e}");
                }
                self.request_quit_without_confirmation();
            }
            SlashCommand::Diff => {
                self.add_diff_in_progress();
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let text = match get_git_diff().await {
                        Ok((is_git_repo, diff_text)) => {
                            if is_git_repo {
                                diff_text
                            } else {
                                "`/diff` — _not inside a git repository_".to_string()
                            }
                        }
                        Err(e) => format!("Failed to compute diff: {e}"),
                    };
                    tx.send(AppEvent::DiffResult(text));
                });
            }
            SlashCommand::Copy => {
                let Some(text) = self.last_copyable_output.as_deref() else {
                    self.add_info_message(
                        "`/copy` is unavailable before the first Chaos output or right after a rollback."
                            .to_string(),
                        /*hint*/ None,
                    );
                    return;
                };

                let copy_result = clipboard_text::copy_text_to_clipboard(text);

                match copy_result {
                    Ok(()) => {
                        let hint = self.agent_turn_running.then_some(
                            "Current turn is still running; copied the latest completed output (not the in-progress response)."
                                .to_string(),
                        );
                        self.add_info_message(
                            "Copied latest Chaos output to clipboard.".to_string(),
                            hint,
                        );
                    }
                    Err(err) => {
                        self.add_error_message(format!("Failed to copy to clipboard: {err}"))
                    }
                }
            }
            SlashCommand::Mention => {
                self.insert_str("@");
            }
            SlashCommand::Status => {
                self.add_status_output();
            }
            SlashCommand::DebugConfig => {
                self.add_debug_config_output();
            }
            SlashCommand::Statusline => {
                self.open_status_line_setup();
            }
            SlashCommand::Theme => {
                self.open_theme_picker();
            }
            SlashCommand::Ps => {
                self.add_ps_output();
            }
            SlashCommand::Stop => {
                self.clean_background_terminals();
            }
            SlashCommand::MemoryDrop => {
                self.submit_op(Op::DropMemories);
            }
            SlashCommand::MemoryUpdate => {
                self.submit_op(Op::UpdateMemories);
            }
            SlashCommand::Mcp => {
                self.add_mcp_output();
            }
            SlashCommand::McpAdd => {
                self.open_mcp_add_form();
            }
            SlashCommand::Tools => {
                self.submit_op(Op::ListAllTools);
            }
            SlashCommand::Clamp => {
                // Toggle clamped mode — Claude Code subprocess as transport.
                let is_clamped = crate::theme::is_clamped();
                let new_state = !is_clamped;
                crate::theme::set_clamped(new_state);
                self.app_event_tx
                    .send(AppEvent::ChaosOp(Op::SetClamped { enabled: new_state }));
                if new_state {
                    // Save the current direct-API selection before clamping so we can
                    // restore it when switching transports back off.
                    self.capture_pre_clamp_selection();
                    self.add_info_message(
                        "Clamped: using Claude Code MAX subscription as transport.".to_string(),
                        Some("Type /clamp again to switch back".to_string()),
                    );
                } else {
                    self.restore_pre_clamp_selection();
                    self.add_info_message(
                        "Unclamped: using direct API transport.".to_string(),
                        None,
                    );
                    // Force a full screen repaint so the green theme takes effect
                    // on all chrome (borders, status bar, input prompt).
                }
            }
            SlashCommand::TestApproval => {
                use chaos_ipc::protocol::EventMsg;
                use std::collections::HashMap;

                use chaos_ipc::protocol::ApplyPatchApprovalRequestEvent;
                use chaos_ipc::protocol::FileChange;

                self.app_event_tx.send(AppEvent::ChaosEvent(Event {
                    id: "1".to_string(),
                    // msg: EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
                    //     call_id: "1".to_string(),
                    //     command: vec!["git".into(), "apply".into()],
                    //     cwd: self.config.cwd.clone(),
                    //     reason: Some("test".to_string()),
                    // }),
                    msg: EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
                        call_id: "1".to_string(),
                        turn_id: "turn-1".to_string(),
                        changes: HashMap::from([
                            (
                                PathBuf::from("/tmp/test.txt"),
                                FileChange::Add {
                                    content: "test".to_string(),
                                },
                            ),
                            (
                                PathBuf::from("/tmp/test2.txt"),
                                FileChange::Update {
                                    unified_diff: "+test\n-test2".to_string(),
                                    move_path: None,
                                },
                            ),
                        ]),
                        reason: None,
                        grant_root: Some(PathBuf::from("/tmp")),
                    }),
                }));
            }
        }
    }

    pub(super) fn dispatch_command_with_args(
        &mut self,
        cmd: SlashCommand,
        args: String,
        _text_elements: Vec<TextElement>,
    ) {
        if !cmd.supports_inline_args() {
            self.dispatch_command(cmd);
            return;
        }
        if !cmd.available_during_task() && self.bottom_pane.is_task_running() {
            let message = format!(
                "'/{}' is disabled while a task is in progress.",
                cmd.command()
            );
            self.add_to_history(history_cell::new_error_event(message));
            self.request_redraw();
            return;
        }

        let trimmed = args.trim();
        match cmd {
            SlashCommand::Rename if !trimmed.is_empty() => {
                self.session_telemetry
                    .counter("chaos.process.rename", /*inc*/ 1, &[]);
                let Some((prepared_args, _prepared_elements)) = self
                    .bottom_pane
                    .prepare_inline_args_submission(/*record_history*/ false)
                else {
                    return;
                };
                let Some(name) = chaos_kern::util::normalize_process_name(&prepared_args) else {
                    self.add_error_message("Process name cannot be empty.".to_string());
                    return;
                };
                let cell = Self::rename_confirmation_cell(&name, self.process_id);
                self.add_boxed_history(Box::new(cell));
                self.request_redraw();
                self.app_event_tx
                    .send(AppEvent::ChaosOp(Op::SetProcessName { name }));
                self.bottom_pane.drain_pending_submission_state();
            }
            SlashCommand::Plan if !trimmed.is_empty() => {
                self.dispatch_command(cmd);
                if self.active_mode_kind() != ModeKind::Plan {
                    return;
                }
                let Some((prepared_args, prepared_elements)) = self
                    .bottom_pane
                    .prepare_inline_args_submission(/*record_history*/ true)
                else {
                    return;
                };
                let local_images = self
                    .bottom_pane
                    .take_recent_submission_images_with_placeholders();
                let remote_image_urls = self.take_remote_image_urls();
                let user_message = UserMessage {
                    text: prepared_args,
                    local_images,
                    remote_image_urls,
                    text_elements: prepared_elements,
                    mention_bindings: self.bottom_pane.take_recent_submission_mention_bindings(),
                };
                if self.is_session_configured() {
                    self.reasoning_buffer.clear();
                    self.full_reasoning_buffer.clear();
                    self.set_status_header(String::from("Working"));
                    self.submit_user_message(user_message);
                } else {
                    self.queue_user_message(user_message);
                }
            }
            SlashCommand::Review if !trimmed.is_empty() => {
                let Some((prepared_args, _prepared_elements)) = self
                    .bottom_pane
                    .prepare_inline_args_submission(/*record_history*/ false)
                else {
                    return;
                };
                self.submit_op(Op::Review {
                    review_request: ReviewRequest {
                        target: ReviewTarget::Custom {
                            instructions: prepared_args,
                        },
                        user_facing_hint: None,
                    },
                });
                self.bottom_pane.drain_pending_submission_state();
            }
            _ => self.dispatch_command(cmd),
        }
    }

    pub(super) fn show_rename_prompt(&mut self) {
        let tx = self.app_event_tx.clone();
        let has_name = self
            .process_name
            .as_ref()
            .is_some_and(|name| !name.is_empty());
        let title = if has_name {
            "Rename process"
        } else {
            "Name process"
        };
        let process_id = self.process_id;
        let view = CustomPromptView::new(
            title.to_string(),
            "Type a name and press Enter".to_string(),
            /*context_label*/ None,
            Box::new(move |name: String| {
                let Some(name) = chaos_kern::util::normalize_process_name(&name) else {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event("Process name cannot be empty.".to_string()),
                    )));
                    return;
                };
                let cell = Self::rename_confirmation_cell(&name, process_id);
                tx.send(AppEvent::InsertHistoryCell(Box::new(cell)));
                tx.send(AppEvent::ChaosOp(Op::SetProcessName { name }));
            }),
        );

        self.bottom_pane.show_view(Box::new(view));
    }
}
