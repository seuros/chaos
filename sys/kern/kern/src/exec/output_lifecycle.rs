use state_machines::{runtime::Parallel, state_machine};
use std::sync::Arc;
use tokio::sync::watch;
use tokio::sync::{Mutex, broadcast};
use tokio::time::{Duration, Instant};

use super::head_tail_buffer::HeadTailBuffer;
use crate::chaos::{Session, TurnContext};

const POST_EXIT_CLOSE_WAIT_CAP: Duration = Duration::from_millis(50);

struct CollectionData {
    output: Vec<u8>,
    deadline: Instant,
    pause: Option<watch::Receiver<bool>>,
}

impl std::fmt::Debug for CollectionData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CollectionData")
            .field("bytes", &self.output.len())
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct CloseWait {
    deadline: Instant,
}

mod execution {
    use super::*;

    state_machine! {
        name: ExecObservation,
        dynamic: true,
        initial: Running,
        states: [Running, Exited],
        final_states: [Exited],
        events {
            exit { transition: { from: Running, to: Exited } }
        }
    }
}

mod collection {
    use super::*;

    state_machine! {
        name: ExecOutputCollection,
        dynamic: true,
        initial: Reading,
        states: [
            superstate Collecting(CollectionData) {
                state Reading, state Closing(CloseWait)
            },
            Complete
        ],
        final_states: [Complete],
        events {
            close {
                payload: Instant,
                transition: { from: Reading, to: Closing, data: close_wait }
            }
            finish { transition: { from: Collecting, to: Complete } }
        }
    }

    impl<C, S> ExecOutputCollection<C, S> {
        fn close_wait(&self, deadline: &mut Instant) -> CloseWait {
            CloseWait {
                deadline: *deadline,
            }
        }
    }
}

pub(super) struct Collection {
    regions: Parallel<
        execution::DynamicExecObservation<()>,
        collection::DynamicExecOutputCollection<()>,
    >,
}

impl Collection {
    pub(super) fn new(deadline: Instant, pause: Option<watch::Receiver<bool>>) -> Self {
        Self {
            regions: Parallel::new(
                execution::DynamicExecObservation::new(()),
                collection::ExecOutputCollection::new(())
                    .with_collecting_data(CollectionData {
                        output: Vec::with_capacity(4096),
                        deadline,
                        pause,
                    })
                    .into_dynamic(),
            ),
        }
    }

    fn data(&self) -> &CollectionData {
        self.regions
            .right()
            .collecting_data()
            .unwrap_or_else(|| unreachable!("collection owns its output and deadline"))
    }

    fn data_mut(&mut self) -> &mut CollectionData {
        self.regions
            .right_mut()
            .collecting_data_mut()
            .unwrap_or_else(|| unreachable!("collection owns its output and deadline"))
    }

    pub(super) fn observe_exit(&mut self, exited: bool) {
        if exited && !self.exited() {
            assert!(
                self.regions
                    .left_mut()
                    .handle(execution::ExecObservationEvent::Exit)
                    .is_ok()
            );
        }
    }

    pub(super) fn exited(&self) -> bool {
        self.regions.left().is_finished()
    }

    pub(super) fn deadline(&self) -> Instant {
        self.data().deadline
    }

    pub(super) fn pause_receiver(&self) -> Option<watch::Receiver<bool>> {
        self.data().pause.clone()
    }

    pub(super) fn close_deadline(&mut self, now: Instant) -> Instant {
        assert!(self.exited());
        if let Some(data) = self.regions.right().closing_data() {
            return data.deadline;
        }
        let deadline = self.deadline().min(now + POST_EXIT_CLOSE_WAIT_CAP);
        assert!(
            self.regions
                .right_mut()
                .handle(collection::ExecOutputCollectionEvent::Close(deadline))
                .is_ok()
        );
        deadline
    }

    pub(super) fn append(&mut self, chunks: Vec<Vec<u8>>) {
        for chunk in chunks {
            self.data_mut().output.extend_from_slice(&chunk);
        }
    }

    pub(super) async fn wait_while_paused(&mut self) {
        let Some(receiver) = self.data_mut().pause.as_mut() else {
            return;
        };
        if receiver.has_changed().is_err() {
            self.data_mut().pause = None;
            return;
        }
        if !*receiver.borrow_and_update() {
            return;
        }
        let paused_at = Instant::now();
        let mut closed = false;
        while *receiver.borrow() {
            if receiver.changed().await.is_err() {
                closed = true;
                break;
            }
        }
        let paused_for = paused_at.elapsed();
        self.data_mut().deadline += paused_for;
        if closed {
            self.data_mut().pause = None;
        }
        if let Some(data) = self.regions.right_mut().closing_data_mut() {
            data.deadline += paused_for;
        }
    }

    pub(super) fn finish(mut self) -> Vec<u8> {
        let output = std::mem::take(&mut self.data_mut().output);
        assert!(
            self.regions
                .right_mut()
                .handle(collection::ExecOutputCollectionEvent::Finish)
                .is_ok()
        );
        output
    }
}

pub(super) struct StreamData {
    pub receiver: broadcast::Receiver<Vec<u8>>,
    pub pending: Vec<u8>,
    pub emitted_deltas: usize,
    pub transcript: Arc<Mutex<HeadTailBuffer>>,
    pub session: Arc<Session>,
    pub turn: Arc<TurnContext>,
    pub call_id: String,
}

impl std::fmt::Debug for StreamData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamData")
            .field("pending_bytes", &self.pending.len())
            .field("emitted_deltas", &self.emitted_deltas)
            .finish_non_exhaustive()
    }
}

mod streaming {
    use super::*;

    state_machine! {
        name: ExecOutputStream,
        dynamic: true,
        initial: Reading,
        states: [
            superstate Streaming(StreamData) {
                state Reading, state Trailing(CloseWait)
            },
            Complete
        ],
        final_states: [Complete],
        events {
            exit {
                payload: Instant,
                transition: { from: Reading, to: Trailing, data: trailing }
            }
            finish { transition: { from: Streaming, to: Complete } }
        }
    }

    impl<C, S> ExecOutputStream<C, S> {
        fn trailing(&self, deadline: &mut Instant) -> CloseWait {
            CloseWait {
                deadline: *deadline,
            }
        }
    }
}

pub(super) struct Stream {
    machine: streaming::DynamicExecOutputStream<()>,
}

impl Stream {
    pub(super) fn new(data: StreamData) -> Self {
        Self {
            machine: streaming::ExecOutputStream::new(())
                .with_streaming_data(data)
                .into_dynamic(),
        }
    }

    pub(super) fn data_mut(&mut self) -> &mut StreamData {
        self.machine
            .streaming_data_mut()
            .unwrap_or_else(|| unreachable!("stream owns output receiver and delivery data"))
    }

    pub(super) fn trailing_deadline(&self) -> Option<Instant> {
        self.machine.trailing_data().map(|data| data.deadline)
    }

    pub(super) fn observe_exit(&mut self, deadline: Instant) {
        assert!(
            self.machine
                .handle(streaming::ExecOutputStreamEvent::Exit(deadline))
                .is_ok()
        );
    }

    pub(super) fn finish(mut self) {
        assert!(
            self.machine
                .handle(streaming::ExecOutputStreamEvent::Finish)
                .is_ok()
        );
    }
}
