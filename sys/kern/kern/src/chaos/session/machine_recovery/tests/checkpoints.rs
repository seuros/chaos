use super::*;
use chaos_proc::{ProcessMetadataBuilder, RecoveryCheckpointNote, RuntimeDbHandle, StateRuntime};
use serde_json::{Value, json};

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolPayload};
use crate::tools::handlers::{DynamicToolHandler, McpResourceHandler, recovery_checkpoint};
use crate::tools::registry::{ToolHandler, ToolRegistryBuilder};
use crate::turn_diff_tracker::TurnDiffTracker;

async fn fixture() -> anyhow::Result<(
    tempfile::TempDir,
    Arc<Session>,
    Arc<TurnContext>,
    Arc<StateRuntime>,
)> {
    let home = tempfile::tempdir()?;
    let (mut session, mut turn) =
        crate::chaos::make_session_and_context_with_home(home.path()).await;
    // Recovery notes must also work in otherwise read-only modes.
    turn.mode_capabilities.mutation = false;
    let runtime = StateRuntime::init(home.path().join("database"), "test".into()).await?;
    let builder = ProcessMetadataBuilder::new(
        session.conversation_id,
        jiff::Timestamp::now(),
        chaos_ipc::protocol::SessionSource::Cli,
    );
    runtime.upsert_process(&builder.build("test")).await?;
    session.services.runtime_db = Some(RuntimeDbHandle::Sqlite(runtime.clone()));
    Ok((home, Arc::new(session), Arc::new(turn), runtime))
}

fn note() -> RecoveryCheckpointNote {
    serde_json::from_value(json!({
        "summary": "\"UNTRUSTED_CHECKPOINT_SENTINEL\"",
        "next_steps": ["Run the scoped test"],
        "artifacts": ["src/lib.rs"],
        "running_tasks": ["exec:42 stopped"]
    }))
    .unwrap()
}

fn invocation(
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    name: &str,
    arguments: Value,
) -> ToolInvocation {
    ToolInvocation {
        session: session.clone(),
        turn: turn.clone(),
        tracker: Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        call_id: "checkpoint-test".into(),
        tool_name: name.into(),
        tool_namespace: None,
        payload: ToolPayload::Function {
            arguments: arguments.to_string(),
        },
    }
}

async fn resource(
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    uri: &str,
) -> anyhow::Result<Value> {
    let output = McpResourceHandler
        .handle(invocation(
            session,
            turn,
            "read_mcp_resource",
            json!({"uri": uri}),
        ))
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let payload: Value = serde_json::from_str(&output.into_text())?;
    Ok(serde_json::from_str(
        payload["contents"][0]["text"].as_str().unwrap(),
    )?)
}

#[tokio::test]
async fn recovery_checkpoint_tool_commits_and_resource_stays_readable_after_recovery()
-> anyhow::Result<()> {
    let (home, session, turn, _runtime) = fixture().await?;
    unresolved(&session).await;
    let mut builder = ToolRegistryBuilder::new();
    builder.register_handler(
        recovery_checkpoint::NAME,
        Arc::new(recovery_checkpoint::Handler),
    );
    let (_, registry) = builder.build();
    let call = invocation(
        &session,
        &turn,
        recovery_checkpoint::NAME,
        serde_json::to_value(note())?,
    );
    let output = registry.dispatch_any(call.clone()).await?;
    let ack: Value = serde_json::from_str(&output.result.log_preview())?;
    assert_eq!(ack["status"], "saved");
    let uri = ack["resource_uri"].as_str().unwrap();
    let saved = session
        .runtime_db()
        .unwrap()
        .recovery_checkpoint(session.conversation_id, None)
        .await?
        .unwrap();
    assert_eq!(
        ack["checkpoint_id"], saved.id,
        "ack refers to a committed row"
    );
    assert_eq!(ack["recovery_id"], saved.recovery_id);
    assert_eq!(uri, "chaos://recovery/1");
    assert_eq!(
        saved.note,
        note(),
        "note strings are not sanitized or rewritten"
    );
    assert_eq!(
        registry.dispatch_any(call).await?.result.log_preview(),
        ack.to_string()
    );
    let detail = resource(&session, &turn, uri).await?;
    assert_eq!(detail["checkpoint"]["note"]["summary"], note().summary);
    assert_eq!(detail["checkpoint"]["observation"]["phase"], "warning");
    assert!(
        !session
            .machine_recovery_instruction()
            .await
            .unwrap()
            .contains("UNTRUSTED_CHECKPOINT_SENTINEL")
    );
    assert!(
        !home.path().join("checkpoints").exists(),
        "no checkpoint directory is created"
    );

    tokio::time::pause();
    sample(&session, chaos_machine::ThermalState::Normal).await;
    for _ in 0..10 {
        tokio::time::advance(POLL_INTERVAL).await;
        sample(&session, chaos_machine::ThermalState::Normal).await;
    }
    tokio::time::resume();
    assert_eq!(
        session.machine_recovery_status().await.phase,
        Phase::Recovered
    );
    assert!(
        session
            .save_recovery_checkpoint(&turn, "late", note())
            .await
            .is_err()
    );
    let root = resource(&session, &turn, "chaos://recovery").await?;
    assert_eq!(root["current_recovery"]["phase"], "recovered");
    assert_eq!(root["current_recovery"]["recovery_id"], saved.recovery_id);
    assert_eq!(root["checkpoint_uris"], json!([uri]));
    assert_eq!(
        root["checkpoint"]["observation"]["phase"], "warning",
        "saved evidence stays historical"
    );
    Ok(())
}

