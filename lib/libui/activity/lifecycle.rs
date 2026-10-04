use super::{Activity, Phase};
use state_machines::state_machine;
use std::collections::HashSet;
use std::time::Instant;

#[derive(Debug, Default)]
pub(super) struct ActivityData {
    pub last_activity: Option<Instant>,
    pub last_runtime_event: Option<Instant>,
    pub expecting_since: Option<Instant>,
    pub turn: Option<String>,
    pub tools: HashSet<(&'static str, String)>,
    pub waits: HashSet<String>,
    pub inputs: HashSet<String>,
}

#[derive(Debug)]
pub(super) struct Priority {
    inputs: bool,
    waits: bool,
    tools: bool,
}

state_machine! {
    name: LiveActivity,
    dynamic: true,
    initial: Idle,
    states: [
        superstate Observed(ActivityData) {
            state Idle, state Starting, state Working, state Tools,
            state WaitingAgents, state NeedsInput, state Reconnecting,
            state Failed, state Interrupted, state Closed, state Disconnected
        }
    ],
    events {
        start { transition: { from: Observed, to: Starting } }
        work { transition: { from: Observed, to: Working } }
        idle { transition: { from: Observed, to: Idle } }
        reconnect { transition: { from: Observed, to: Reconnecting } }
        fail { transition: { from: Observed, to: Failed } }
        interrupt { transition: { from: Observed, to: Interrupted } }
        close { transition: { from: Observed, to: Closed } }
        disconnect { transition: { from: Observed, to: Disconnected } }
        refresh {
            branching: true,
            payload: Priority,
            transition: { from: Observed, to: NeedsInput, guards: [needs_input] }
            transition: { from: Observed, to: WaitingAgents, guards: [waiting] }
            transition: { from: Observed, to: Tools, guards: [using_tools] }
            transition: { from: Observed, to: Working, guards: [working] }
        }
    }
}

impl<C, S> LiveActivity<C, S> {
    fn needs_input(&self, _ctx: &C, priority: &Priority) -> bool {
        priority.inputs
    }
    fn waiting(&self, _ctx: &C, priority: &Priority) -> bool {
        !priority.inputs && priority.waits
    }
    fn using_tools(&self, _ctx: &C, priority: &Priority) -> bool {
        !priority.inputs && !priority.waits && priority.tools
    }
    fn working(&self, _ctx: &C, priority: &Priority) -> bool {
        !priority.inputs && !priority.waits && !priority.tools
    }
}

pub(super) struct ProcessActivity {
    machine: DynamicLiveActivity<()>,
}

impl Default for ProcessActivity {
    fn default() -> Self {
        Self {
            machine: LiveActivity::new(())
                .with_observed_data(ActivityData::default())
                .into_dynamic(),
        }
    }
}

impl ProcessActivity {
    pub(super) fn data(&self) -> &ActivityData {
        self.machine
            .observed_data()
            .unwrap_or_else(|| unreachable!("activity owns observations"))
    }

    pub(super) fn data_mut(&mut self) -> &mut ActivityData {
        self.machine
            .observed_data_mut()
            .unwrap_or_else(|| unreachable!("activity owns observations"))
    }

    pub(super) fn snapshot(&self) -> Activity {
        let data = self.data();
        Activity {
            phase: self.phase(),
            last_activity: data.last_activity,
            last_runtime_event: data.last_runtime_event,
            expecting_since: data.expecting_since,
        }
    }

    pub(super) fn phase(&self) -> Phase {
        match self.machine.current_state() {
            LiveActivityState::Idle => Phase::Idle,
            LiveActivityState::Starting => Phase::Starting,
            LiveActivityState::Working => Phase::Working,
            LiveActivityState::Tools => Phase::Tools,
            LiveActivityState::WaitingAgents => Phase::WaitingAgents,
            LiveActivityState::NeedsInput => Phase::NeedsInput,
            LiveActivityState::Reconnecting => Phase::Reconnecting,
            LiveActivityState::Failed => Phase::Failed,
            LiveActivityState::Interrupted => Phase::Interrupted,
            LiveActivityState::Closed => Phase::Closed,
            LiveActivityState::Disconnected => Phase::Disconnected,
        }
    }

    pub(super) fn set_phase(&mut self, phase: Phase) {
        let event = match phase {
            Phase::Idle => LiveActivityEvent::Idle,
            Phase::Starting => LiveActivityEvent::Start,
            Phase::Working => LiveActivityEvent::Work,
            Phase::Reconnecting => LiveActivityEvent::Reconnect,
            Phase::Failed => LiveActivityEvent::Fail,
            Phase::Interrupted => LiveActivityEvent::Interrupt,
            Phase::Closed => LiveActivityEvent::Close,
            Phase::Disconnected => LiveActivityEvent::Disconnect,
            _ => unreachable!("pending work phase uses guarded priority"),
        };
        assert!(self.machine.handle(event).is_ok());
    }

    pub(super) fn refresh_phase(&mut self) {
        let data = self.data();
        let priority = Priority {
            inputs: !data.inputs.is_empty(),
            waits: !data.waits.is_empty(),
            tools: !data.tools.is_empty(),
        };
        assert!(
            self.machine
                .handle(LiveActivityEvent::Refresh(priority))
                .is_ok()
        );
    }

    pub(super) fn finish(&mut self, phase: Phase) {
        let data = self.data_mut();
        data.turn = None;
        data.tools.clear();
        data.waits.clear();
        data.inputs.clear();
        self.set_phase(phase);
    }
}
