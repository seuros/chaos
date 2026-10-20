use super::*;
use protocol::schema::v1::{SetSessionConfigOptionRequest, SetSessionModeRequest};

#[tokio::test]
async fn unsupported_controls_do_not_invalidate_the_session() {
    let (connection, mut updates, task, _) = start(ConnectionLimits::default()).await;
    let id = session(&connection).await;
    assert!(
        connection
            .client()
            .request(SetSessionConfigOptionRequest::new(
                id.clone(),
                "model",
                "other",
            ))
            .await
            .is_err()
    );
    assert!(
        connection
            .client()
            .request(SetSessionModeRequest::new(id.clone(), "plan",))
            .await
            .is_err()
    );
    assert_eq!(
        connection
            .client()
            .request(prompt(&id, "after"))
            .await
            .unwrap()
            .stop_reason,
        StopReason::EndTurn
    );
    updates.recv().await.unwrap();
    connection.disconnect().await.unwrap();
    assert!(task.await.unwrap().is_ok());
}

#[tokio::test]
async fn busy_configuration_cannot_interfere_with_prompt_cancellation() {
    let (connection, mut updates, task, observations) = start(ConnectionLimits::default()).await;
    let id = session(&connection).await;
    let client = connection.client();
    let pending = {
        let client = client.clone();
        let id = id.clone();
        tokio::spawn(async move { client.request(prompt(&id, "wait")).await })
    };
    updates.recv().await.unwrap();
    assert!(
        client
            .request(SetSessionModeRequest::new(id.clone(), "plan"))
            .await
            .is_err()
    );
    assert!(
        client
            .request(SetSessionConfigOptionRequest::new(
                id.clone(),
                "model",
                "other"
            ))
            .await
            .is_err()
    );
    client.notify(CancelNotification::new(id.clone())).unwrap();
    assert_eq!(
        pending.await.unwrap().unwrap().stop_reason,
        StopReason::Cancelled
    );
    assert_eq!(observations.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(
        client
            .request(prompt(&id, "after"))
            .await
            .unwrap()
            .stop_reason,
        StopReason::EndTurn
    );
    updates.recv().await.unwrap();
    connection.disconnect().await.unwrap();
    assert!(task.await.unwrap().is_ok());
}

#[tokio::test]
async fn dropping_serve_during_backend_metadata_still_shuts_it_down() {
    let (connection, _updates, task, observations) = start(ConnectionLimits::default()).await;
    observations.wait_metadata.store(true, Ordering::Release);
    let client = connection.client();
    let pending = tokio::spawn(async move {
        client
            .request(NewSessionRequest::new(std::path::PathBuf::from(
                "/workspace",
            )))
            .await
    });
    observations.metadata_started.notified().await;
    task.abort();
    let _ = task.await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while observations.shutdowns.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(observations.shutdowns.load(Ordering::SeqCst), 1);
    let _ = pending.await;
    let _ = connection.disconnect().await;
}
