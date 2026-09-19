use super::*;

impl Tracker {
    pub(super) fn observe_local(&mut self, process_id: ProcessId, event: &Event, now: Instant) {
        let state = self.processes.entry(process_id).or_default();
        let msg = &event.msg;

        if let EventMsg::TurnStarted(ev) = msg {
            if state.turn.as_ref() != Some(&ev.turn_id) {
                *state = ProcessActivity::default();
                state.turn = Some(ev.turn_id.clone());
                state.activity.phase = Phase::Working;
                state.activity.last_activity = Some(now);
            }
            state.activity.last_runtime_event = Some(now);
            return;
        }

        let turn = match msg {
            EventMsg::TurnComplete(ev) => Some(ev.turn_id.as_str()),
            EventMsg::TurnAborted(ev) => ev.turn_id.as_deref(),
            EventMsg::TurnProgress(ev) => Some(ev.turn_id.as_str()),
            EventMsg::AgentMessageContentDelta(ev) => Some(ev.turn_id.as_str()),
            EventMsg::ReasoningContentDelta(ev) => Some(ev.turn_id.as_str()),
            EventMsg::ReasoningRawContentDelta(ev) => Some(ev.turn_id.as_str()),
            EventMsg::PlanDelta(ev) => Some(ev.turn_id.as_str()),
            EventMsg::ItemStarted(ev) => Some(ev.turn_id.as_str()),
            EventMsg::ItemCompleted(ev) => Some(ev.turn_id.as_str()),
            EventMsg::ExecCommandBegin(ev) => Some(ev.turn_id.as_str()),
            EventMsg::ExecCommandEnd(ev) => Some(ev.turn_id.as_str()),
            EventMsg::PatchApplyBegin(ev) => Some(ev.turn_id.as_str()),
            EventMsg::PatchApplyEnd(ev) => Some(ev.turn_id.as_str()),
            EventMsg::ExecApprovalRequest(ev) => Some(ev.turn_id.as_str()),
            EventMsg::ApplyPatchApprovalRequest(ev) => Some(ev.turn_id.as_str()),
            EventMsg::RequestUserInput(ev) => Some(ev.turn_id.as_str()),
            EventMsg::RequestPermissions(ev) => Some(ev.turn_id.as_str()),
            EventMsg::ElicitationRequest(ev) => ev.turn_id.as_deref(),
            EventMsg::DynamicToolCallRequest(ev) => Some(ev.turn_id.as_str()),
            EventMsg::DynamicToolCallResponse(ev) => Some(ev.turn_id.as_str()),
            _ => None,
        };
        // Empty IDs are emitted by older producers and cannot be correlated.
        if let Some(turn) = turn.filter(|turn| !turn.is_empty())
            && state.turn.as_deref() != Some(turn)
        {
            return;
        }
        state.activity.last_runtime_event = Some(now);
        match msg {
            EventMsg::ShutdownComplete => {
                state.finish(Phase::Closed);
                return;
            }
            EventMsg::TurnComplete(_) => {
                let phase = if state.activity.phase == Phase::Failed {
                    Phase::Failed
                } else {
                    Phase::Idle
                };
                state.finish(phase);
                return;
            }
            EventMsg::TurnAborted(_) => {
                state.finish(Phase::Interrupted);
                return;
            }
            EventMsg::Error(_) if state.turn.is_some() => {
                state.activity.phase = Phase::Failed;
                return;
            }
            EventMsg::StreamError(_) if state.turn.is_some() => {
                state.activity.phase = Phase::Reconnecting;
                return;
            }
            _ => {}
        }
        if state.turn.is_none() {
            return;
        }

        // Whitelist work signals. In particular, TurnProgress, TokenCount,
        // metadata and background notifications are NOT evidence of progress.
        let tool_change = match msg {
            EventMsg::McpToolCallBegin(ev) => Some(("mcp", &ev.call_id, true)),
            EventMsg::McpToolCallEnd(ev) => Some(("mcp", &ev.call_id, false)),
            EventMsg::ExecCommandBegin(ev) => Some(("exec", &ev.call_id, true)),
            EventMsg::ExecCommandEnd(ev) => Some(("exec", &ev.call_id, false)),
            EventMsg::PatchApplyBegin(ev) => Some(("patch", &ev.call_id, true)),
            EventMsg::PatchApplyEnd(ev) => Some(("patch", &ev.call_id, false)),
            EventMsg::WebSearchBegin(ev) => Some(("web", &ev.call_id, true)),
            EventMsg::WebSearchEnd(ev) => Some(("web", &ev.call_id, false)),
            EventMsg::ImageGenerationBegin(ev) => Some(("image", &ev.call_id, true)),
            EventMsg::ImageGenerationEnd(ev) => Some(("image", &ev.call_id, false)),
            EventMsg::DynamicToolCallRequest(ev) => Some(("dynamic", &ev.call_id, true)),
            EventMsg::DynamicToolCallResponse(ev) => Some(("dynamic", &ev.call_id, false)),
            _ => None,
        };
        if let Some((kind, id, begin)) = tool_change {
            let key = (kind, id.clone());
            if begin {
                state.tools.insert(key);
            } else {
                state.tools.remove(&key);
            }
        } else {
            match msg {
                EventMsg::CollabWaitingBegin(ev) => {
                    state.waits.insert(ev.call_id.clone());
                }
                EventMsg::CollabWaitingEnd(ev) => {
                    state.waits.remove(&ev.call_id);
                }
                EventMsg::ExecApprovalRequest(ev) => {
                    state
                        .inputs
                        .insert(format!("exec:{}", ev.effective_approval_id()));
                }
                EventMsg::ApplyPatchApprovalRequest(ev) => {
                    state.inputs.insert(format!("patch:{}", ev.call_id));
                }
                EventMsg::RequestUserInput(ev) => {
                    state
                        .inputs
                        .insert(format!("input:{}:{}", ev.turn_id, ev.call_id));
                }
                EventMsg::RequestPermissions(ev) => {
                    state.inputs.insert(format!("permissions:{}", ev.call_id));
                }
                EventMsg::ElicitationRequest(ev) => {
                    state
                        .inputs
                        .insert(format!("elicitation:{}:{}", ev.server_name, ev.id));
                }
                EventMsg::AgentMessageContentDelta(ev) if !ev.delta.is_empty() => {}
                EventMsg::ReasoningContentDelta(ev) if !ev.delta.is_empty() => {}
                EventMsg::ReasoningRawContentDelta(ev) if !ev.delta.is_empty() => {}
                EventMsg::PlanDelta(ev) if !ev.delta.is_empty() => {}
                EventMsg::AgentMessage(ev) if !ev.message.is_empty() => {}
                EventMsg::AgentReasoning(ev) if !ev.text.is_empty() => {}
                EventMsg::AgentReasoningRawContent(ev) if !ev.text.is_empty() => {}
                EventMsg::ExecCommandOutputDelta(ev) if !ev.chunk.is_empty() => {}
                EventMsg::ItemStarted(ev)
                    if !matches!(ev.item, chaos_ipc::items::TurnItem::UserMessage(_)) => {}
                EventMsg::ItemCompleted(ev)
                    if !matches!(ev.item, chaos_ipc::items::TurnItem::UserMessage(_)) => {}
                EventMsg::ContextCompacted(_)
                | EventMsg::ElicitationComplete(_)
                | EventMsg::PlanUpdate(_)
                | EventMsg::ViewImageToolCall(_)
                | EventMsg::CollabAgentSpawnBegin(_)
                | EventMsg::CollabAgentSpawnEnd(_)
                | EventMsg::CollabAgentInteractionBegin(_)
                | EventMsg::CollabAgentInteractionEnd(_)
                | EventMsg::CollabCloseBegin(_)
                | EventMsg::CollabCloseEnd(_)
                | EventMsg::CollabResumeBegin(_)
                | EventMsg::CollabResumeEnd(_) => {}
                _ => return,
            }
        }
        state.activity.last_activity = Some(now);
        state.refresh_phase();
    }
}
