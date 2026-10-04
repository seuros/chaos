use state_machines::state_machine;
use tokio::task::JoinHandle;

#[derive(Debug, Default)]
pub(super) struct Correlation {
    generation: u64,
}

pub(super) struct Invocation(JoinHandle<()>);

impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Invocation(..)")
    }
}

impl Drop for Invocation {
    fn drop(&mut self) {
        self.0.abort();
    }
}

state_machine! {
    name: StatusScript,
    dynamic: true,
    initial: Idle,
    states: [
        superstate Tracked(Correlation) {
            state Idle,
            state Rendering(Invocation),
        }
    ],
    events {
        launch {
            payload: Option<Invocation>,
            transition: { from: Idle, to: Rendering, data: own_invocation }
        }
        stop { transition: { from: Tracked, to: Idle } }
    }
}

impl<C, S> StatusScript<C, S> {
    fn own_invocation(&self, invocation: &mut Option<Invocation>) -> Invocation {
        invocation
            .take()
            .unwrap_or_else(|| unreachable!("render owns invocation"))
    }
}

pub(super) struct StatusScriptRender {
    machine: DynamicStatusScript<()>,
}

impl Default for StatusScriptRender {
    fn default() -> Self {
        Self {
            machine: StatusScript::new(())
                .with_tracked_data(Correlation::default())
                .into_dynamic(),
        }
    }
}

impl StatusScriptRender {
    pub(super) fn generation(&self) -> u64 {
        self.machine
            .tracked_data()
            .unwrap_or_else(|| unreachable!("tracked correlation"))
            .generation
    }

    pub(super) fn invalidate(&mut self) -> u64 {
        assert!(self.machine.handle(StatusScriptEvent::Stop).is_ok());
        let correlation = self
            .machine
            .tracked_data_mut()
            .unwrap_or_else(|| unreachable!("tracked correlation"));
        correlation.generation = correlation.generation.wrapping_add(1);
        correlation.generation
    }

    pub(super) fn launch(&mut self, task: JoinHandle<()>) {
        assert!(
            self.machine
                .handle(StatusScriptEvent::Launch(Some(Invocation(task))))
                .is_ok()
        );
    }

    pub(super) fn complete(&mut self, generation: u64) -> bool {
        if self.generation() != generation {
            return false;
        }
        assert!(self.machine.handle(StatusScriptEvent::Stop).is_ok());
        true
    }
}
