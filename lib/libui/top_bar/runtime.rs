//! Shared timers, background reads, and kernel events for cached widget state.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use chaos_kern::PersistenceStatus;
use chaos_sysinfo::PowerInfo;
use jiff::Zoned;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::WidgetRef;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::Bar;
use super::BarWidget;
use super::widgets;
use crate::tui::FrameRequester;

type EnvironmentTask = JoinHandle<Vec<BarWidget>>;

pub(crate) struct Runtime {
    widgets: Arc<Mutex<Vec<BarWidget>>>,
    task: JoinHandle<()>,
}

impl Runtime {
    pub(crate) fn new(requester: FrameRequester) -> Self {
        let (power_tx, power_rx) = watch::channel(PowerInfo::default());
        let persistence = chaos_kern::subscribe_persistence_status();
        let environment =
            tokio::task::spawn_blocking(|| widgets::environment_widgets(chaos_sysinfo::sysinfo()));
        Self::start(
            requester,
            widgets::initial_widgets(chaos_sysinfo::hostname(), power_rx, persistence.clone()),
            Some(power_tx),
            Some(persistence),
            Some(environment),
        )
    }

    #[cfg(test)]
    fn with_widgets(requester: FrameRequester, widgets: Vec<BarWidget>) -> Self {
        Self::start(requester, widgets, None, None, None)
    }

    fn start(
        requester: FrameRequester,
        mut widgets: Vec<BarWidget>,
        power: Option<watch::Sender<PowerInfo>>,
        mut persistence: Option<watch::Receiver<PersistenceStatus>>,
        mut environment: Option<EnvironmentTask>,
    ) -> Self {
        let sampled_at = tokio::time::Instant::now();
        let (_, next) = refresh(&mut widgets, &Zoned::now());
        let mut next = next.map(|delay| sampled_at + delay);
        let widgets = Arc::new(Mutex::new(widgets));
        let state = Arc::clone(&widgets);
        let task = tokio::spawn(async move {
            let mut power_ticks = tokio::time::interval(Duration::from_secs(30));
            power_ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut power_read: Option<JoinHandle<PowerInfo>> = None;
            loop {
                let mut widgets_added = false;
                tokio::select! {
                    _ = wait_for(next.map(tokio::time::sleep_until)) => {}
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
                    _ = power_ticks.tick(), if power.is_some() && power_read.is_none() => {
                        // OS I/O never holds the widget lock or blocks the clock.
                        // At most one read is in flight; missed polls are not replayed.
                        power_read = Some(tokio::task::spawn_blocking(chaos_sysinfo::power_info));
                        continue;
                    }
                    result = wait_for(power_read.as_mut()) => {
                        power_read = None;
                        match result {
                            Ok(snapshot) => {
                                if let Some(sender) = &power {
                                    sender.send_replace(snapshot);
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, "power snapshot worker failed");
                                continue;
                            }
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
        Self { widgets, task }
    }

    pub(crate) fn buffer(&self, width: u16) -> Buffer {
        let area = Rect::new(0, 0, width, 1);
        let mut buffer = Buffer::empty(area);
        let widgets = self
            .widgets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use tokio::sync::broadcast;

    use crate::top_bar::Content;
    use crate::top_bar::Side;
    use crate::top_bar::Update;

    fn probe(calls: Arc<AtomicUsize>) -> BarWidget {
        BarWidget::text("probe", Side::Right, 0, Content::new(" ")).with_refresh(move |_, _| {
            let count = calls.fetch_add(1, Ordering::Relaxed);
            Update {
                changed: count == 1,
                next: Some(Duration::from_secs(60)),
            }
        })
    }

    #[tokio::test(start_paused = true)]
    async fn one_timer_refreshes_without_drawing_and_stops_on_drop() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (tx, mut rx) = broadcast::channel(16);
        let runtime =
            Runtime::with_widgets(FrameRequester::new(tx), vec![probe(Arc::clone(&calls))]);
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        tokio::time::advance(Duration::from_secs(59)).await;
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        // Receiving the redraw also lets both the updater and frame scheduler run.
        rx.recv().await.expect("changed state requests a frame");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        assert!(rx.try_recv().is_err(), "unchanged state must not redraw");

        let state = Arc::downgrade(&runtime.widgets);
        drop(runtime);
        tokio::task::yield_now().await;
        assert!(
            state.upgrade().is_none(),
            "drop releases the updater's state"
        );
        tokio::time::advance(Duration::from_secs(120)).await;
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn persistence_events_update_while_hidden_and_release_subscriptions() {
        use chaos_kern::PersistenceHealth;

        let (status_tx, status_rx) = watch::channel(PersistenceStatus::default());
        let (frame_tx, mut frames) = broadcast::channel(16);
        let runtime = Runtime::start(
            FrameRequester::new(frame_tx),
            vec![
                widgets::storage::new(status_rx.clone()),
                widgets::persistence::new(status_rx.clone()),
            ],
            None,
            Some(status_rx),
            None,
        );
        assert_eq!(status_tx.receiver_count(), 3);
        assert!(
            runtime
                .buffer(3)
                .content
                .iter()
                .all(|cell| cell.symbol() == " ")
        );
        tokio::task::yield_now().await;
        assert!(
            frames.try_recv().is_err(),
            "initial unchanged state does not redraw"
        );

        for (health, expected) in [
            (PersistenceHealth::Degraded, " ⚠ log "),
            (PersistenceHealth::Failed, " ⚠ log "),
            (PersistenceHealth::Healthy, "       "),
        ] {
            status_tx.send_modify(|status| status.health = health);
            tokio::time::timeout(Duration::from_secs(1), frames.recv())
                .await
                .expect("status change must wake an idle runtime")
                .expect("redraw");
            let line: String = runtime
                .buffer(7)
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect();
            assert_eq!(line, expected);
            status_tx.send_modify(|status| status.health = health);
            tokio::task::yield_now().await;
            assert!(
                frames.try_recv().is_err(),
                "duplicate snapshots must not redraw"
            );
        }
        let state = Arc::downgrade(&runtime.widgets);
        drop(runtime);
        tokio::task::yield_now().await;
        assert!(state.upgrade().is_none());
        assert_eq!(status_tx.receiver_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn static_environment_loading_does_not_block_timers_or_rendering() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let environment: EnvironmentTask = tokio::spawn(async move {
            ready_rx.await.unwrap();
            vec![widgets::os::new("linux", "")]
        });
        let (tx, mut frames) = broadcast::channel(16);
        let runtime = Runtime::start(
            FrameRequester::new(tx),
            vec![probe(Arc::clone(&calls))],
            None,
            None,
            Some(environment),
        );
        tokio::task::yield_now().await;
        assert!(
            runtime.widgets.try_lock().is_ok(),
            "waiting for I/O holds no render lock"
        );
        assert_eq!(runtime.buffer(20).area.width, 20);
        tokio::time::advance(Duration::from_secs(60)).await;
        frames
            .recv()
            .await
            .expect("timer runs while environment is pending");
        assert_eq!(calls.load(Ordering::Relaxed), 2);

        ready_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), frames.recv())
            .await
            .expect("new static widgets request a frame")
            .expect("redraw");
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        let line: String = runtime
            .buffer(20)
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(line.contains("linux"));
    }
}
