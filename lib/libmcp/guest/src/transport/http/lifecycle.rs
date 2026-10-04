use state_machines::state_machine;
use std::time::Duration;
use tokio::task::{JoinHandle, JoinSet};

#[derive(Debug, Default)]
pub(super) struct SessionData {
    pub cached_initialize: Option<super::JsonRpcMessage>,
    pub session_id: Option<String>,
    pub negotiated_version: Option<String>,
    pub last_event_id: Option<String>,
    pub recovered_generation: Option<u64>,
    pub streams: JoinSet<()>,
}

impl SessionData {
    pub fn clear(&mut self) {
        self.session_id = None;
        self.negotiated_version = None;
        self.last_event_id = None;
        self.recovered_generation = None;
    }
}

#[derive(Debug)]
pub(super) struct StreamData {
    pub task: Option<JoinHandle<()>>,
    pub reconnect_delay: Duration,
    pub reconnect_attempt: u32,
}

impl Drop for StreamData {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

mod session {
    use super::*;
    state_machine! {
        name: HttpSession,
        dynamic: true,
        initial: Fresh,
        states: [
            superstate Open(SessionData) {
                state Fresh, state Initializing, state Negotiated, state Ready,
                state RecoveringFresh, state RecoveringReady,
            },
            Closed,
        ],
        events {
            begin_initialization {
                transition: { from: Fresh, to: Initializing }
                transition: { from: Initializing, to: Initializing }
                transition: { from: Negotiated, to: Initializing }
                transition: { from: Ready, to: Ready }
                transition: { from: RecoveringFresh, to: RecoveringFresh }
                transition: { from: RecoveringReady, to: RecoveringReady }
            }
            negotiated {
                transition: { from: Fresh, to: Negotiated }
                transition: { from: Initializing, to: Negotiated }
                transition: { from: Negotiated, to: Negotiated }
                transition: { from: Ready, to: Ready }
                transition: { from: RecoveringFresh, to: RecoveringFresh }
                transition: { from: RecoveringReady, to: RecoveringReady }
            }
            initialized {
                transition: { from: Fresh, to: Ready }
                transition: { from: Initializing, to: Ready }
                transition: { from: Negotiated, to: Ready }
                transition: { from: Ready, to: Ready }
                transition: { from: RecoveringFresh, to: RecoveringReady }
                transition: { from: RecoveringReady, to: RecoveringReady }
            }
            recover {
                transition: { from: Fresh, to: RecoveringFresh }
                transition: { from: Initializing, to: RecoveringFresh }
                transition: { from: Negotiated, to: RecoveringFresh }
                transition: { from: Ready, to: RecoveringReady }
                transition: { from: RecoveringFresh, to: RecoveringFresh }
                transition: { from: RecoveringReady, to: RecoveringReady }
            }
            restored {
                transition: { from: RecoveringFresh, to: Negotiated }
                transition: { from: RecoveringReady, to: Ready }
            }
            close {
                transition: { from: Open, to: Closed }
                transition: { from: Closed, to: Closed }
            }
        }
    }
}

mod sse {
    use super::*;
    state_machine! {
        name: HttpSse,
        dynamic: true,
        initial: Idle,
        states: [
            superstate Enabled(StreamData) { state Idle, state Connecting, state Streaming, state Backoff },
            Disabled, Stopped,
        ],
        events {
            connect { transition: { from: Enabled, to: Connecting } }
            connected { transition: { from: Connecting, to: Streaming } }
            retry { transition: { from: Enabled, to: Backoff } }
            disable { transition: { from: Enabled, to: Disabled } }
            stop {
                transition: { from: Enabled, to: Idle }
                transition: { from: Disabled, to: Disabled }
                transition: { from: Stopped, to: Stopped }
            }
            close {
                transition: { from: Enabled, to: Stopped }
                transition: { from: Disabled, to: Stopped }
                transition: { from: Stopped, to: Stopped }
            }
        }
    }
}

pub(super) use session::HttpSessionEvent;
pub(super) use sse::HttpSseEvent;

pub(super) struct Lifecycle {
    session: session::DynamicHttpSession<()>,
    sse: sse::DynamicHttpSse<()>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new(Duration::from_millis(500))
    }
}

impl Lifecycle {
    pub(super) fn new(reconnect_delay: Duration) -> Self {
        Self {
            session: session::HttpSession::new(())
                .with_open_data(SessionData::default())
                .into_dynamic(),
            sse: sse::HttpSse::new(())
                .with_enabled_data(StreamData {
                    task: None,
                    reconnect_delay,
                    reconnect_attempt: 0,
                })
                .into_dynamic(),
        }
    }

    pub(super) fn data(&self) -> Option<&SessionData> {
        self.session.open_data()
    }
    pub(super) fn data_mut(&mut self) -> Option<&mut SessionData> {
        self.session.open_data_mut()
    }
    pub(super) fn stream(&mut self) -> Option<&mut StreamData> {
        self.sse.enabled_data_mut()
    }
    pub(super) fn session(&mut self, event: HttpSessionEvent) -> bool {
        self.session.handle(event).is_ok()
    }
    pub(super) fn sse(&mut self, event: HttpSseEvent) -> bool {
        self.sse.handle(event).is_ok()
    }
    pub(super) fn initialized(&self) -> bool {
        matches!(
            self.session.current_state(),
            session::HttpSessionState::Ready | session::HttpSessionState::RecoveringReady
        )
    }
    pub(super) fn sse_enabled(&self) -> bool {
        !matches!(
            self.sse.current_state(),
            sse::HttpSseState::Disabled | sse::HttpSseState::Stopped
        )
    }
    pub(super) fn close(&mut self) {
        assert!(self.session(HttpSessionEvent::Close));
        assert!(self.sse(HttpSseEvent::Close));
    }
}

#[cfg(test)]
mod tests;
