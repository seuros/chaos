use super::*;
use crate::top_bar::tests::observations::{discharging, machine_status};
use chaos_kern::config::ConfigBuilder;
use tokio::sync::{mpsc, oneshot};

#[tokio::test]
async fn monitor_coalesces_slow_reads_discards_old_workspaces_and_clears_failures() {
    let home = tempfile::tempdir().unwrap();
    let config = ConfigBuilder::default()
        .chaos_home(home.path().to_path_buf())
        .fallback_cwd(Some(home.path().to_path_buf()))
        .build()
        .await
        .unwrap();
    let first = ObservationRequest::new(&config, &home.path().join("first"));
    let second = ObservationRequest::new(&config, &home.path().join("second"));
    tokio::time::pause();
    let (context, receiver) = watch::channel(None);
    let (started, mut reads) = mpsc::unbounded_channel();
    let monitor = Monitor::start(receiver, move |request| {
        let (reply, result) = oneshot::channel();
        started.send((request, reply)).unwrap();
        async move { result.await.unwrap() }
    });
    let mut snapshots = monitor.source.clone();
    tokio::task::yield_now().await;
    assert!(
        reads.try_recv().is_err(),
        "no config means no guessed disk probes"
    );
    context.send_replace(Some(first.clone()));
    let (request, reply) = reads.recv().await.unwrap();
    assert_eq!(request, first);
    snapshots.borrow_and_update();
    reply
        .send(Ok(machine_status(discharging(Some(80)))))
        .unwrap();
    snapshots.changed().await.unwrap();
    assert!(snapshots.borrow_and_update().is_some());

    tokio::time::advance(Duration::from_secs(29)).await;
    tokio::task::yield_now().await;
    assert!(reads.try_recv().is_err());
    tokio::time::advance(Duration::from_secs(1)).await;
    let (_, stale_reply) = reads.recv().await.unwrap();
    tokio::time::advance(Duration::from_secs(300)).await;
    tokio::task::yield_now().await;
    assert!(
        reads.try_recv().is_err(),
        "no overlapping reads or catch-up burst"
    );

    context.send_replace(Some(second.clone()));
    let (request, reply) = reads.recv().await.unwrap();
    assert_eq!(request, second);
    assert!(
        snapshots.borrow_and_update().is_none(),
        "old workspace data is cleared"
    );
    assert!(
        stale_reply
            .send(Ok(machine_status(discharging(Some(1)))))
            .is_err()
    );
    reply
        .send(Ok(machine_status(discharging(Some(90)))))
        .unwrap();
    snapshots.changed().await.unwrap();
    assert_eq!(
        snapshots
            .borrow_and_update()
            .as_ref()
            .unwrap()
            .machine
            .power
            .batteries
            .as_ref()
            .unwrap()[0]
            .charge_percent,
        Some(90)
    );

    tokio::time::advance(Duration::from_secs(30)).await;
    let (_, reply) = reads.recv().await.unwrap();
    reply.send(Err("probe timed out".into())).unwrap();
    snapshots.changed().await.unwrap();
    assert!(
        snapshots.borrow_and_update().is_none(),
        "timeout must not preserve stale health"
    );

    tokio::time::advance(Duration::from_secs(30)).await;
    let (_, reply) = reads.recv().await.unwrap();
    drop(monitor);
    tokio::task::yield_now().await;
    assert_eq!(context.receiver_count(), 0);
    assert!(
        reply
            .send(Ok(machine_status(discharging(Some(50)))))
            .is_err()
    );
    assert!(snapshots.changed().await.is_err());
}
