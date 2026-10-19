use super::*;
use crate::tools::workflows::tests::{fixture, invocation};
use chaos_synopsis::runs::RunStatus;
use mcp_host::prelude::ClientRequester;
use mcp_host::protocol::types::JsonRpcResponse;
use mcp_host::protocol::version::ProtocolVersion;
use mcp_host::server::multiplexer::RequestMultiplexer;
use mcp_host::server::session::Session as McpSession;
use serde_json::Value;
use tokio::sync::mpsc;

fn external_session(
    version: ProtocolVersion,
    capabilities: Value,
) -> anyhow::Result<Arc<McpSession>> {
    let session = Arc::new(McpSession::with_id("external"));
    assert!(session.initialize(
        serde_json::from_value(json!({"name":"test","version":"1"}))?,
        serde_json::from_value(capabilities)?,
        version,
        Default::default(),
    ));
    assert!(session.mark_ready());
    Ok(session)
}

async fn caller() -> anyhow::Result<ToolInvocation> {
    let (session, turn, tracker) = fixture().await;
    session.services.tool_group_catalog.set_groups_enabled(
        &session.services.tool_group_state,
        [
            crate::tools::groups::WORKFLOWS,
            crate::tools::groups::FILESYSTEM,
        ],
        true,
    )?;
    Ok(invocation(
        session,
        turn,
        tracker,
        "run_workflow",
        json!({}),
    ))
}

fn args(value: Value) -> anyhow::Result<RunArgs> {
    Ok(serde_json::from_value(value)?)
}

#[tokio::test]
async fn external_call_uses_kernel_dispatch_and_separate_owner_registry() -> anyhow::Result<()> {
    let caller = caller().await?;
    let native_registry = caller.session.services.workflow_runs.clone();
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("input.txt");
    std::fs::write(&path, "external dispatch fixture\n")?;
    let session = external_session(ProtocolVersion::V2025_11_25, json!({}))?;
    let logger = Server::builder("external", "1")
        .build()
        .logger_for_session(session.id());
    let registry = RunRegistry::default();
    let snapshot = run(
        caller,
        args(json!({"input":{"path":path},"tree":{
            "type":"sequence","id":"root","children":[
                {"type":"call","id":"read","tool":"read_file","arguments":{"file_path":""},
                 "bindings":[{"source":"/input/path","target":"/file_path"}]},
                {"type":"condition","id":"exists","pointer":"/read","predicate":{"op":"exists"}}
            ]
        }}))?,
        &registry,
        Origin::external(
            ExecutionContext::new(json!({}), &session, &logger).with_request_meta(
                serde_json::from_value(json!({
                    "host_provenance":{"idempotent":true,"root_access":true}
                }))?,
            ),
        ),
        CancellationToken::new(),
    )
    .await?;
    assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Success));
    assert_eq!(
        snapshot.steps[0].detail.as_ref().unwrap()["tool"],
        "read_file"
    );
    assert!(serde_json::to_string(&snapshot)?.contains("external dispatch fixture"));
    assert!(native_registry.status(snapshot.run_id).is_err());
    assert!(RunRegistry::default().status(snapshot.run_id).is_err());
    assert!(registry.status(snapshot.run_id).is_ok());
    Ok(())
}

#[tokio::test]
async fn external_interactions_use_negotiated_requester_in_both_protocols() -> anyhow::Result<()> {
    for version in [ProtocolVersion::V2025_06_18, ProtocolVersion::V2025_11_25] {
        let caller = caller().await?;
        let session = external_session(version, json!({"sampling":{},"elicitation":{"form":{}}}))?;
        let logger = Server::builder("external", "1")
            .build()
            .logger_for_session(session.id());
        let registry = RunRegistry::default();
        let mux = Arc::new(RequestMultiplexer::new());
        let (tx, mut rx) = mpsc::channel(4);
        let requester = ClientRequester::new(tx, mux.clone(), session.clone());
        // This response pump is the external client, not a model or native picker.
        let client = async {
            let ask = rx
                .recv()
                .await
                .ok_or_else(|| anyhow::anyhow!("missing Ask"))?;
            assert_eq!(ask.method, "elicitation/create");
            let wire = ask.params.as_ref().unwrap();
            assert_eq!(wire["message"], "External question");
            if version == ProtocolVersion::V2025_06_18 {
                assert!(wire.get("mode").is_none());
            }
            assert!(
                mux.route_response(&serde_json::from_value::<JsonRpcResponse>(json!({
                    "jsonrpc":"2.0","id":ask.id,"result":{"action":"accept","content":{"ok":true}}
                }))?)
            );
            let sample = rx
                .recv()
                .await
                .ok_or_else(|| anyhow::anyhow!("missing Sample"))?;
            assert_eq!(sample.method, "sampling/createMessage");
            assert_eq!(sample.params.as_ref().unwrap()["maxTokens"], 17);
            assert!(
                mux.route_response(&serde_json::from_value::<JsonRpcResponse>(json!({
                    "jsonrpc":"2.0","id":sample.id,
                    "result":{
                        "role":"assistant","content":{"type":"text","text":"REMOTE_ONLY"},
                        "model":"external-model"
                    }
                }))?)
            );
            anyhow::Ok(())
        };
        let run = run(
            caller,
            args(json!({"tree":{"type":"sequence","id":"root","children":[
                {"type":"ask","id":"ask","message":"External question",
                 "schema":{
                     "type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"]
                 }},
                {"type":"sample","id":"sample","prompt":"External prompt","max_tokens":17},
                {"type":"condition","id":"result","pointer":"/sample",
                 "predicate":{"op":"equals","value":"REMOTE_ONLY"}}
            ]}}))?,
            &registry,
            Origin::external(
                ExecutionContext::new(json!({}), &session, &logger)
                    .with_client_requester(requester),
            ),
            CancellationToken::new(),
        );
        let (snapshot, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(async { run.await.map_err(anyhow::Error::from) }, client)
        })
        .await??;
        assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Success));
        assert_eq!(mux.pending_count(), 0);
    }
    Ok(())
}