#[tokio::test]
async fn recovery_checkpoint_ids_share_an_episode_but_not_a_replaced_context() -> anyhow::Result<()>
{
    let (home, session, turn, _runtime) = fixture().await?;
    unresolved(&session).await;
    assert_eq!(session.machine_recovery_status().await.recovery_id, None);
    let serial = session
        .state
        .lock()
        .await
        .machine_recovery
        .checkpoint_writer
        .clone();
    let (a, b) = tokio::join!(
        session.save_recovery_checkpoint(&turn, "parallel-a", note()),
        session.save_recovery_checkpoint(&turn, "parallel-b", note())
    );
    let (a, b) = (a?, b?);
    assert_ne!(a.id, b.id);
    assert_eq!((a.recovery_id, b.recovery_id), (1, 1));
    let before = session.machine_recovery_status().await;
    assert_eq!(before.recovery_id, Some(1));

    session.state.lock().await.session_configuration.cwd = home.path().join("other-context");
    unresolved(&session).await;
    let after = session.machine_recovery_status().await;
    assert!(
        after.generation > before.generation,
        "context resets cannot alias a live generation"
    );
    assert_eq!(after.recovery_id, None);
    assert!(Arc::ptr_eq(
        &serial,
        &session
            .state
            .lock()
            .await
            .machine_recovery
            .checkpoint_writer
    ));
    let retry = session
        .save_recovery_checkpoint(&turn, "parallel-a", note())
        .await?;
    assert_eq!(retry, a);
    assert_eq!(
        session.machine_recovery_status().await.recovery_id,
        None,
        "a retained retry cannot bind an old recovery ID to a new episode"
    );
    let next = session
        .save_recovery_checkpoint(&turn, "next-context", note())
        .await?;
    assert_eq!((next.id, next.recovery_id), (3, 2));
    assert_eq!(session.machine_recovery_status().await.recovery_id, Some(2));
    Ok(())
}

#[tokio::test]
async fn recovery_checkpoint_resume_does_not_restore_health_or_authority() -> anyhow::Result<()> {
    let (home, session, turn, runtime) = fixture().await?;
    unresolved(&session).await;
    let first = session
        .save_recovery_checkpoint(&turn, "before-restart", note())
        .await?;
    let (mut resumed, resumed_turn) =
        crate::chaos::make_session_and_context_with_home(home.path()).await;
    resumed.conversation_id = session.conversation_id;
    resumed.services.runtime_db = Some(RuntimeDbHandle::Sqlite(runtime.clone()));
    let resumed = Arc::new(resumed);
    let resumed_turn = Arc::new(resumed_turn);
    let root = resource(&resumed, &resumed_turn, "chaos://recovery").await?;
    assert!(
        root["current_recovery"].is_null(),
        "a resumed session has no inherited observation"
    );
    assert_eq!(root["checkpoint"]["id"], first.id);
    assert!(resumed.machine_recovery_instruction().await.is_none());
    assert!(
        resumed
            .save_recovery_checkpoint(&resumed_turn, "no-warning", note())
            .await
            .is_err()
    );
    unresolved(&resumed).await;
    let next = resumed
        .save_recovery_checkpoint(&resumed_turn, "new-episode", note())
        .await?;
    assert_eq!(
        first.observation["generation"],
        next.observation["generation"]
    );
    assert_eq!(next.id, first.id + 1);
    assert_eq!(
        next.recovery_id,
        first.recovery_id + 1,
        "durable identity is not the resettable generation"
    );

    let (mut foreign, foreign_turn) =
        crate::chaos::make_session_and_context_with_home(home.path()).await;
    foreign.services.runtime_db = Some(RuntimeDbHandle::Sqlite(runtime));
    let foreign = Arc::new(foreign);
    let err = foreign
        .recovery_resource_json(Some(first.id))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        foreign
            .recovery_resource_json(Some(i64::MAX))
            .await
            .unwrap_err()
    );
    let foreign_root = resource(&foreign, &Arc::new(foreign_turn), "chaos://recovery").await?;
    assert!(foreign_root["checkpoint"].is_null());
    assert_eq!(foreign_root["checkpoint_uris"], json!([]));
    Ok(())
}

