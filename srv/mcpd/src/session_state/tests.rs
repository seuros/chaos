use super::*;
use anyhow::{Context, Result};
use chaos_kern::config::{ConfigBuilder, ConfigOverrides};
use mcp_host::protocol::types::JsonRpcRequest;
use serde_json::json;

async fn initialize(server: &Server, id: &str) -> Result<Session> {
    let response = server
        .handle_request(
            id,
            JsonRpcRequest::new(
                json!(1),
                "initialize",
                Some(json!({
                    "protocolVersion": "2025-11-25",
                    "clientInfo": {"name": "isolation-test", "version": "1"},
                    "capabilities": {}
                })),
            ),
        )
        .await
        .context("initialize response")?;
    assert!(response.error.is_none(), "{response:?}");
    server
        .handle_request(
            id,
            JsonRpcRequest::notification("notifications/initialized", None),
        )
        .await;
    server.get_session(id).context("SDK session")
}

async fn config(home: &tempfile::TempDir) -> Result<(Config, Arc<AuthManager>)> {
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "storage_url = \"sqlite://{}\"\n",
            home.path().join("chaos.sqlite").display()
        ),
    )?;
    let mut config = ConfigBuilder::default()
        .chaos_home(home.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(home.path().to_path_buf()),
            ..Default::default()
        })
        .build()
        .await?;
    // These tests never start a model or discover provider catalogs.
    config.clamp = true;
    let auth = AuthManager::shared(
        config.chaos_home.clone(),
        false,
        config.cli_auth_credentials_store_mode,
    );
    Ok((config, auth))
}

#[tokio::test]
async fn state_is_shared_only_with_sdk_session_clones() -> Result<()> {
    let home = tempfile::tempdir()?;
    let (config, auth) = config(&home).await?;
    let server = Server::builder("isolation-test", "1").build();
    let first = initialize(&server, "first").await?;
    let second = initialize(&server, "second").await?;
    let states = SessionStates::default();
    let a = states.get(&first, &config, &auth)?;
    let same = states.get(&first.clone(), &config, &auth)?;
    let b = states.get(&second, &config, &auth)?;
    assert!(Arc::ptr_eq(&a, &same));
    assert!(!Arc::ptr_eq(&a, &b));
    assert!(!Arc::ptr_eq(&a.process_table, &b.process_table));
    assert!(!Arc::ptr_eq(&a.process_table, &a.workflow_process_table));
    assert!(!Arc::ptr_eq(&a.url_elicitations, &b.url_elicitations));

    // Identical JSON-RPC IDs are valid in two MCP sessions.
    let id = RequestId::number(7);
    let first_pid = ProcessId::new();
    let second_pid = ProcessId::new();
    a.running_requests
        .lock()
        .unwrap()
        .insert(id.clone(), first_pid);
    b.running_requests
        .lock()
        .unwrap()
        .insert(id.clone(), second_pid);
    assert_eq!(
        a.running_requests.lock().unwrap().get(&id),
        Some(&first_pid)
    );
    assert_eq!(
        b.running_requests.lock().unwrap().get(&id),
        Some(&second_pid)
    );
    a.session_processes
        .lock()
        .await
        .insert("first".into(), first_pid);
    assert!(b.session_processes.lock().await.is_empty());
    assert!(!b.owns_process(first_pid).await);
    assert!(
        states.expire(&server).is_empty(),
        "no notification/SSE sender is required"
    );

    server.remove_session("first");
    let expired = states.expire(&server);
    assert_eq!(expired.len(), 1);
    assert!(a.is_closed());
    assert!(a.workflow_admission.is_closed());
    assert!(!b.is_closed());
    assert!(states.get(&second, &config, &auth).is_ok());
    let mut pending = tokio::task::JoinSet::new();
    cleanup_sessions(&mut pending, expired);
    cleanup_sessions(&mut pending, states.drain());
    while pending.join_next().await.is_some() {}
    assert!(states.get(&second, &config, &auth).is_err());
    Ok(())
}

#[tokio::test]
async fn reused_textual_id_never_inherits_previous_session_state() -> Result<()> {
    let home = tempfile::tempdir()?;
    let (config, auth) = config(&home).await?;
    let server = Server::builder("isolation-test", "1").build();
    let states = SessionStates::default();
    let original = initialize(&server, "reused").await?;
    let old = states.get(&original, &config, &auth)?;
    server.remove_session("reused");
    let replacement = initialize(&server, "reused").await?;
    assert!(!same_session(&original, &replacement));
    let new = states.get(&replacement, &config, &auth)?;
    assert!(!Arc::ptr_eq(&old, &new));
    assert!(old.is_closed());
    assert!(!new.is_closed());
    let mut pending = tokio::task::JoinSet::new();
    cleanup_sessions(&mut pending, states.drain());
    while pending.join_next().await.is_some() {}
    Ok(())
}

#[tokio::test]
async fn closure_signal_is_not_lost_before_subscription() -> Result<()> {
    let home = tempfile::tempdir()?;
    let (config, auth) = config(&home).await?;
    let server = Server::builder("isolation-test", "1").build();
    let session = initialize(&server, "owner").await?;
    let states = SessionStates::default();
    let state = states.get(&session, &config, &auth)?;
    assert_eq!(state.workflow_admission.available_permits(), 4);
    states.close();
    tokio::time::timeout(Duration::from_millis(50), state.cancelled()).await?;
    assert!(
        state
            .workflow_admission
            .clone()
            .try_acquire_owned()
            .is_err()
    );
    assert!(states.get(&session, &config, &auth).is_err());
    let mut pending = tokio::task::JoinSet::new();
    cleanup_sessions(&mut pending, states.drain());
    while pending.join_next().await.is_some() {}
    Ok(())
}
