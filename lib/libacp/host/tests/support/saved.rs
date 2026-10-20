use super::*;
use protocol::schema::v1::LoadSessionRequest;

struct SavedBackend(Arc<Observations>);
struct Replay(Arc<Observations>);

impl AgentBackend for SavedBackend {
    fn load_session_supported(&self) -> bool {
        true
    }

    fn new_session(
        &self,
        _request: NewSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>> {
        Box::pin(async { Err(protocol::Error::invalid_params()) })
    }

    fn load_session(
        &self,
        _request: LoadSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>> {
        Box::pin(async { Ok(Arc::new(Replay(self.0.clone())) as Arc<dyn AgentSession>) })
    }
}

impl AgentSession for Replay {
    fn info(&self) -> protocol::BoxFuture<'_, protocol::Result<SessionInfo>> {
        Box::pin(async {
            Ok(SessionInfo {
                id: Some(SessionId::new("durable-id")),
                ..Default::default()
            })
        })
    }

    fn replay(&self, context: SessionContext) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async move {
            *self.0.context.lock().unwrap() = Some(context.clone());
            if self.0.fail_shutdown.load(Ordering::Acquire) {
                return Err(protocol::Error::internal_error().data("fixture replay failure"));
            }
            if self.0.wait_metadata.load(Ordering::Acquire) {
                self.0.metadata_started.notify_one();
                self.0.cancel_release.notified().await;
            }
            for index in 0..128 {
                context
                    .client()
                    .update_wait(message(index.to_string()))
                    .await?;
            }
            Ok(())
        })
    }

    fn prompt(
        &self,
        _request: PromptRequest,
        _context: SessionContext,
    ) -> protocol::BoxFuture<'_, protocol::Result<PromptResponse>> {
        Box::pin(async { Ok(PromptResponse::new(StopReason::EndTurn)) })
    }

    fn cancel(&self) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn shutdown(&self) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async {
            self.0.shutdowns.fetch_add(1, Ordering::AcqRel);
            if self.0.fail_shutdown.load(Ordering::Acquire) {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn failed_replay_with_unconfirmed_cleanup_has_a_bounded_connection_failure() {
    let observations = Arc::new(Observations::default());
    observations.fail_shutdown.store(true, Ordering::Release);
    let server = AgentServer::builder(SavedBackend(observations))
        .limits(ConnectionLimits {
            shutdown_timeout: Duration::from_millis(20),
            ..Default::default()
        })
        .build()
        .unwrap();
    let (a, b) = tokio::io::duplex(8192);
    let (reader, writer) = tokio::io::split(b);
    let task = tokio::spawn(async move { server.serve(reader, writer).await });
    let (reader, writer) = tokio::io::split(a);
    let (connection, _updates) =
        AcpConnection::connect(reader, writer, ConnectionOptions::default())
            .await
            .unwrap();
    assert!(
        connection
            .client()
            .request(LoadSessionRequest::new("durable-id", "/workspace"))
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let _ = connection.disconnect().await;
}

#[tokio::test]
async fn settings_and_prompts_cannot_enter_a_partially_replayed_session() {
    let observations = Arc::new(Observations::default());
    observations.wait_metadata.store(true, Ordering::Release);
    let server = AgentServer::builder(SavedBackend(observations.clone()))
        .build()
        .unwrap();
    let (a, b) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(b);
    let task = tokio::spawn(async move { server.serve(reader, writer).await });
    let (reader, writer) = tokio::io::split(a);
    let (connection, mut updates) =
        AcpConnection::connect(reader, writer, ConnectionOptions::default())
            .await
            .unwrap();
    let loading = {
        let client = connection.client();
        tokio::spawn(async move {
            client
                .request(LoadSessionRequest::new("durable-id", "/workspace"))
                .await
        })
    };
    observations.metadata_started.notified().await;
    let client = connection.client();
    assert!(
        client
            .request(prompt(&SessionId::new("durable-id"), "too early"))
            .await
            .is_err()
    );
    assert!(
        client
            .request(protocol::schema::v1::SetSessionModeRequest::new(
                "durable-id",
                "plan"
            ))
            .await
            .is_err()
    );
    observations.cancel_release.notify_one();
    for _ in 0..128 {
        updates.recv().await.unwrap();
    }
    loading.await.unwrap().unwrap();
    assert_eq!(
        client
            .request(prompt(&SessionId::new("durable-id"), "ready"))
            .await
            .unwrap()
            .stop_reason,
        StopReason::EndTurn
    );
    connection.disconnect().await.unwrap();
    assert!(task.await.unwrap().is_ok());
}

#[tokio::test]
async fn replay_backpressures_a_tiny_queue_and_finishes_before_load_response() {
    let observations = Arc::new(Observations::default());
    let server = AgentServer::builder(SavedBackend(observations.clone()))
        .limits(ConnectionLimits {
            update_capacity: 1,
            ..Default::default()
        })
        .build()
        .unwrap();
    let (a, b) = tokio::io::duplex(1024);
    let (reader, writer) = tokio::io::split(b);
    let task = tokio::spawn(async move { server.serve(reader, writer).await });
    let (reader, writer) = tokio::io::split(a);
    let (connection, mut updates) =
        AcpConnection::connect(reader, writer, ConnectionOptions::default())
            .await
            .unwrap();
    let pending = {
        let client = connection.client();
        tokio::spawn(async move {
            client
                .request(LoadSessionRequest::new("durable-id", "/workspace"))
                .await
        })
    };
    for index in 0..128 {
        let update = serde_json::to_value(updates.recv().await.unwrap()).unwrap();
        assert_eq!(update["sessionId"], "durable-id");
        assert_eq!(update["update"]["content"]["text"], index.to_string());
    }
    pending.await.unwrap().unwrap();
    let stale = observations.context.lock().unwrap().clone().unwrap();
    assert!(stale.client().update(message("late")).is_err());
    assert!(
        connection
            .client()
            .request(LoadSessionRequest::new("durable-id", "/workspace"))
            .await
            .is_err()
    );
    connection.disconnect().await.unwrap();
    assert!(task.await.unwrap().is_ok());
    assert_eq!(observations.shutdowns.load(Ordering::Acquire), 1);
}
