use super::*;
use protocol::schema::v1::CloseSessionRequest;

#[tokio::test]
async fn close_cancels_pending_prompt_joins_once_and_releases_admission() {
    let (connection, mut updates, task, observations) = start(ConnectionLimits {
        max_sessions: 1,
        ..Default::default()
    })
    .await;
    assert!(
        connection
            .initialized()
            .agent_capabilities
            .session_capabilities
            .close
            .is_some()
    );
    let id = session(&connection).await;
    let client = connection.client();
    let pending = {
        let client = client.clone();
        let id = id.clone();
        tokio::spawn(async move { client.request(prompt(&id, "wait")).await })
    };
    updates.recv().await.unwrap();
    let stale = observations.context.lock().unwrap().clone().unwrap();
    client
        .request(CloseSessionRequest::new(id.clone()))
        .await
        .unwrap();
    assert_eq!(
        pending.await.unwrap().unwrap().stop_reason,
        StopReason::Cancelled
    );
    assert_eq!(observations.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(observations.shutdowns.load(Ordering::SeqCst), 1);
    assert!(stale.client().update(message("late")).is_err());
    assert!(client.request(prompt(&id, "closed")).await.is_err());
    assert!(client.request(CloseSessionRequest::new(id)).await.is_err());
    session(&connection).await;
    connection.disconnect().await.unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(observations.shutdowns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn close_gates_new_work_until_cancellation_has_joined() {
    let (connection, mut updates, task, observations) = start(ConnectionLimits::default()).await;
    observations.slow_cancel.store(true, Ordering::SeqCst);
    let id = session(&connection).await;
    let client = connection.client();
    let pending = {
        let client = client.clone();
        let id = id.clone();
        tokio::spawn(async move { client.request(prompt(&id, "wait")).await })
    };
    updates.recv().await.unwrap();
    let closing = {
        let client = client.clone();
        let id = id.clone();
        tokio::spawn(async move { client.request(CloseSessionRequest::new(id)).await })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while observations.cancels.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!closing.is_finished());
    assert!(client.request(prompt(&id, "no")).await.is_err());
    assert!(client.request(CloseSessionRequest::new(id)).await.is_err());
    observations.cancel_release.notify_one();
    pending.await.unwrap().unwrap();
    closing.await.unwrap().unwrap();
    connection.disconnect().await.unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(observations.shutdowns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_close_never_reports_success_or_repeats_unknown_shutdown() {
    let (connection, _updates, task, observations) = start(ConnectionLimits::default()).await;
    let id = session(&connection).await;
    observations.fail_shutdown.store(true, Ordering::SeqCst);
    assert!(
        connection
            .client()
            .request(CloseSessionRequest::new(id))
            .await
            .is_err()
    );
    assert!(task.await.unwrap().is_err());
    assert_eq!(observations.shutdowns.load(Ordering::SeqCst), 1);
}