#[tokio::test]
async fn recovery_checkpoint_rejects_stale_disabled_parked_and_unavailable_writes()
-> anyhow::Result<()> {
    let (_home, session, turn, runtime) = fixture().await?;
    assert!(
        session
            .save_recovery_checkpoint(&turn, "healthy", note())
            .await
            .is_err()
    );
    unresolved(&session).await;
    {
        let mut state = session.state.lock().await;
        Arc::make_mut(&mut state.session_configuration.original_config_do_not_use)
            .machine_warnings
            .enabled = false;
    }
    assert!(
        session
            .save_recovery_checkpoint(&turn, "disabled", note())
            .await
            .is_err()
    );
    {
        let mut state = session.state.lock().await;
        Arc::make_mut(&mut state.session_configuration.original_config_do_not_use)
            .machine_warnings
            .enabled = true;
    }
    let mut forged = serde_json::to_value(note())?;
    forged["process_id"] = json!(chaos_ipc::ProcessId::new());
    assert!(
        recovery_checkpoint::Handler
            .handle(invocation(
                &session,
                &turn,
                recovery_checkpoint::NAME,
                forged
            ))
            .await
            .is_err()
    );
    session.cancel_machine_recovery_wait().await;
    assert!(
        session
            .save_recovery_checkpoint(&turn, "stale", note())
            .await
            .is_err()
    );
    session.state.lock().await.machine_recovery.begin_sample();
    session
        .state
        .lock()
        .await
        .machine_recovery
        .request_wait(&turn.sub_id)
        .unwrap();
    assert!(
        session
            .save_recovery_checkpoint(&turn, "parked", note())
            .await
            .is_err()
    );
    session.cancel_machine_recovery_wait().await;
    session.state.lock().await.machine_recovery.begin_sample();
    let mut oversized = note();
    oversized.summary = "x".repeat(chaos_proc::MAX_RECOVERY_CHECKPOINT_BYTES);
    assert!(
        session
            .save_recovery_checkpoint(&turn, "oversized", oversized)
            .await
            .is_err()
    );
    assert!(
        session
            .runtime_db()
            .unwrap()
            .recovery_checkpoint_ids(session.conversation_id)
            .await?
            .is_empty()
    );
    runtime.pool().close().await;
    let err = session
        .save_recovery_checkpoint(&turn, "storage-down", note())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("persistence was not confirmed"));
    assert!(
        session.recovery_resource_json(None).await.is_err(),
        "a storage failure is not an empty history"
    );

    let (session, turn) = crate::chaos::make_session_and_context().await;
    unresolved(&session).await;
    assert!(
        session
            .save_recovery_checkpoint(&turn, "missing-db", note())
            .await
            .unwrap_err()
            .to_string()
            .contains("storage is unavailable")
    );
    Ok(())
}

#[tokio::test]
async fn recovery_checkpoint_names_do_not_grant_read_only_mode_exceptions() -> anyhow::Result<()> {
    let (_home, session, turn, _runtime) = fixture().await?;
    unresolved(&session).await;
    let mut builder = ToolRegistryBuilder::new();
    builder.register_handler(recovery_checkpoint::NAME, Arc::new(DynamicToolHandler));
    let (_, registry) = builder.build();
    let err = registry
        .dispatch_any(invocation(
            &session,
            &turn,
            recovery_checkpoint::NAME,
            json!({}),
        ))
        .await
        .unwrap_err();
    std::assert_matches!(
        err,
        FunctionCallError::RespondToModel(message) if message.contains("mutation is disabled")
    );
    Ok(())
}
