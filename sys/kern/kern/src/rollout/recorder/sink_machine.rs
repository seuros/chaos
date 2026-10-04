use super::{ActiveJournalWriter, PendingJournalConfig, RolloutItem};
use state_machines::state_machine;

#[derive(Debug)]
pub(super) struct PendingBatch {
    pub(super) config: PendingJournalConfig,
    pub(super) items: Vec<RolloutItem>,
}

state_machine! {
    name: JournalStorage,
    dynamic: true,
    initial: Pending,
    states: [
        Disabled,
        Pending(PendingBatch),
        Active(ActiveJournalWriter),
    ],
    events {
        connect {
            payload: Option<ActiveJournalWriter>,
            transition: { from: Pending, to: Active, data: own_writer }
        }
        disable {
            transition: { from: Pending, to: Disabled }
            transition: { from: Active, to: Disabled }
            transition: { from: Disabled, to: Disabled }
        }
    }
}

impl<C, S> JournalStorage<C, S> {
    fn own_writer(&self, writer: &mut Option<ActiveJournalWriter>) -> ActiveJournalWriter {
        writer
            .take()
            .unwrap_or_else(|| unreachable!("connect carries an owned journal writer"))
    }
}
