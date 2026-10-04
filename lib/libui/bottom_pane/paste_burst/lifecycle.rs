use super::{PASTE_BURST_CHAR_INTERVAL, PASTE_ENTER_SUPPRESS_WINDOW};
use state_machines::{runtime::Parallel, state_machine};
use std::time::Instant;

#[derive(Default)]
struct BufferData {
    text: String,
}

impl std::fmt::Debug for BufferData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferData")
            .field("bytes", &self.text.len())
            .finish()
    }
}

#[derive(Clone, Copy)]
struct OwnedChar {
    ch: char,
    at: Instant,
}

impl std::fmt::Debug for OwnedChar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnedChar(..)")
    }
}

#[derive(Debug)]
struct Sample {
    at: Instant,
    count: u16,
}

mod buffer {
    use super::*;
    state_machine! {
        name: BurstClassification,
        dynamic: true,
        initial: Idle,
        states: [superstate Text(BufferData) { state Idle, state Buffering }],
        events {
            begin { transition: { from: Text, to: Buffering } }
            flush { transition: { from: Text, to: Idle } }
        }
    }
}

mod held {
    use super::*;
    state_machine! {
        name: HeldInput,
        dynamic: true,
        initial: Idle,
        states: [Idle, Holding(OwnedChar)],
        events {
            hold {
                payload: Option<OwnedChar>,
                transition: { from: Idle, to: Holding, data: own_char }
                transition: { from: Holding, to: Holding, data: own_char }
            }
            clear {
                transition: { from: Holding, to: Idle }
                transition: { from: Idle, internal: true }
            }
        }
    }

    impl<C, S> HeldInput<C, S> {
        fn own_char(&self, data: &mut Option<OwnedChar>) -> OwnedChar {
            data.take()
                .unwrap_or_else(|| unreachable!("hold owns character"))
        }
    }
}

mod sampling {
    use super::*;
    state_machine! {
        name: CharSampling,
        dynamic: true,
        initial: Quiet,
        states: [Quiet, Tracking(Sample)],
        events {
            start {
                payload: Option<Sample>,
                transition: { from: Quiet, to: Tracking, data: own_sample }
            }
            clear {
                transition: { from: Tracking, to: Quiet }
                transition: { from: Quiet, internal: true }
            }
        }
    }

    impl<C, S> CharSampling<C, S> {
        fn own_sample(&self, data: &mut Option<Sample>) -> Sample {
            data.take()
                .unwrap_or_else(|| unreachable!("sampling owns timing"))
        }
    }
}

mod suppression {
    use super::*;
    state_machine! {
        name: EnterSuppression,
        dynamic: true,
        initial: Quiet,
        states: [Quiet, Open(Instant)],
        events {
            extend {
                payload: Option<Instant>,
                transition: { from: Quiet, to: Open, data: own_deadline }
                transition: { from: Open, to: Open, data: own_deadline }
            }
            clear {
                transition: { from: Open, to: Quiet }
                transition: { from: Quiet, internal: true }
            }
        }
    }

    impl<C, S> EnterSuppression<C, S> {
        fn own_deadline(&self, data: &mut Option<Instant>) -> Instant {
            data.take()
                .unwrap_or_else(|| unreachable!("suppression owns deadline"))
        }
    }
}

type TextRegions = Parallel<held::DynamicHeldInput<()>, buffer::DynamicBurstClassification<()>>;
type TimingRegions =
    Parallel<sampling::DynamicCharSampling<()>, suppression::DynamicEnterSuppression<()>>;

pub(super) struct Lifecycle {
    regions: Parallel<TextRegions, TimingRegions>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            regions: Parallel::new(
                Parallel::new(
                    held::DynamicHeldInput::new(()),
                    buffer::BurstClassification::new(())
                        .with_text_data(BufferData::default())
                        .into_dynamic(),
                ),
                Parallel::new(
                    sampling::DynamicCharSampling::new(()),
                    suppression::DynamicEnterSuppression::new(()),
                ),
            ),
        }
    }
}

impl Lifecycle {
    pub(super) fn buffering(&self) -> bool {
        self.regions.left().right().current_state() == buffer::BurstClassificationState::Buffering
    }

