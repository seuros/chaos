use chaos_ipc::protocol::AgentStatus;
use chaos_ipc::protocol::EventMsg;

/// Derive the next agent status from a single emitted event.
/// Returns `None` when the event does not affect status tracking.
pub(crate) fn agent_status_from_event(msg: &EventMsg) -> Option<AgentStatus> {
    match msg {
        EventMsg::TurnStarted(_) => Some(AgentStatus::Running),
        EventMsg::TurnComplete(ev) => Some(AgentStatus::Completed(ev.last_agent_message.clone())),
        EventMsg::TurnAborted(ev) => match ev.reason {
            chaos_ipc::protocol::TurnAbortReason::Interrupted => Some(AgentStatus::Interrupted),
            _ => Some(AgentStatus::Errored(format!("{:?}", ev.reason))),
        },
        EventMsg::Error(ev) => Some(AgentStatus::Errored(ev.message.clone())),
        EventMsg::ShutdownComplete => Some(AgentStatus::Shutdown),
        _ => None,
    }
}

pub(crate) fn is_final(status: &AgentStatus) -> bool {
    !matches!(
        status,
        AgentStatus::PendingInit | AgentStatus::Running | AgentStatus::Interrupted
    )
}

/// Empty completions must not erase turn failures.
pub(crate) fn preserve_turn_failure(current: &AgentStatus, next: AgentStatus) -> AgentStatus {
    if matches!(next, AgentStatus::Completed(None))
        && matches!(current, AgentStatus::Errored(_) | AgentStatus::Interrupted)
    {
        current.clone()
    } else {
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_completion_preserves_failure_but_next_turn_resets() {
        for failure in [
            AgentStatus::Errored("provider failed".into()),
            AgentStatus::Interrupted,
        ] {
            assert_eq!(
                preserve_turn_failure(&failure, AgentStatus::Completed(None)),
                failure
            );
            assert_eq!(
                preserve_turn_failure(&failure, AgentStatus::Running),
                AgentStatus::Running
            );
        }
        assert_eq!(
            preserve_turn_failure(&AgentStatus::Running, AgentStatus::Completed(None)),
            AgentStatus::Completed(None),
        );
    }
}
