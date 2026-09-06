//! Caller lifetime policy. Scheduling and task counts remain kernel-owned.

use std::time::Duration;

use chaos_ipc::background_tasks::{ProcessActivity, WakePolicy};
use chaos_ipc::protocol::{Event, EventMsg};
use chaos_kern::Process;
use tokio::sync::watch;
use tokio::time::Instant;

pub const DEFAULT_BACKGROUND_TIMEOUT: Duration = Duration::from_secs(600);

pub enum WaitEvent {
    Event(Box<Event>),
    Complete,
    Stopped(String),
}

/// Reduces the kernel activity subscription and ordinary event stream. It
/// forwards every turn event, including intermediate completion turns.
pub struct BackgroundWait {
    activity: watch::Receiver<ProcessActivity>,
    deadline: Instant,
    enabled: bool,
    completed_turn: bool,
    blocked: Option<String>,
}

impl BackgroundWait {
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn for_recovery(process: &Process, timeout: Duration) -> Self {
        let mut wait = Self::new(process, true, timeout);
        wait.completed_turn = true;
        wait
    }
    pub fn new(process: &Process, enabled: bool, timeout: Duration) -> Self {
        Self {
            activity: process.subscribe_activity(),
            deadline: Instant::now()
                + timeout.clamp(Duration::from_secs(1), Duration::from_secs(86_400)),
            enabled,
            completed_turn: false,
            blocked: None,
        }
    }

    pub async fn next(&mut self, process: &Process) -> WaitEvent {
        loop {
            let outcome = if self.enabled {
                if let Some(reason) = self.blocked.take() {
                    return WaitEvent::Stopped(reason);
                }
                if Instant::now() >= self.deadline {
                    return WaitEvent::Stopped("background wait timed out".into());
                }
                disposition(&self.activity.borrow(), self.completed_turn)
            } else {
                None
            };
            tokio::select! {
                biased;
                event = process.next_event() => {
                    return match event {
                        Ok(event) => {
                            self.completed_turn |= matches!(event.msg, EventMsg::TurnComplete(_));
                            if matches!(event.msg, EventMsg::ExecApprovalRequest(_)
                                | EventMsg::ApplyPatchApprovalRequest(_) | EventMsg::RequestUserInput(_)
                                | EventMsg::RequestPermissions(_) | EventMsg::ElicitationRequest(_)
                                | EventMsg::DynamicToolCallRequest(_))
                            {
                                self.blocked = Some("background work requires interactive input".into());
                            }
                            WaitEvent::Event(Box::new(event))
                        }
                        Err(error) => WaitEvent::Stopped(error.to_string()),
                    };
                }
                _ = std::future::ready(()), if outcome.is_some() => {
                    if let Some(outcome) = outcome {
                        return outcome;
                    }
                }
                _ = tokio::time::sleep_until(self.deadline), if self.enabled => {
                    return WaitEvent::Stopped("background wait timed out".into());
                }
                changed = self.activity.changed(), if self.enabled => {
                    if changed.is_err() {
                        return WaitEvent::Stopped("kernel activity subscription closed".into());
                    }
                }
            }
        }
    }
}

fn disposition(activity: &ProcessActivity, completed_turn: bool) -> Option<WaitEvent> {
    if let Some(reason) = &activity.blocked {
        return Some(WaitEvent::Stopped(reason.clone()));
    }
    if activity.wake_policy != WakePolicy::Enabled {
        return Some(WaitEvent::Stopped(
            "background wake-ups are suppressed".into(),
        ));
    }
    (completed_turn && activity.is_quiescent()).then_some(WaitEvent::Complete)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiescence_requires_a_completed_turn_and_no_pending_work() {
        let mut activity = ProcessActivity::default();
        assert!(disposition(&activity, false).is_none());
        assert!(matches!(
            disposition(&activity, true),
            Some(WaitEvent::Complete)
        ));
        activity.outstanding_tasks = 1;
        assert!(disposition(&activity, true).is_none());
        activity.outstanding_tasks = 0;
        activity.pending_completions = 1;
        assert!(disposition(&activity, true).is_none());
        activity.pending_completions = 0;
        activity.active_turn = true;
        assert!(disposition(&activity, true).is_none());
    }

    #[test]
    fn interrupted_or_blocked_is_not_success() {
        let mut activity = ProcessActivity {
            wake_policy: WakePolicy::Interrupted,
            ..Default::default()
        };
        assert!(matches!(
            disposition(&activity, true),
            Some(WaitEvent::Stopped(_))
        ));
        activity.wake_policy = WakePolicy::Enabled;
        activity.blocked = Some("storage".into());
        assert!(matches!(
            disposition(&activity, false),
            Some(WaitEvent::Stopped(_))
        ));
    }
}
