use serde::{Deserialize, Deserializer, Serialize, Serializer};
use state_machines::DynamicError;

pub use invocation::{
    DynamicInvocationLifecycle, InvocationLifecycleEvent, InvocationLifecycleState,
};
pub use session::{DynamicSessionLifecycle, SessionLifecycleEvent, SessionLifecycleState};
pub use turn::{DynamicTurnLifecycle, TurnLifecycleEvent, TurnLifecycleState};

mod session {
    use state_machines::state_machine;

    state_machine! {
        name: SessionLifecycle,
        dynamic: true,
        initial: Starting,
        states: [
            superstate Open {
                state Starting,
                state Idle,
                state Running,
                state Closing,
                state Unknown
            },
            Closed,
            Failed
        ],
        final_states: [Closed, Failed],
        events {
            ready { transition: { from: Starting, to: Idle } }
            busy { transition: { from: Idle, to: Running } }
            settle { transition: { from: Running, to: Idle } }
            reconcile { transition: { from: Unknown, to: Idle } }
            lose { transition: { from: Open, to: Unknown } }
            close { transition: { from: Open, to: Closing } }
            shut {
                transition: { from: Idle, to: Closed }
                transition: { from: Closing, to: Closed }
            }
            resolve { transition: { from: Unknown, to: Closed } }
            fail {
                transition: { from: Starting, to: Failed }
                transition: { from: Closing, to: Failed }
            }
        }
    }
}

mod turn {
    use state_machines::state_machine;

    state_machine! {
        name: TurnLifecycle,
        dynamic: true,
        initial: Persisting,
        states: [
            superstate Live {
                state Persisting,
                state Accepted,
                state Running,
                state WaitingBackground,
                state Interrupting
            },
            Unknown,
            Completed,
            Failed,
            Interrupted,
            TimedOut
        ],
        final_states: [Completed, Failed, Interrupted, TimedOut],
        events {
            accept { transition: { from: Persisting, to: Accepted } }
            start { transition: { from: Accepted, to: Running } }
            interrupt {
                transition: { from: Persisting, to: Interrupting }
                transition: { from: Accepted, to: Interrupting }
                transition: { from: Running, to: Interrupting }
                transition: { from: WaitingBackground, to: Interrupting }
            }
            settle { transition: { from: Live, to: WaitingBackground } }
            complete {
                transition: { from: Running, to: Completed }
                transition: { from: WaitingBackground, to: Completed }
                transition: { from: Interrupting, to: Completed }
                transition: { from: Unknown, to: Completed }
            }
            fail {
                transition: { from: Running, to: Failed }
                transition: { from: WaitingBackground, to: Failed }
                transition: { from: Interrupting, to: Failed }
                transition: { from: Unknown, to: Failed }
            }
            abort {
                transition: { from: Live, to: Interrupted }
                transition: { from: Unknown, to: Interrupted }
            }
            time_out {
                transition: { from: Live, to: TimedOut }
                transition: { from: Unknown, to: TimedOut }
            }
            lose {
                transition: { from: Live, to: Unknown }
                transition: { from: Unknown, to: Unknown }
            }
        }
    }
}

mod invocation {
    use state_machines::state_machine;

    state_machine! {
        name: InvocationLifecycle,
        dynamic: true,
        initial: Started,
        states: [Started, Completed, Unknown],
        final_states: [Completed, Unknown],
        events {
            complete { transition: { from: Started, to: Completed } }
            lose { transition: { from: Started, to: Unknown } }
        }
    }
}

macro_rules! persisted_state {
    (
        $state:ident,
        $dynamic:ident,
        $event:ident,
        { $($variant:ident => $name:literal),+ $(,)? }
    ) => {
        impl $state {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)+
                }
            }

            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $($name => Some(Self::$variant),)+
                    _ => None,
                }
            }

            pub fn is_final(self) -> bool {
                $dynamic::new_init_state((), self).is_finished()
            }

            pub fn fire(&mut self, event: $event) -> Result<(), DynamicError> {
                let mut machine = $dynamic::new_init_state((), *self);
                machine.handle(event)?;
                *self = machine.current_state();
                Ok(())
            }
        }

        impl Serialize for $state {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $state {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::parse(&value).ok_or_else(|| {
                    serde::de::Error::unknown_variant(&value, &[$($name),+])
                })
            }
        }
    };
}

persisted_state!(SessionLifecycleState, DynamicSessionLifecycle, SessionLifecycleEvent, {
    Starting => "starting",
    Idle => "idle",
    Running => "running",
    Closing => "closing",
    Unknown => "unknown",
    Closed => "closed",
    Failed => "failed",
});

persisted_state!(TurnLifecycleState, DynamicTurnLifecycle, TurnLifecycleEvent, {
    Persisting => "persisting",
    Accepted => "accepted",
    Running => "running",
    WaitingBackground => "waiting_background",
    Interrupting => "interrupting",
    Unknown => "unknown",
    Completed => "completed",
    Failed => "failed",
    Interrupted => "interrupted",
    TimedOut => "timed_out",
});

persisted_state!(InvocationLifecycleState, DynamicInvocationLifecycle, InvocationLifecycleEvent, {
    Started => "started",
    Completed => "completed",
    Unknown => "unknown",
});

impl SessionLifecycleState {
    pub const LEDGER: &'static [Self] = &[Self::Idle, Self::Running, Self::Unknown, Self::Closed];

    pub fn ledger_str(self) -> Option<&'static str> {
        match self {
            Self::Running => Some("active"),
            Self::Idle | Self::Unknown | Self::Closed => Some(self.as_str()),
            Self::Starting | Self::Closing | Self::Failed => None,
        }
    }

    pub fn from_ledger(value: &str) -> Option<Self> {
        Self::LEDGER
            .iter()
            .copied()
            .find(|state| state.ledger_str() == Some(value))
    }
}

impl TurnLifecycleState {
    pub fn conclude(&mut self, outcome: Self) -> Result<(), DynamicError> {
        let event = match outcome {
            Self::Completed => TurnLifecycleEvent::Complete,
            Self::Failed => TurnLifecycleEvent::Fail,
            Self::Interrupted => TurnLifecycleEvent::Abort,
            Self::TimedOut => TurnLifecycleEvent::TimeOut,
            Self::Unknown => TurnLifecycleEvent::Lose,
            other => return Err(DynamicError::invalid_transition(other.name(), "conclude")),
        };
        self.fire(event)
    }
}

pub mod ledger {
    use super::SessionLifecycleState;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        state: &SessionLifecycleState,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let value = state.ledger_str().ok_or_else(|| {
            serde::ser::Error::custom(format!("{} is not a ledger session state", state.name()))
        })?;
        serializer.serialize_str(value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SessionLifecycleState, D::Error> {
        let value = String::deserialize(deserializer)?;
        SessionLifecycleState::from_ledger(&value).ok_or_else(|| {
            serde::de::Error::unknown_variant(&value, &["idle", "active", "unknown", "closed"])
        })
    }
}

#[cfg(test)]
mod tests;
