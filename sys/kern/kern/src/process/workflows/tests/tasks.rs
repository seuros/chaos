//! Exercise the real SDK task worker, not a reconstructed execution context.

use super::{active_session, attach};
use crate::chaos::Session;
use crate::process::workflows::WorkflowScope;
use chaos_synopsis::runs::{MAX_ACTIVE_RUNS, RunError, RunKind};
use mcp_host::prelude::ExecutionContext;
use mcp_host::protocol::types::{JsonRpcRequest, JsonRpcResponse, TaskSupport, ToolExecution};
use mcp_host::registry::tools::{Tool, ToolFuture};
use mcp_host::server::Server;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const OWNER: &str = "workflow-owner";
const FOREIGN: &str = "workflow-foreign";

struct ScopedWorkflow(WorkflowScope);

impl Tool for ScopedWorkflow {
    fn name(&self) -> &str {
        "scoped_workflow"
    }

    fn input_schema(&self) -> Value {
        json!({"type":"object"})
    }

    fn execution(&self) -> Option<ToolExecution> {
        Some(ToolExecution {
            task_support: Some(TaskSupport::Optional),
        })
    }

    fn execute<'a>(&'a self, ctx: ExecutionContext<'a>) -> ToolFuture<'a> {
        // In particular, do not replace the SDK's task-related requester with
        // server.create_client_requester(), or manufacture a new context.
        Box::pin(self.0.run(ctx))
    }
}

struct Harness {
    native: Arc<Session>,
    native_cancellation: CancellationToken,
    scope: WorkflowScope,
    server: Server,
}

impl Harness {
    async fn new(version: &str) -> anyhow::Result<Self> {
        let (native, native_cancellation, _) = active_session().await;
        native.services.tool_group_catalog.set_groups_enabled(
            &native.services.tool_group_state,
            [crate::tools::groups::WORKFLOWS],
            true,
        )?;
        // A regression must never fall back to contacting a native model.
        native.services.model_client.set_clamped(true, None).await;
        let scope = attach(&native, OWNER).await?;
        let server = Server::builder("workflow-task-tests", "1")
            .with_tools(false)
            .with_tasks(true, true)
            .build();
        server
            .tool_registry()
            .register(ScopedWorkflow(scope.clone()))?;
        initialize(&server, OWNER, version).await?;
        Ok(Self {
            native,
            native_cancellation,
            scope,
            server,
        })
    }

    async fn finish(self) {
        self.scope.close();
        self.native.active_turn.lock().await.take();
    }
}

async fn request(
    server: &Server,
    session: &str,
    method: &str,
    params: Value,
) -> anyhow::Result<JsonRpcResponse> {
    tokio::time::timeout(
        Duration::from_secs(5),
        server.handle_request(session, JsonRpcRequest::new(json!(1), method, Some(params))),
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("missing {method} response"))
}

fn success(response: JsonRpcResponse) -> anyhow::Result<Value> {
    anyhow::ensure!(response.error.is_none(), "{:?}", response.error);
    response
        .result
        .ok_or_else(|| anyhow::anyhow!("missing JSON-RPC result"))
}

async fn initialize(server: &Server, session: &str, version: &str) -> anyhow::Result<Value> {
    let initialized = success(
        request(
            server,
            session,
            "initialize",
            json!({
                "protocolVersion":version,
                "capabilities":{"sampling":{}},
                "clientInfo":{"name":"in-process-task-client","version":"1"}
            }),
        )
        .await?,
    )?;
    assert!(
        server
            .handle_request(
                session,
                JsonRpcRequest::notification("notifications/initialized", None),
            )
            .await
            .is_none()
    );
    assert!(server.get_session(session).expect("SDK session").is_ready());
    Ok(initialized)
}

fn condition() -> Value {
    json!({"tree":{
        "type":"condition","id":"present","pointer":"/input","predicate":{"op":"exists"}
    }})
}

