//! Caller lifetime policy. Scheduling and task counts remain kernel-owned.

use std::time::Duration;

use chaos_ipc::background_tasks::{ProcessActivity, WakePolicy};
use chaos_ipc::protocol::{Event, EventMsg};
use chaos_kern::Process;
use tokio::sync::watch;
use tokio::time::Instant;

mod lifecycle;
use lifecycle::{
    BackgroundWaitLifecycle, BackgroundWaitLifecycleEvent, BackgroundWaitLifecycleState,
    DynamicBackgroundWaitLifecycle, WaitData,
};

pub const DEFAULT_BACKGROUND_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug)]
pub enum WaitEvent {
    Event(Box<Event>),
    Complete,
    Stopped(String),
}

/// Reduces the kernel activity subscription and ordinary event stream. It
/// forwards every turn event, including intermediate completion turns.
pub struct BackgroundWait {
    lifecycle: DynamicBackgroundWaitLifecycle<()>,
}

impl BackgroundWait {
    pub fn is_enabled(&self) -> bool {
        !matches!(
            self.lifecycle.current_state(),
            BackgroundWaitLifecycleState::Forwarding
                | BackgroundWaitLifecycleState::ForwardingStopped
        )
    }

    pub fn for_recovery(process: &Process, timeout: Duration) -> Self {
        let mut wait = Self::new(process, true, timeout);
        wait.lifecycle
            .waiting_data_mut()
            .unwrap_or_else(|| unreachable!("recovery owns its wait"))
            .completed_turn = true;
        wait
    }
    pub fn new(process: &Process, enabled: bool, timeout: Duration) -> Self {
        Self::from_activity(process.subscribe_activity(), enabled, timeout)
    }

    fn from_activity(
        activity: watch::Receiver<ProcessActivity>,
        enabled: bool,
        timeout: Duration,
    ) -> Self {
        let lifecycle = if enabled {
            BackgroundWaitLifecycle::new(())
                .with_waiting_data(WaitData {
                    activity,
                    deadline: Instant::now()
                        + timeout.clamp(Duration::from_secs(1), Duration::from_secs(86_400)),
                    completed_turn: false,
                })
                .into_dynamic()
        } else {
            DynamicBackgroundWaitLifecycle::new_init_state(
                (),
                BackgroundWaitLifecycleState::Forwarding,
            )
        };
        Self { lifecycle }
    }

    fn terminal(&self) -> Option<WaitEvent> {
        match self.lifecycle.current_state() {
            BackgroundWaitLifecycleState::Complete => Some(WaitEvent::Complete),
            BackgroundWaitLifecycleState::Stopped => self
                .lifecycle
                .stopped_data()
                .cloned()
                .map(WaitEvent::Stopped),
            BackgroundWaitLifecycleState::ForwardingStopped => self
                .lifecycle
                .forwarding_stopped_data()
                .cloned()
                .map(WaitEvent::Stopped),
            _ => None,
        }
    }

    fn finish(&mut self, outcome: WaitEvent) -> WaitEvent {
        let event = match outcome {
            WaitEvent::Complete => BackgroundWaitLifecycleEvent::Finish,
            WaitEvent::Stopped(reason) => BackgroundWaitLifecycleEvent::Stop(Some(reason)),
            WaitEvent::Event(_) => unreachable!("events do not finish a wait"),
        };
        assert!(
            self.lifecycle.handle(event).is_ok(),
            "wait outcome is valid"
        );
        self.terminal()
            .unwrap_or_else(|| unreachable!("wait finished"))
    }

    pub async fn next(&mut self, process: &Process) -> WaitEvent {
        loop {
            if let Some(outcome) = self.terminal() {
                return outcome;
            }
            let waiting = self.lifecycle.waiting_data();
            let enabled = waiting.is_some();
            let deadline = waiting
                .map(|data| data.deadline)
                .unwrap_or_else(Instant::now);
            let outcome = if let Some(waiting) = waiting {
                if Instant::now() >= deadline {
                    return self.finish(WaitEvent::Stopped("background wait timed out".into()));
                }
                disposition(&waiting.activity.borrow(), waiting.completed_turn)
            } else {
                None
            };
            tokio::select! {
                biased;
                event = process.next_event() => {
                    return match event {
                        Ok(event) => {
                            if let Some(waiting) = self.lifecycle.waiting_data_mut() {
                                waiting.completed_turn |= matches!(event.msg, EventMsg::TurnComplete(_));
                            }
                            if enabled && matches!(event.msg, EventMsg::ExecApprovalRequest(_)
                                | EventMsg::ApplyPatchApprovalRequest(_) | EventMsg::RequestUserInput(_)
                                | EventMsg::RequestPermissions(_) | EventMsg::ElicitationRequest(_)
                                | EventMsg::DynamicToolCallRequest(_))
                            {
                                self.finish(WaitEvent::Stopped("background work requires interactive input".into()));
                            }
                            WaitEvent::Event(Box::new(event))
                        }
                        Err(error) => self.finish(WaitEvent::Stopped(error.to_string())),
                    };
                }
                _ = std::future::ready(()), if outcome.is_some() => {
                    if let Some(outcome) = outcome {
                        return self.finish(outcome);
                    }
                }
                _ = tokio::time::sleep_until(deadline), if enabled => {
                    return self.finish(WaitEvent::Stopped("background wait timed out".into()));
                }
                changed = async {
                    match self.lifecycle.waiting_data_mut() {
                        Some(waiting) => waiting.activity.changed().await,
                        None => std::future::pending().await,
                    }
                }, if enabled => {
                    if changed.is_err() {
                        return self.finish(WaitEvent::Stopped("kernel activity subscription closed".into()));
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
mod tests;
