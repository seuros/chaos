use super::*;
use anyhow::{Context, Result};
use mcp_host::protocol::types::JsonRpcRequest;
use serde_json::json;

async fn server_with_session() -> Result<Arc<Server>> {
    let server = Arc::new(Server::builder("factory-lifecycle-test", "1").build());
    let response = server
        .handle_request(
            "owner",
            JsonRpcRequest::new(
                json!(1),
                "initialize",
                Some(json!({
                    "protocolVersion": "2025-11-25",
                    "clientInfo": {"name": "lifecycle-test", "version": "1"},
                    "capabilities": {}
                })),
            ),
        )
        .await
        .context("initialize response")?;
    assert!(response.error.is_none(), "{response:?}");
    server
        .handle_request(
            "owner",
            JsonRpcRequest::notification("notifications/initialized", None),
        )
        .await;
    Ok(server)
}

#[tokio::test]
async fn explicit_shutdown_revokes_sdk_sessions_and_is_idempotent() -> Result<()> {
    let server = server_with_session().await?;
    let (stop, stopped) = oneshot::channel();
    let cleanup = tokio::spawn(async move {
        let _ = stopped.await;
        Ok(())
    });
    let mut handle = McpServerHandle {
        server: server.clone(),
        states: Arc::default(),
        stop: Some(stop),
        cleanup: Some(cleanup),
    };
    assert_eq!(server.active_sessions().len(), 1);
    handle.shutdown().await?;
    assert!(server.active_sessions().is_empty());
    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn drop_revokes_sdk_sessions_and_signals_cleanup_without_retaining_server() -> Result<()> {
    let server = server_with_session().await?;
    let weak = Arc::downgrade(&server);
    let (stop, stopped) = oneshot::channel();
    let (finished, done) = oneshot::channel();
    let cleanup = tokio::spawn(async move {
        let _ = stopped.await;
        let _ = finished.send(());
        Ok(())
    });
    let handle = McpServerHandle {
        server: server.clone(),
        states: Arc::default(),
        stop: Some(stop),
        cleanup: Some(cleanup),
    };
    drop(handle);
    assert!(server.active_sessions().is_empty());
    tokio::time::timeout(Duration::from_secs(1), done).await??;
    drop(server);
    assert!(weak.upgrade().is_none());
    Ok(())
}