async fn start_task(server: &Server, arguments: Value) -> anyhow::Result<String> {
    let result = success(
        request(
            server,
            OWNER,
            "tools/call",
            json!({"name":"scoped_workflow","arguments":arguments,"task":{"ttl":60000}}),
        )
        .await?,
    )?;
    assert!(result.get("structuredContent").is_none(), "{result}");
    Ok(result["task"]["taskId"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing SDK task: {result}"))?
        .to_owned())
}

async fn task_status(server: &Server, task: &str) -> anyhow::Result<Value> {
    success(request(server, OWNER, "tasks/get", json!({"taskId":task})).await?)
}

async fn pending_sample(server: &Server) -> anyhow::Result<String> {
    let task = start_task(
        server,
        json!({"tree":{"type":"sample","id":"sample","prompt":"Wait for this client only"}}),
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = task_status(server, &task).await?;
            if status["status"] == "input_required" && server.multiplexer().pending_count() == 1 {
                break;
            }
            anyhow::ensure!(
                status["status"] == "working" || status["status"] == "input_required",
                "Sample did not wait for the external client: {status}"
            );
            tokio::task::yield_now().await;
        }
        anyhow::Ok(())
    })
    .await??;
    // input_required is maintained by the *related-task* requester. Merely
    // recreating a requester for the same session would leave the task working.
    Ok(task)
}

async fn wait_for_request_cleanup(server: &Server) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.multiplexer().pending_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn augmented_call_returns_sdk_task_and_owned_workflow_snapshot() -> anyhow::Result<()> {
    let harness = Harness::new("2025-11-25").await?;
    let listed = success(request(&harness.server, OWNER, "tools/list", json!({})).await?)?;
    assert_eq!(listed["tools"][0]["execution"]["taskSupport"], "optional");
    let task = start_task(&harness.server, condition()).await?;
    let result = success(
        request(
            &harness.server,
            OWNER,
            "tasks/result",
            json!({"taskId":task}),
        )
        .await?,
    )?;
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/related-task"]["taskId"],
        task
    );
    let snapshot = &result["structuredContent"];
    assert_eq!(snapshot["status"]["state"], "terminal", "{result}");
    assert_eq!(snapshot["status"]["outcome"], "success", "{result}");
    assert_eq!(
        task_status(&harness.server, &task).await?["status"],
        "completed"
    );
    let owner = harness.server.get_session(OWNER).expect("SDK session");
    let logger = harness.server.logger_for_session(OWNER);
    let status = harness.scope.status(ExecutionContext::new(
        json!({"run_id":snapshot["run_id"]}),
        &owner,
        &logger,
    ))?;
    assert_eq!(status.into_response_value()["structuredContent"], *snapshot);
    assert_eq!(harness.server.multiplexer().pending_count(), 0);
    harness.finish().await;
    Ok(())
}

#[tokio::test]
async fn sdk_task_ownership_rejects_foreign_get_result_and_cancel() -> anyhow::Result<()> {
    let harness = Harness::new("2025-11-25").await?;
    initialize(&harness.server, FOREIGN, "2025-11-25").await?;
    let task = pending_sample(&harness.server).await?;
    for method in ["tasks/get", "tasks/result", "tasks/cancel"] {
        let denied = request(&harness.server, FOREIGN, method, json!({"taskId":task})).await?;
        let error = denied.error.expect("foreign task must not be visible");
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("Task not found"), "{error:?}");
    }
    let foreign_list = success(request(&harness.server, FOREIGN, "tasks/list", json!({})).await?)?;
    assert_eq!(foreign_list["tasks"], json!([]));
    assert_eq!(
        task_status(&harness.server, &task).await?["status"],
        "input_required"
    );
    assert_eq!(harness.server.multiplexer().pending_count(), 1);
    success(
        request(
            &harness.server,
            OWNER,
            "tasks/cancel",
            json!({"taskId":task}),
        )
        .await?,
    )?;
    wait_for_request_cleanup(&harness.server).await?;
    harness.finish().await;
    Ok(())
}

