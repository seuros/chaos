//! Shared timers, background reads, and kernel events for cached widget state.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use chaos_kern::PersistenceStatus;
use jiff::Zoned;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::WidgetRef;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::Bar;
use super::BarWidget;
use super::machine;
use super::widgets;
use crate::tui::FrameRequester;

type EnvironmentTask = JoinHandle<Vec<BarWidget>>;

pub(crate) struct Runtime {
    widgets: Arc<Mutex<Vec<BarWidget>>>,
    task: JoinHandle<()>,
    machine: Option<machine::Monitor>,
}

impl Runtime {
    pub(crate) fn new(
        requester: FrameRequester,
        context: machine::Context,
        activity: watch::Receiver<crate::activity::Snapshot>,
    ) -> Self {
        let monitor = machine::Monitor::new(context);
        let persistence = chaos_kern::subscribe_persistence_status();
        let environment =
            tokio::task::spawn_blocking(|| widgets::environment_widgets(chaos_sysinfo::sysinfo()));
        let mut initial_widgets = widgets::activity::new(activity.clone());
        initial_widgets.extend(widgets::initial_widgets(
            chaos_sysinfo::hostname(),
            monitor.source.clone(),
            persistence.clone(),
        ));
        let mut runtime = Self::start(
            requester,
            initial_widgets,
            Some(monitor.source.clone()),
            Some(persistence),
            Some(environment),
            Some(activity),
        );
        runtime.machine = Some(monitor);
        runtime
    }

    #[cfg(test)]
    fn with_widgets(requester: FrameRequester, widgets: Vec<BarWidget>) -> Self {
        Self::start(requester, widgets, None, None, None, None)
    }

    fn start(
        requester: FrameRequester,
        mut widgets: Vec<BarWidget>,
        mut machine: Option<machine::Source>,
        mut persistence: Option<watch::Receiver<PersistenceStatus>>,
        mut environment: Option<EnvironmentTask>,
        mut activity: Option<watch::Receiver<crate::activity::Snapshot>>,
    ) -> Self {
        let sampled_at = tokio::time::Instant::now();
        let (_, next) = refresh(&mut widgets, &Zoned::now());
        let mut next = next.map(|delay| sampled_at + delay);
        let widgets = Arc::new(Mutex::new(widgets));
        let state = Arc::clone(&widgets);
        let task = tokio::spawn(async move {
            loop {
                let mut widgets_added = false;
                tokio::select! {
                    _ = wait_for(next.map(tokio::time::sleep_until)) => {}
                    result = wait_for(activity.as_mut().map(watch::Receiver::changed)) => {
                        if result.is_err() {
                            activity = None;
                            continue;
                        }
                    }
                    result = wait_for(persistence.as_mut().map(watch::Receiver::changed)) => {
                        if result.is_err() {
                            // A closed source must not become a busy loop.
                            persistence = None;
                            continue;
                        }
                    }
                    result = wait_for(environment.as_mut()) => {
                        environment = None;
                        match result {
                            Ok(additional) => {
                                widgets_added = !additional.is_empty();
                                // Layout groups by side, retaining declaration order.
                                // These left-side widgets follow the existing hostname.
                                state.lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .extend(additional);
                            }
                            Err(error) => {
                                tracing::warn!(%error, "top bar environment worker failed");
                                continue;
                            }
                        }
                    }
                    result = wait_for(machine.as_mut().map(watch::Receiver::changed)) => {
                        if result.is_err() {
                            machine = None;
                            continue;
                        }
                    }
                }
                // Re-sample wall time after every wake, including after suspend.
                // Do not replay missed ticks or advance a cached clock by one.
                let sampled_at = tokio::time::Instant::now();
                let (changed, delay) = refresh(
                    &mut state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    &Zoned::now(),
                );
                next = delay.map(|delay| sampled_at + delay);
                if changed || widgets_added {
                    requester.schedule_frame();
                }
            }
        });
        Self {
            widgets,
            task,
            machine: None,
        }
    }

    #[cfg(test)]
    fn buffer(&self, width: u16) -> Buffer {
        self.buffer_with_sandbox_policy(width, None)
    }

    pub(crate) fn buffer_with_sandbox_policy(
        &self,
        width: u16,
        policy: Option<&chaos_ipc::protocol::SandboxPolicy>,
    ) -> Buffer {
        let area = Rect::new(0, 0, width, 1);
        let mut buffer = Buffer::empty(area);
        let mut widgets = self
            .widgets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(widget) = widgets
            .iter_mut()
            .find(|widget| widget.spec().id == "sandbox")
        {
            widget.set_tone(widgets::sandbox::tone(policy));
        }
        Bar {
            widgets: &widgets,
            palette: crate::theme::palette(),
        }
        .render_ref(area, &mut buffer);
        buffer
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Absent sources stay dormant without duplicating optional-future handling in select.
async fn wait_for<F: std::future::Future>(future: Option<F>) -> F::Output {
    match future {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

fn refresh(widgets: &mut [BarWidget], now: &Zoned) -> (bool, Option<Duration>) {
    let mut changed = false;
    let mut next = None;
    for widget in widgets {
        let update = widget.refresh(now);
        if update.changed {
            tracing::trace!(widget = widget.spec().id, "top bar widget updated");
            changed = true;
        }
        if let Some(delay) = update.next {
            next = Some(next.map_or(delay, |current: Duration| current.min(delay)));
        }
    }
    (changed, next)
}

#[cfg(test)]
mod tests;