    pub(super) fn buffer(&self) -> &str {
        &self
            .regions
            .left()
            .right()
            .text_data()
            .unwrap_or_else(|| unreachable!("text owner retains buffer"))
            .text
    }

    pub(super) fn buffer_mut(&mut self) -> &mut String {
        &mut self
            .regions
            .left_mut()
            .right_mut()
            .text_data_mut()
            .unwrap_or_else(|| unreachable!("text owner retains buffer"))
            .text
    }

    pub(super) fn begin(&mut self) {
        assert!(
            self.regions
                .left_mut()
                .right_mut()
                .handle(buffer::BurstClassificationEvent::Begin)
                .is_ok()
        );
    }

    pub(super) fn flush_buffer(&mut self) -> String {
        let out = std::mem::take(self.buffer_mut());
        self.reset_classification();
        out
    }

    fn reset_classification(&mut self) {
        assert!(
            self.regions
                .left_mut()
                .right_mut()
                .handle(buffer::BurstClassificationEvent::Flush)
                .is_ok()
        );
    }

    pub(super) fn held(&self) -> Option<(char, Instant)> {
        self.regions
            .left()
            .left()
            .holding_data()
            .map(|data| (data.ch, data.at))
    }

    pub(super) fn hold(&mut self, ch: char, at: Instant) {
        assert!(
            self.regions
                .left_mut()
                .left_mut()
                .handle(held::HeldInputEvent::Hold(Some(OwnedChar { ch, at })))
                .is_ok()
        );
    }

    pub(super) fn take_held(&mut self) -> Option<char> {
        let ch = self.held().map(|(ch, _)| ch);
        assert!(
            self.regions
                .left_mut()
                .left_mut()
                .handle(held::HeldInputEvent::Clear)
                .is_ok()
        );
        ch
    }

    pub(super) fn last_plain_char_time(&self) -> Option<Instant> {
        self.regions
            .right()
            .left()
            .tracking_data()
            .map(|data| data.at)
    }

    pub(super) fn consecutive_chars(&self) -> u16 {
        self.regions
            .right()
            .left()
            .tracking_data()
            .map_or(0, |data| data.count)
    }

    pub(super) fn note_plain_char(&mut self, now: Instant) {
        self.expire_window(now);
        let machine = self.regions.right_mut().left_mut();
        if let Some(data) = machine.tracking_data_mut() {
            data.count = if now.duration_since(data.at) <= PASTE_BURST_CHAR_INTERVAL {
                data.count.saturating_add(1)
            } else {
                1
            };
            data.at = now;
        } else {
            assert!(
                machine
                    .handle(sampling::CharSamplingEvent::Start(Some(Sample {
                        at: now,
                        count: 1
                    })))
                    .is_ok()
            );
        }
    }

    pub(super) fn suppresses_enter(&self, now: Instant) -> bool {
        self.regions
            .right()
            .right()
            .open_data()
            .is_some_and(|until| now <= *until)
    }

    pub(super) fn extend_window(&mut self, now: Instant) {
        assert!(
            self.regions
                .right_mut()
                .right_mut()
                .handle(suppression::EnterSuppressionEvent::Extend(Some(
                    now + PASTE_ENTER_SUPPRESS_WINDOW
                )))
                .is_ok()
        );
    }

    pub(super) fn expire_window(&mut self, now: Instant) {
        if self
            .regions
            .right()
            .right()
            .open_data()
            .is_some_and(|until| now > *until)
        {
            self.clear_suppression();
        }
    }

    fn clear_suppression(&mut self) {
        assert!(
            self.regions
                .right_mut()
                .right_mut()
                .handle(suppression::EnterSuppressionEvent::Clear)
                .is_ok()
        );
    }

    pub(super) fn clear_window(&mut self) {
        assert!(
            self.regions
                .right_mut()
                .left_mut()
                .handle(sampling::CharSamplingEvent::Clear)
                .is_ok()
        );
        self.clear_suppression();
        self.reset_classification();
        self.take_held();
    }
}

#[cfg(test)]
mod tests;
