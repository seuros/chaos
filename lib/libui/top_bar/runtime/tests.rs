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
async fn machine_updates_do_not_block_the_clock_or_redraw_unchanged_content() {
    use crate::top_bar::tests::observations::{discharging, snapshot};

    let (source, receiver) = watch::channel(None);
    let calls = Arc::new(AtomicUsize::new(0));
    let (tx, mut frames) = broadcast::channel(16);
    let runtime = Runtime::start(
        FrameRequester::new(tx),
        vec![
            widgets::battery::new(receiver.clone()),
            probe(calls.clone()),
        ],
        Some(receiver),
        None,
        None,
    );
    tokio::task::yield_now().await;
    assert!(runtime.widgets.try_lock().is_ok());
    tokio::time::advance(Duration::from_secs(60)).await;
    frames.recv().await.unwrap();
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "clock timer runs before a machine read finishes"
    );
    source.send_replace(snapshot(discharging(Some(80))));
    frames.recv().await.unwrap();
    let line: String = runtime
        .buffer(7)
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert_eq!(line, " ● 80% ");
    source.send_replace(snapshot(discharging(Some(80))));
    tokio::task::yield_now().await;
    assert!(
        frames.try_recv().is_err(),
        "identical display must not redraw"
    );
    source.send_replace(None);
    frames.recv().await.unwrap();
    assert!(
        runtime
            .buffer(7)
            .content
            .iter()
            .all(|cell| cell.symbol() == " ")
    );
    drop(runtime);
    tokio::task::yield_now().await;
    assert_eq!(source.receiver_count(), 0);
}

#[tokio::test]
async fn sandbox_color_tracks_policy_without_changing_label() {
    use chaos_ipc::protocol::{NetworkAccess, SandboxPolicy};
    use chaos_sysinfo::SandboxKind;

    let palette = crate::theme::palette();
    for kind in [
        SandboxKind::Seatbelt,
        SandboxKind::Seccomp,
        SandboxKind::Capsicum,
    ] {
        let (tx, _) = broadcast::channel(16);
        let runtime =
            Runtime::with_widgets(FrameRequester::new(tx), vec![widgets::sandbox::new(kind)]);
        let initial = runtime.buffer(20);
        for (policy, color) in [
            (Some(SandboxPolicy::new_read_only_policy()), palette.success),
            (Some(SandboxPolicy::RootAccess), palette.error),
            (
                Some(SandboxPolicy::new_workspace_write_policy()),
                palette.success,
            ),
            (
                Some(SandboxPolicy::ExternalSandbox {
                    network_access: NetworkAccess::Restricted,
                }),
                palette.top_bar_fg,
            ),
            (None, palette.top_bar_fg),
        ] {
            // Policy changes while too narrow must still appear on expansion.
            runtime.buffer_with_sandbox_policy(1, policy.as_ref());
            let buffer = runtime.buffer_with_sandbox_policy(20, policy.as_ref());
            for (cell, original) in buffer.content.iter().zip(&initial.content) {
                assert_eq!(cell.symbol(), original.symbol());
                if cell.symbol() != " " {
                    assert_eq!(cell.fg, color);
                }
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn one_timer_refreshes_without_drawing_and_stops_on_drop() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (tx, mut rx) = broadcast::channel(16);
    let runtime = Runtime::with_widgets(FrameRequester::new(tx), vec![probe(Arc::clone(&calls))]);
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