#[tokio::test]
async fn cancelling_sdk_task_drops_pending_sample_and_releases_run_budget() -> anyhow::Result<()> {
    let harness = Harness::new("2025-11-25").await?;
    let task = pending_sample(&harness.server).await?;
    let cancelled = success(
        request(
            &harness.server,
            OWNER,
            "tasks/cancel",
            json!({"taskId":task}),
        )
        .await?,
    )?;
    assert_eq!(cancelled["status"], "cancelled");
    wait_for_request_cleanup(&harness.server).await?;
    assert_eq!(
        task_status(&harness.server, &task).await?["status"],
        "cancelled"
    );
    assert!(!harness.native_cancellation.is_cancelled());

    // The aborted worker cannot return its run ID. Reusing *all* active slots
    // proves its dropped RunLease no longer leaves a running registry entry.
    let slots = (0..MAX_ACTIVE_RUNS)
        .map(|_| harness.scope.0.registry.start(RunKind::Workflow))
        .collect::<Result<Vec<_>, _>>()?;
    assert!(matches!(
        harness.scope.0.registry.start(RunKind::Workflow),
        Err(RunError::ActiveLimit)
    ));
    drop(slots);
    let fresh = start_task(&harness.server, condition()).await?;
    let result = success(
        request(
            &harness.server,
            OWNER,
            "tasks/result",
            json!({"taskId":fresh}),
        )
        .await?,
    )?;
    assert_eq!(result["structuredContent"]["status"]["outcome"], "success");
    harness.finish().await;
    Ok(())
}

#[tokio::test]
async fn closing_scope_cleans_pending_sample_before_completing_sdk_task() -> anyhow::Result<()> {
    let harness = Harness::new("2025-11-25").await?;
    let task = pending_sample(&harness.server).await?;
    harness.scope.close();
    let result = success(
        request(
            &harness.server,
            OWNER,
            "tasks/result",
            json!({"taskId":task}),
        )
        .await?,
    )?;
    let snapshot = &result["structuredContent"];
    assert_eq!(snapshot["status"]["state"], "terminal", "{result}");
    assert_eq!(snapshot["status"]["outcome"], "cancelled", "{result}");
    assert_eq!(snapshot["steps"][0]["status"]["state"], "terminal");
    assert_eq!(harness.server.multiplexer().pending_count(), 0);
    assert_eq!(
        task_status(&harness.server, &task).await?["status"],
        "completed"
    );
    assert!(!harness.native_cancellation.is_cancelled());
    harness.finish().await;
    Ok(())
}

#[tokio::test]
async fn older_client_retains_synchronous_workflow_without_task_capability() -> anyhow::Result<()> {
    let harness = Harness::new("2025-06-18").await?;
    let owner = harness.server.get_session(OWNER).expect("SDK session");
    assert!(
        owner
            .server_capabilities()
            .expect("negotiated")
            .tasks
            .is_none()
    );
    let listed = success(request(&harness.server, OWNER, "tools/list", json!({})).await?)?;
    assert_eq!(listed["tools"][0]["name"], "scoped_workflow");
    assert!(listed["tools"][0].get("execution").is_none());
    let result = success(
        request(
            &harness.server,
            OWNER,
            "tools/call",
            json!({"name":"scoped_workflow","arguments":condition()}),
        )
        .await?,
    )?;
    assert!(result.get("task").is_none());
    assert_eq!(result["structuredContent"]["status"]["outcome"], "success");
    // SDK 0.13.1 conservatively keeps the synchronous path when tasks were
    // not negotiated: a legacy task parameter does not enable augmentation.
    // Record that compatibility behavior, not rejection of the parameter.
    let legacy = success(
        request(
            &harness.server,
            OWNER,
            "tools/call",
            json!({
                "name":"scoped_workflow",
                "arguments":condition(),
                "task":{"ttl":60000}
            }),
        )
        .await?,
    )?;
    assert!(legacy.get("task").is_none(), "{legacy}");
    assert!(
        legacy["structuredContent"]["run_id"].is_string(),
        "{legacy}"
    );
    assert_eq!(
        legacy["structuredContent"]["status"]["state"], "terminal",
        "{legacy}"
    );
    assert_eq!(
        legacy["structuredContent"]["status"]["outcome"], "success",
        "{legacy}"
    );
    for method in ["tasks/list", "tasks/get", "tasks/result", "tasks/cancel"] {
        let denied = request(
            &harness.server,
            OWNER,
            method,
            json!({"taskId":"unavailable"}),
        )
        .await?;
        assert!(denied.error.is_some(), "{method} must not be negotiated");
    }
    assert_eq!(harness.server.multiplexer().pending_count(), 0);
    harness.finish().await;
    Ok(())
}
