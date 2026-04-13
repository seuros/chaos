use chaos_ipc::items::TurnItem;
use chaos_ipc::num_format::format_with_separators;
use chaos_ipc::plan_tool::StepStatus;
use chaos_ipc::plan_tool::UpdatePlanArgs;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_ipc::protocol::AgentMessageEvent;
use chaos_ipc::protocol::AgentReasoningRawContentEvent;
use chaos_ipc::protocol::BackgroundEventEvent;
use chaos_ipc::protocol::CollabAgentInteractionBeginEvent;
use chaos_ipc::protocol::CollabAgentInteractionEndEvent;
use chaos_ipc::protocol::CollabAgentSpawnBeginEvent;
use chaos_ipc::protocol::CollabAgentSpawnEndEvent;
use chaos_ipc::protocol::CollabCloseBeginEvent;
use chaos_ipc::protocol::CollabCloseEndEvent;
use chaos_ipc::protocol::CollabWaitingBeginEvent;
use chaos_ipc::protocol::DeprecationNoticeEvent;
use chaos_ipc::protocol::ErrorEvent;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ExecCommandBeginEvent;
use chaos_ipc::protocol::ExecCommandEndEvent;
use chaos_ipc::protocol::FileChange;
use chaos_ipc::protocol::ItemCompletedEvent;
use chaos_ipc::protocol::McpToolCallBeginEvent;
use chaos_ipc::protocol::McpToolCallEndEvent;
use chaos_ipc::protocol::PatchApplyBeginEvent;
use chaos_ipc::protocol::PatchApplyEndEvent;
use chaos_ipc::protocol::SessionConfiguredEvent;
use chaos_ipc::protocol::StreamErrorEvent;
use chaos_ipc::protocol::TurnAbortReason;
use chaos_ipc::protocol::TurnCompleteEvent;
use chaos_ipc::protocol::TurnDiffEvent;
use chaos_ipc::protocol::WarningEvent;
use chaos_ipc::protocol::WebSearchEndEvent;
use chaos_jail_report::create_config_summary_entries;
use chaos_kern::config::Config;
use chaos_kern::web_search::web_search_detail;
use chaos_uptime::format_duration;
use chaos_uptime::format_elapsed;
use owo_colors::OwoColorize;
use std::io::IsTerminal;

use crate::event_processor::ChaosStatus;
use crate::event_processor::EventProcessor;
use crate::event_processor::handle_last_message;

use super::EventProcessorWithHumanOutput;
use super::MAX_OUTPUT_LINES_FOR_EXEC_TOOL_CALL;
use super::PatchApplyBegin;
use super::helpers::should_print_final_message_to_stdout;
use super::rendering::escape_command;
use super::rendering::format_collab_invocation;
use super::rendering::format_collab_status;
use super::rendering::format_file_change;
use super::rendering::format_mcp_invocation;
use super::rendering::format_receiver_list;
use super::rendering::is_collab_status_failure;
use super::rendering::style_for_agent_status;

/// Timestamped helper. The timestamp is styled with self.dimmed.
macro_rules! ts_msg {
    ($self:ident, $($arg:tt)*) => {{
        eprintln!($($arg)*);
    }};
}

impl EventProcessor for EventProcessorWithHumanOutput {
    /// Print a concise summary of the effective configuration that will be used
    /// for the session. This mirrors the information shown in the TUI welcome
    /// screen.
    fn print_config_summary(
        &mut self,
        config: &Config,
        prompt: &str,
        session_configured_event: &SessionConfiguredEvent,
    ) {
        ts_msg!(self, "Chaos v{}\n--------", CHAOS_VERSION);

        let mut entries =
            create_config_summary_entries(config, session_configured_event.model.as_str());
        entries.push((
            "session id",
            session_configured_event.session_id.to_string(),
        ));

        for (key, value) in entries {
            eprintln!("{} {}", format!("{key}:").style(self.bold), value);
        }

        eprintln!("--------");

        // Echo the prompt that will be sent to the agent so it is visible in the
        // transcript/logs before any events come in. Note the prompt may have been
        // read from stdin, so it may not be visible in the terminal otherwise.
        ts_msg!(self, "{}\n{}", "user".style(self.cyan), prompt);
    }

