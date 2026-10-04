use chaos_ipc::background_tasks::ProcessActivity;
use state_machines::state_machine;
use tokio::sync::watch;
use tokio::time::Instant;

#[derive(Debug)]
pub(super) struct WaitData {
    pub activity: watch::Receiver<ProcessActivity>,
    pub deadline: Instant,
    pub completed_turn: bool,
}

state_machine! {
    name: BackgroundWaitLifecycle,
    dynamic: true,
    initial: Waiting,
    states: [Forwarding, Waiting(WaitData), Complete, Stopped(String), ForwardingStopped(String)],
    final_states: [Complete, Stopped, ForwardingStopped],
    events {
        finish { transition: { from: Waiting, to: Complete } }
        stop {
            payload: Option<String>,
            transition: { from: Waiting, to: Stopped, data: own_reason }
            transition: { from: Forwarding, to: ForwardingStopped, data: own_reason }
        }
    }
}

impl<C, S> BackgroundWaitLifecycle<C, S> {
    fn own_reason(&self, reason: &mut Option<String>) -> String {
        reason
            .take()
            .unwrap_or_else(|| unreachable!("stop owns its reason"))
    }
}