#[tokio::test]
async fn absent_capabilities_fail_without_native_fallback_and_forged_meta_cannot_enable_them()
-> anyhow::Result<()> {
    for capabilities in [json!({}), json!({"sampling":{},"elicitation":{"form":{}}})] {
        let caller = caller().await?;
        let session = external_session(ProtocolVersion::V2025_11_25, capabilities)?;
        let logger = Server::builder("external", "1")
            .build()
            .logger_for_session(session.id());
        let registry = RunRegistry::default();
        // Even advertised capabilities do not invent a transport requester.
        let snapshot = run(
            caller,
            args(json!({"tree":{"type":"fallback","id":"root","children":[
                {"type":"ask","id":"ask","message":"No native picker"},
                {"type":"sample","id":"sample","prompt":"No native provider"},
                {
                    "type":"condition","id":"safe_fallback","pointer":"/input",
                    "predicate":{"op":"exists"}
                }
            ]}}))?,
            &registry,
            Origin::external(
                ExecutionContext::new(json!({}), &session, &logger).with_request_meta(
                    serde_json::from_value(json!({
                        "sampling":{},"elicitation":{"form":{}},"trusted":true
                    }))?,
                ),
            ),
            CancellationToken::new(),
        )
        .await?;
        assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Success));
        assert!(
            serde_json::to_string(&snapshot.steps[..2])?.contains("no negotiated client requester")
        );
    }
    Ok(())
}

#[tokio::test]
async fn closing_scope_cancels_pending_remote_request_and_records_cleanup() -> anyhow::Result<()> {
    let caller = caller().await?;
    let session = external_session(ProtocolVersion::V2025_11_25, json!({"sampling":{}}))?;
    let logger = Server::builder("external", "1")
        .build()
        .logger_for_session(session.id());
    let registry = RunRegistry::default();
    let mux = Arc::new(RequestMultiplexer::new());
    let (tx, mut rx) = mpsc::channel(1);
    let requester = ClientRequester::new(tx, mux.clone(), session.clone());
    let lifetime = CancellationToken::new();
    let run = run(
        caller,
        args(json!({"tree":{"type":"sample","id":"sample","prompt":"wait"}}))?,
        &registry,
        Origin::external(
            ExecutionContext::new(json!({}), &session, &logger).with_client_requester(requester),
        ),
        lifetime.clone(),
    );
    let close = async {
        assert_eq!(rx.recv().await.unwrap().method, "sampling/createMessage");
        assert_eq!(mux.pending_count(), 1);
        lifetime.cancel();
    };
    let (snapshot, ()) =
        tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(run, close) }).await?;
    let snapshot = snapshot?;
    assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Cancelled));
    assert_eq!(mux.pending_count(), 0);
    assert!(
        snapshot
            .steps
            .iter()
            .all(|step| matches!(step.status, chaos_synopsis::runs::StepStatus::Terminal(_)))
    );
    Ok(())
}

#[tokio::test]
async fn declared_transport_without_capability_sends_no_requests() -> anyhow::Result<()> {
    let caller = caller().await?;
    let session = external_session(ProtocolVersion::V2025_11_25, json!({}))?;
    let logger = Server::builder("external", "1")
        .build()
        .logger_for_session(session.id());
    let registry = RunRegistry::default();
    let mux = Arc::new(RequestMultiplexer::new());
    let (tx, mut rx) = mpsc::channel(1);
    let requester = ClientRequester::new(tx, mux.clone(), session.clone());
    let snapshot = run(
        caller,
        args(json!({"tree":{"type":"fallback","id":"root","children":[
            {"type":"ask","id":"ask","message":"unsupported"},
            {"type":"sample","id":"sample","prompt":"unsupported"},
            {"type":"condition","id":"fallback","pointer":"/input","predicate":{"op":"exists"}}
        ]}}))?,
        &registry,
        Origin::external(
            ExecutionContext::new(json!({}), &session, &logger)
                .with_client_requester(requester)
                .with_request_meta(serde_json::from_value(
                    json!({"sampling":{},"elicitation":{"form":{}}}),
                )?),
        ),
        CancellationToken::new(),
    )
    .await?;
    assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Success));
    assert!(rx.try_recv().is_err());
    assert_eq!(mux.pending_count(), 0);
    let failures = serde_json::to_string(&snapshot.steps[..2])?;
    assert!(
        failures.contains("does not support elicitation"),
        "{failures}"
    );
    assert!(failures.contains("does not support sampling"), "{failures}");
    Ok(())
}

#[tokio::test]
async fn removed_turn_authority_fails_closed_without_recreating_defaults() -> anyhow::Result<()> {
    let caller = caller().await?;
    caller
        .session
        .permission_actor
        .remove_turn(caller.turn.sub_id.clone())
        .await?;
    let session = external_session(ProtocolVersion::V2025_11_25, json!({}))?;
    let logger = Server::builder("external", "1")
        .build()
        .logger_for_session(session.id());
    let registry = RunRegistry::default();
    let result = run(
        caller,
        args(json!({"tree":{
            "type":"condition","id":"check","pointer":"/input","predicate":{"op":"exists"}
        }}))?,
        &registry,
        Origin::external(ExecutionContext::new(json!({}), &session, &logger)),
        CancellationToken::new(),
    )
    .await;
    assert!(result.is_err());
    Ok(())
}
