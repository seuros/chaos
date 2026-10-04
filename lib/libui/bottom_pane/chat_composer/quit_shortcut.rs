use crate::key_hint::{self, KeyBinding};
use crate::tui::FrameRequester;
use crossterm::event::KeyCode;
use state_machines::state_machine;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

pub(super) struct ArmedData {
    key: KeyBinding,
    expires_at: Instant,
    redraw: CancellationToken,
}

impl std::fmt::Debug for ArmedData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArmedData")
            .field("key", &self.key)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl Drop for ArmedData {
    fn drop(&mut self) {
        self.redraw.cancel();
    }
}

state_machine! {
    name: QuitAdmission,
    dynamic: true,
    initial: Idle,
    states: [Idle, Armed(ArmedData)],
    events {
        arm {
            payload: Option<ArmedData>,
            transition: { from: Idle, to: Armed, data: own_window }
            transition: { from: Armed, to: Armed, data: own_window }
        }
        expire {
            payload: Instant,
            transition: { from: Armed, to: Idle, guards: [expired] }
        }
        clear {
            transition: { from: Armed, to: Idle }
            transition: { from: Idle, internal: true }
        }
    }
}

impl<C, S> QuitAdmission<C, S> {
    fn own_window(&self, data: &mut Option<ArmedData>) -> ArmedData {
        data.take()
            .unwrap_or_else(|| unreachable!("arm owns quit window"))
    }
}

impl<C> QuitAdmission<C, Armed> {
    fn expired(&self, _ctx: &C, now: &Instant) -> bool {
        self.armed_data()
            .is_some_and(|data| *now >= data.expires_at)
    }
}

pub(super) struct QuitShortcut {
    machine: DynamicQuitAdmission<()>,
}

impl Default for QuitShortcut {
    fn default() -> Self {
        Self {
            machine: QuitAdmission::new(()).into_dynamic(),
        }
    }
}

impl QuitShortcut {
    pub(super) fn arm(
        &mut self,
        key: KeyBinding,
        expires_at: Instant,
        frame_requester: Option<FrameRequester>,
    ) {
        assert!(
            self.machine
                .handle(QuitAdmissionEvent::Arm(Some(ArmedData {
                    key,
                    expires_at,
                    redraw: CancellationToken::new(),
                })))
                .is_ok()
        );
        let Some(frame_requester) = frame_requester else {
            return;
        };
        let redraw = self
            .machine
            .armed_data()
            .unwrap_or_else(|| unreachable!("armed shortcut owns redraw"))
            .redraw
            .clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                tokio::select! {
                    biased;
                    _ = redraw.cancelled() => {}
                    _ = tokio::time::sleep_until(expires_at.into()) => {
                        frame_requester.schedule_frame();
                    }
                }
            });
        } else {
            std::thread::spawn(move || {
                std::thread::sleep(expires_at.saturating_duration_since(Instant::now()));
                if !redraw.is_cancelled() {
                    frame_requester.schedule_frame();
                }
            });
        }
    }

    pub(super) fn clear(&mut self) {
        assert!(self.machine.handle(QuitAdmissionEvent::Clear).is_ok());
    }

    pub(super) fn visible_at(&self, now: Instant) -> bool {
        self.machine
            .armed_data()
            .is_some_and(|data| now < data.expires_at)
    }

    pub(super) fn active_for(&mut self, key: KeyBinding, now: Instant) -> bool {
        if !self.visible_at(now) {
            if self.machine.armed_data().is_some() {
                assert!(self.machine.handle(QuitAdmissionEvent::Expire(now)).is_ok());
            }
            return false;
        }
        self.key() == key
    }

    pub(super) fn key(&self) -> KeyBinding {
        self.machine
            .armed_data()
            .map_or(key_hint::ctrl(KeyCode::Char('c')), |data| data.key)
    }
}
