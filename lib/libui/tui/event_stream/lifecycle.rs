use super::EventSource;
use state_machines::state_machine;
use std::any::Any;
use std::marker::PhantomData;

pub(super) struct Source(Box<dyn Any + Send>);

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Source(..)")
    }
}

state_machine! {
    name: TerminalInput,
    dynamic: true,
    initial: Start,
    states: [superstate Available { state Start, state Running(Source) }, Paused],
    events {
        begin {
            payload: Option<Source>,
            transition: { from: Start, to: Running, data: own_source }
        }
        pause {
            transition: { from: Available, to: Paused }
            transition: { from: Paused, internal: true }
        }
        reset {
            transition: { from: Available, to: Start }
            transition: { from: Paused, to: Start }
        }
    }
}

impl<C, S> TerminalInput<C, S> {
    fn own_source(&self, source: &mut Option<Source>) -> Source {
        source
            .take()
            .unwrap_or_else(|| unreachable!("begin owns terminal source"))
    }
}

pub(super) struct EventBrokerState<S: EventSource> {
    machine: DynamicTerminalInput<()>,
    source: PhantomData<fn() -> S>,
}

impl<S: EventSource + Default> EventBrokerState<S> {
    pub(super) fn new() -> Self {
        Self {
            machine: TerminalInput::new(()).into_dynamic(),
            source: PhantomData,
        }
    }

    pub(super) fn pause(&mut self) {
        assert!(self.machine.handle(TerminalInputEvent::Pause).is_ok());
    }

    pub(super) fn reset(&mut self) {
        assert!(self.machine.handle(TerminalInputEvent::Reset).is_ok());
    }

    pub(super) fn active_event_source_mut(&mut self) -> Option<&mut S> {
        match self.machine.current_state() {
            TerminalInputState::Paused => return None,
            TerminalInputState::Start => {
                let source = Source(Box::new(S::default()));
                assert!(
                    self.machine
                        .handle(TerminalInputEvent::Begin(Some(source)))
                        .is_ok()
                );
            }
            TerminalInputState::Running => {}
        }
        Some(
            self.machine
                .running_data_mut()
                .and_then(|source| source.0.downcast_mut::<S>())
                .unwrap_or_else(|| unreachable!("broker owns its source type")),
        )
    }
}