    fn process_event(&mut self, event: Event) -> ChaosStatus {
        let Event { id: _, msg } = event;
        if let EventMsg::BackgroundEvent(BackgroundEventEvent { message }) = &msg
            && let Some(update) = Self::parse_agent_job_progress(message)
        {
            self.render_agent_job_progress(update);
            return ChaosStatus::Running;
        }
        if self.progress_active && !Self::should_interrupt_progress(&msg) {
            return ChaosStatus::Running;
        }
        if !Self::is_silent_event(&msg) {
            self.finish_progress_line();
        }
        match msg {
            EventMsg::Error(ErrorEvent { message, .. }) => {
                let prefix = "ERROR:".style(self.red);
                ts_msg!(self, "{prefix} {message}");
            }
            EventMsg::Warning(WarningEvent { message }) => {
                ts_msg!(
                    self,
                    "{} {message}",
                    "warning:".style(self.yellow).style(self.bold)
                );
            }

            EventMsg::ModelReroute(_) => {}
            EventMsg::DeprecationNotice(DeprecationNoticeEvent { summary, details }) => {
                ts_msg!(
                    self,
                    "{} {summary}",
                    "deprecated:".style(self.magenta).style(self.bold)
                );
                if let Some(details) = details {
                    ts_msg!(self, "  {}", details.style(self.dimmed));
                }
            }
            EventMsg::McpStartupUpdate(update) => {
                let status_text = match update.status {
                    chaos_ipc::protocol::McpStartupStatus::Starting => "starting".to_string(),
                    chaos_ipc::protocol::McpStartupStatus::Ready => "ready".to_string(),
                    chaos_ipc::protocol::McpStartupStatus::Cancelled => "cancelled".to_string(),
                    chaos_ipc::protocol::McpStartupStatus::Failed { ref error } => {
                        format!("failed: {error}")
                    }
                };
                ts_msg!(
                    self,
                    "{} {} {}",
                    "mcp:".style(self.cyan),
                    update.server,
                    status_text
                );
            }
            EventMsg::McpStartupComplete(summary) => {
                let mut parts = Vec::new();
                if !summary.ready.is_empty() {
                    parts.push(format!("ready: {}", summary.ready.join(", ")));
                }
                if !summary.failed.is_empty() {
                    let servers: Vec<_> = summary.failed.iter().map(|f| f.server.clone()).collect();
                    parts.push(format!("failed: {}", servers.join(", ")));
                }
                if !summary.cancelled.is_empty() {
                    parts.push(format!("cancelled: {}", summary.cancelled.join(", ")));
                }
                let joined = if parts.is_empty() {
                    "no servers".to_string()
                } else {
                    parts.join("; ")
                };
                ts_msg!(self, "{} {}", "mcp startup:".style(self.cyan), joined);
            }
            EventMsg::BackgroundEvent(BackgroundEventEvent { message }) => {
                ts_msg!(self, "{}", message.style(self.dimmed));
            }
            EventMsg::StreamError(StreamErrorEvent {
                message,
                additional_details,
                ..
            }) => {
                let message = match additional_details {
                    Some(details) if !details.trim().is_empty() => format!("{message} ({details})"),
                    _ => message,
                };
                ts_msg!(self, "{}", message.style(self.dimmed));
            }
            EventMsg::TurnStarted(_) => {
                // Ignore.
            }
            EventMsg::ElicitationRequest(ev) => {
                ts_msg!(
                    self,
                    "{} {}",
                    "elicitation request".style(self.magenta),
                    ev.server_name.style(self.dimmed)
                );
                ts_msg!(
                    self,
                    "{}",
                    "auto-cancelling (not supported in exec mode)".style(self.dimmed)
                );
            }
            EventMsg::ElicitationComplete(_) => {}
            EventMsg::TurnComplete(TurnCompleteEvent {
                last_agent_message, ..
            }) => {
                let last_message = last_agent_message
                    .as_deref()
                    .or(self.last_proposed_plan.as_deref());
                if let Some(output_file) = self.last_message_path.as_deref() {
                    handle_last_message(last_message, output_file);
                }

                self.final_message = last_agent_message.or_else(|| self.last_proposed_plan.clone());

                return ChaosStatus::InitiateShutdown;
            }
            EventMsg::TokenCount(ev) => {
                self.last_total_token_usage = ev.info;
            }

            EventMsg::AgentReasoningSectionBreak(_) => {
                if !self.show_agent_reasoning {
                    return ChaosStatus::Running;
                }
                eprintln!();
            }
            EventMsg::AgentReasoningRawContent(AgentReasoningRawContentEvent { text }) => {
                if self.show_raw_agent_reasoning {
                    ts_msg!(
                        self,
                        "{}\n{}",
                        "thinking".style(self.italic).style(self.magenta),
                        text,
                    );
                }
            }
            EventMsg::AgentMessage(AgentMessageEvent { message, .. }) => {
                ts_msg!(
                    self,
                    "{}\n{}",
                    "chaos".style(self.italic).style(self.magenta),
                    message,
                );
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::Plan(item),
                ..
            }) => {
                self.last_proposed_plan = Some(item.text);
            }
            EventMsg::ExecCommandBegin(ExecCommandBeginEvent { command, cwd, .. }) => {
                eprint!(
                    "{}\n{} in {}",
                    "exec".style(self.italic).style(self.magenta),
                    escape_command(&command).style(self.bold),
                    cwd.to_string_lossy(),
                );
            }
            EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                aggregated_output,
                duration,
                exit_code,
                ..
            }) => {
                let duration = format!(" in {}", format_duration(duration));

                let truncated_output = aggregated_output
                    .lines()
                    .take(MAX_OUTPUT_LINES_FOR_EXEC_TOOL_CALL)
                    .collect::<Vec<_>>()
                    .join("\n");
                match exit_code {
                    0 => {
                        let title = format!(" succeeded{duration}:");
                        ts_msg!(self, "{}", title.style(self.green));
                    }
                    _ => {
                        let title = format!(" exited {exit_code}{duration}:");
                        ts_msg!(self, "{}", title.style(self.red));
                    }
                }
                eprintln!("{}", truncated_output.style(self.dimmed));
            }
            EventMsg::McpToolCallBegin(McpToolCallBeginEvent {
                call_id: _,
                invocation,
            }) => {
                ts_msg!(
                    self,
                    "{} {}",
                    "tool".style(self.magenta),
                    format_mcp_invocation(&invocation).style(self.bold),
                );
            }
            EventMsg::McpToolCallEnd(tool_call_end_event) => {
                let is_success = tool_call_end_event.is_success();
                let McpToolCallEndEvent {
                    call_id: _,
                    result,
                    invocation,
                    duration,
                } = tool_call_end_event;

                let duration = format!(" in {}", format_duration(duration));

                let status_str = if is_success { "success" } else { "failed" };
                let title_style = if is_success { self.green } else { self.red };
                let title = format!(
                    "{} {status_str}{duration}:",
                    format_mcp_invocation(&invocation)
                );

                ts_msg!(self, "{}", title.style(title_style));

                if let Ok(res) = result {
                    let val = serde_json::to_value(res)
                        .unwrap_or_else(|_| serde_json::Value::String("<result>".to_string()));
                    let pretty =
                        serde_json::to_string_pretty(&val).unwrap_or_else(|_| val.to_string());

                    for line in pretty.lines().take(MAX_OUTPUT_LINES_FOR_EXEC_TOOL_CALL) {
                        eprintln!("{}", line.style(self.dimmed));
                    }
                }
            }
            EventMsg::WebSearchBegin(_) => {
                ts_msg!(self, "🌐 Searching the web...");
            }
            EventMsg::WebSearchEnd(WebSearchEndEvent {
                call_id: _,
                query,
                action,
            }) => {
                let detail = web_search_detail(Some(&action), &query);
                if detail.is_empty() {
                    ts_msg!(self, "🌐 Searched the web");
                } else {
                    ts_msg!(self, "🌐 Searched: {detail}");
                }
            }
            EventMsg::ImageGenerationBegin(generated) => {
                ts_msg!(
                    self,
                    "{} {}",
                    "image generation started".style(self.magenta),
                    generated.call_id
                );
            }
            EventMsg::ImageGenerationEnd(generated) => {
                if !generated.result.is_empty()
                    && !generated.result.starts_with("data:")
                    && !generated.result.starts_with("http://")
                    && !generated.result.starts_with("https://")
                    && !generated.result.starts_with("file://")
                {
                    ts_msg!(
                        self,
                        "{} {} {}",
                        "generated image".style(self.magenta),
                        generated.call_id,
                        generated.result.style(self.dimmed)
                    );
                } else {
                    ts_msg!(
                        self,
                        "{} {}",
                        "generated image".style(self.magenta),
                        generated.call_id
                    );
                }
            }
            EventMsg::PatchApplyBegin(PatchApplyBeginEvent {
                call_id,
                auto_approved,
                changes,
                ..
            }) => {
                // Store metadata so we can calculate duration later when we
                // receive the corresponding PatchApplyEnd event.
                self.call_id_to_patch.insert(
                    call_id,
                    PatchApplyBegin {
                        start_time: std::time::Instant::now(),
                        auto_approved,
                    },
                );

                ts_msg!(
                    self,
                    "{}",
                    "file update".style(self.magenta).style(self.italic),
                );

                // Pretty-print the patch summary with colored diff markers so
                // it's easy to scan in the terminal output.
                for (path, change) in changes.iter() {
                    match change {
                        FileChange::Add { content } => {
                            let header = format!(
                                "{} {}",
                                format_file_change(change),
                                path.to_string_lossy()
                            );
                            eprintln!("{}", header.style(self.magenta));
                            for line in content.lines() {
                                eprintln!("{}", line.style(self.green));
                            }
                        }
                        FileChange::Delete { content } => {
                            let header = format!(
                                "{} {}",
                                format_file_change(change),
                                path.to_string_lossy()
                            );
                            eprintln!("{}", header.style(self.magenta));
                            for line in content.lines() {
                                eprintln!("{}", line.style(self.red));
                            }
                        }
                        FileChange::Update {
                            unified_diff,
                            move_path,
                        } => {
                            let header = if let Some(dest) = move_path {
                                format!(
                                    "{} {} -> {}",
                                    format_file_change(change),
                                    path.to_string_lossy(),
                                    dest.to_string_lossy()
                                )
                            } else {
                                format!("{} {}", format_file_change(change), path.to_string_lossy())
                            };
                            eprintln!("{}", header.style(self.magenta));

                            // Colorize diff lines. We keep file header lines
                            // (--- / +++) without extra coloring so they are
                            // still readable.
                            for diff_line in unified_diff.lines() {
                                if diff_line.starts_with('+') && !diff_line.starts_with("+++") {
                                    eprintln!("{}", diff_line.style(self.green));
                                } else if diff_line.starts_with('-')
                                    && !diff_line.starts_with("---")
                                {
                                    eprintln!("{}", diff_line.style(self.red));
                                } else {
                                    eprintln!("{diff_line}");
                                }
                            }
                        }
                    }
                }
            }
            EventMsg::PatchApplyEnd(PatchApplyEndEvent {
                call_id,
                stdout,
                stderr,
                success,
                ..
            }) => {
                let patch_begin = self.call_id_to_patch.remove(&call_id);

                // Compute duration and summary label similar to exec commands.
                let (duration, label) = if let Some(PatchApplyBegin {
                    start_time,
                    auto_approved,
                }) = patch_begin
                {
                    (
                        format!(" in {}", format_elapsed(start_time)),
                        format!("apply_patch(auto_approved={auto_approved})"),
                    )
                } else {
                    (String::new(), format!("apply_patch('{call_id}')"))
                };

                let (exit_code, output, title_style) = if success {
                    (0, stdout, self.green)
                } else {
                    (1, stderr, self.red)
                };

                let title = format!("{label} exited {exit_code}{duration}:");
                ts_msg!(self, "{}", title.style(title_style));
                for line in output.lines() {
                    eprintln!("{}", line.style(self.dimmed));
                }
            }
            EventMsg::TurnDiff(TurnDiffEvent { unified_diff }) => {
                ts_msg!(
                    self,
                    "{}",
                    "file update:".style(self.magenta).style(self.italic)
                );
                eprintln!("{unified_diff}");
            }
            EventMsg::AgentReasoning(agent_reasoning_event) => {
                if self.show_agent_reasoning {
                    ts_msg!(
                        self,
                        "{}\n{}",
                        "thinking".style(self.italic).style(self.magenta),
                        agent_reasoning_event.text,
                    );
                }
            }
            EventMsg::SessionConfigured(session_configured_event) => {
                let SessionConfiguredEvent {
                    session_id: conversation_id,
                    model,
                    ..
                } = session_configured_event;

                ts_msg!(
                    self,
                    "{} {}",
                    "chaos session".style(self.magenta).style(self.bold),
                    conversation_id.to_string().style(self.dimmed)
                );

                ts_msg!(self, "model: {}", model);
                eprintln!();
            }
            EventMsg::PlanUpdate(plan_update_event) => {
                let UpdatePlanArgs { explanation, plan } = plan_update_event;

                // Header
                ts_msg!(self, "{}", "Plan update".style(self.magenta));

                // Optional explanation
                if let Some(explanation) = explanation
                    && !explanation.trim().is_empty()
                {
                    ts_msg!(self, "{}", explanation.style(self.italic));
                }

                // Pretty-print the plan items with simple status markers.
                for item in plan {
                    match item.status {
                        StepStatus::Completed => {
                            ts_msg!(self, "  {} {}", "✓".style(self.green), item.step);
                        }
                        StepStatus::InProgress => {
                            ts_msg!(self, "  {} {}", "→".style(self.cyan), item.step);
                        }
                        StepStatus::Pending => {
                            ts_msg!(
                                self,
                                "  {} {}",
                                "•".style(self.dimmed),
                                item.step.style(self.dimmed)
                            );
                        }
                    }
                }
            }
            EventMsg::ViewImageToolCall(view) => {
                ts_msg!(
                    self,
                    "{} {}",
                    "viewed image".style(self.magenta),
                    view.path.display()
                );
            }
            EventMsg::TurnAborted(abort_reason) => {
                match abort_reason.reason {
                    TurnAbortReason::Interrupted => {
                        ts_msg!(self, "task interrupted");
                    }
                    TurnAbortReason::Replaced => {
                        ts_msg!(self, "task aborted: replaced by a new task");
                    }
                    TurnAbortReason::ReviewEnded => {
                        ts_msg!(self, "task aborted: review ended");
                    }
                }
                return ChaosStatus::InitiateShutdown;
            }
            EventMsg::ContextCompacted(_) => {
                ts_msg!(self, "context compacted");
            }
            EventMsg::CollabAgentSpawnBegin(CollabAgentSpawnBeginEvent {
                call_id,
                sender_process_id: _,
                prompt,
                catchphrase,
                missing_topics,
                ..
            }) => {
                ts_msg!(
                    self,
                    "{} {}",
                    "collab".style(self.magenta),
                    format_collab_invocation("spawn_agent", &call_id, Some(&prompt))
                        .style(self.bold)
                );
                if let Some(phrase) = catchphrase {
                    eprintln!("{}", format!("  ⚡ {phrase}").style(self.bold_yellow));
                }
                for topic in &missing_topics {
                    eprintln!(
                        "{}",
                        format!(
                            "  ⚠ no role registered for topic '{topic}' — spawning default (consider reporting this topic as missing)"
                        )
                        .style(self.yellow)
                    );
                }
            }
            EventMsg::CollabAgentSpawnEnd(CollabAgentSpawnEndEvent {
                call_id,
                sender_process_id: _,
                new_process_id,
                prompt,
                status,
                ..
            }) => {
                let success = new_process_id.is_some() && !is_collab_status_failure(&status);
                let title_style = if success { self.green } else { self.red };
                let title = format!(
                    "{} {}:",
                    format_collab_invocation("spawn_agent", &call_id, Some(&prompt)),
                    format_collab_status(&status)
                );
                ts_msg!(self, "{}", title.style(title_style));
                if let Some(new_process_id) = new_process_id {
                    eprintln!("  agent: {}", new_process_id.to_string().style(self.dimmed));
                }
            }
            EventMsg::CollabAgentInteractionBegin(CollabAgentInteractionBeginEvent {
                call_id,
                sender_process_id: _,
                receiver_process_id,
                prompt,
            }) => {
                ts_msg!(
                    self,
                    "{} {}",
                    "collab".style(self.magenta),
                    format_collab_invocation("send_input", &call_id, Some(&prompt))
                        .style(self.bold)
                );
                eprintln!(
                    "  receiver: {}",
                    receiver_process_id.to_string().style(self.dimmed)
                );
            }
            EventMsg::CollabAgentInteractionEnd(CollabAgentInteractionEndEvent {
                call_id,
                sender_process_id: _,
                receiver_process_id,
                prompt,
                status,
                ..
            }) => {
                let success = !is_collab_status_failure(&status);
                let title_style = if success { self.green } else { self.red };
                let title = format!(
                    "{} {}:",
                    format_collab_invocation("send_input", &call_id, Some(&prompt)),
                    format_collab_status(&status)
                );
                ts_msg!(self, "{}", title.style(title_style));
                eprintln!(
                    "  receiver: {}",
                    receiver_process_id.to_string().style(self.dimmed)
                );
            }
            EventMsg::CollabWaitingBegin(CollabWaitingBeginEvent {
                sender_process_id: _,
                receiver_process_ids,
                call_id,
                ..
            }) => {
                ts_msg!(
                    self,
                    "{} {}",
                    "collab".style(self.magenta),
                    format_collab_invocation("wait", &call_id, /*prompt*/ None).style(self.bold)
                );
                eprintln!(
                    "  receivers: {}",
                    format_receiver_list(&receiver_process_ids).style(self.dimmed)
                );
            }
            EventMsg::CollabWaitingEnd(chaos_ipc::protocol::CollabWaitingEndEvent {
                sender_process_id: _,
                call_id,
                statuses,
                ..
            }) => {
                if statuses.is_empty() {
                    ts_msg!(
                        self,
                        "{} {}:",
                        format_collab_invocation("wait", &call_id, /*prompt*/ None),
                        "timed out".style(self.yellow)
                    );
                    return ChaosStatus::Running;
                }
                let success = !statuses.values().any(is_collab_status_failure);
                let title_style = if success { self.green } else { self.red };
                let title = format!(
                    "{} {} agents complete:",
                    format_collab_invocation("wait", &call_id, /*prompt*/ None),
                    statuses.len()
                );
                ts_msg!(self, "{}", title.style(title_style));
                let mut sorted = statuses
                    .into_iter()
                    .map(|(process_id, status)| (process_id.to_string(), status))
                    .collect::<Vec<_>>();
                sorted.sort_by(|(left, _), (right, _)| left.cmp(right));
                for (process_id, status) in sorted {
                    eprintln!(
                        "  {} {}",
                        process_id.style(self.dimmed),
                        format_collab_status(&status).style(style_for_agent_status(&status, self))
                    );
                }
            }
            EventMsg::CollabCloseBegin(CollabCloseBeginEvent {
                call_id,
                sender_process_id: _,
                receiver_process_id,
            }) => {
                ts_msg!(
                    self,
                    "{} {}",
                    "collab".style(self.magenta),
                    format_collab_invocation("close_agent", &call_id, /*prompt*/ None)
                        .style(self.bold)
                );
                eprintln!(
                    "  receiver: {}",
                    receiver_process_id.to_string().style(self.dimmed)
                );
            }
            EventMsg::CollabCloseEnd(CollabCloseEndEvent {
                call_id,
                sender_process_id: _,
                receiver_process_id,
                status,
                ..
            }) => {
                let success = !is_collab_status_failure(&status);
                let title_style = if success { self.green } else { self.red };
                let title = format!(
                    "{} {}:",
                    format_collab_invocation("close_agent", &call_id, /*prompt*/ None),
                    format_collab_status(&status)
                );
                ts_msg!(self, "{}", title.style(title_style));
                eprintln!(
                    "  receiver: {}",
                    receiver_process_id.to_string().style(self.dimmed)
                );
            }
            EventMsg::HookStarted(event) => self.render_hook_started(event),
            EventMsg::HookCompleted(event) => self.render_hook_completed(event),
            EventMsg::ShutdownComplete => return ChaosStatus::Shutdown,
            EventMsg::ProcessNameUpdated(_)
            | EventMsg::ExecApprovalRequest(_)
            | EventMsg::ApplyPatchApprovalRequest(_)
            | EventMsg::TerminalInteraction(_)
            | EventMsg::ExecCommandOutputDelta(_)
            | EventMsg::GetHistoryEntryResponse(_)
            | EventMsg::McpListToolsResponse(_)
            | EventMsg::AllToolsResponse(_)
            | EventMsg::ListCustomPromptsResponse(_)
            | EventMsg::ListSkillsResponse(_)
            | EventMsg::ListRemoteSkillsResponse(_)
            | EventMsg::RemoteSkillDownloaded(_)
            | EventMsg::RawResponseItem(_)
            | EventMsg::UserMessage(_)
            | EventMsg::EnteredReviewMode(_)
            | EventMsg::ExitedReviewMode(_)
            | EventMsg::ItemStarted(_)
            | EventMsg::ItemCompleted(_)
            | EventMsg::AgentMessageContentDelta(_)
            | EventMsg::PlanDelta(_)
            | EventMsg::ReasoningContentDelta(_)
            | EventMsg::ReasoningRawContentDelta(_)
            | EventMsg::SkillsUpdateAvailable
            | EventMsg::UndoCompleted(_)
            | EventMsg::UndoStarted(_)
            | EventMsg::ProcessRolledBack(_)
            | EventMsg::RequestUserInput(_)
            | EventMsg::RequestPermissions(_)
            | EventMsg::CollabResumeBegin(_)
            | EventMsg::CollabResumeEnd(_)
            | EventMsg::DynamicToolCallRequest(_)
            | EventMsg::DynamicToolCallResponse(_) => {}
        }
        ChaosStatus::Running
    }

    fn print_final_output(&mut self) {
        self.finish_progress_line();
        if let Some(usage_info) = &self.last_total_token_usage {
            eprintln!(
                "{}\n{}",
                "tokens used".style(self.magenta).style(self.italic),
                format_with_separators(usage_info.total_token_usage.blended_total())
            );
        }

        // In interactive terminals we already emitted the final assistant
        // message on stderr during event processing. Preserve stdout emission
        // only for non-interactive use so pipes and scripts still receive the
        // final message.
        #[allow(clippy::print_stdout)]
        if should_print_final_message_to_stdout(
            self.final_message.as_deref(),
            std::io::stdout().is_terminal(),
            std::io::stderr().is_terminal(),
        ) && let Some(message) = &self.final_message
        {
            if message.ends_with('\n') {
                print!("{message}");
            } else {
                println!("{message}");
            }
        }
    }
}
