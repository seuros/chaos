//! One policy-aware executor for native and attached MCP calls.

use super::{RunArgs, dispatch, external, failure, interactions, observer};
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use chaos_synopsis::runs::{RunKind, RunOutcome, RunRegistry, RunSnapshot};
use chaos_synopsis::tree::{DEFAULT_WORKFLOW_TIMEOUT_MS, MAX_TIMEOUT_MS};
use mcp_host::bt::TreeTool;
use mcp_host::prelude::{ExecutionContext, Server, ToolBreakerConfig};
use mcp_host::protocol::types::JsonRpcRequest;
use mcp_host::registry::tools::Tool;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub(crate) enum Origin<'a> {
    Native,
    /// The actual outer request, including its negotiated requester/task route.
    /// Never reconstruct it from leaf arguments or caller-supplied `_meta`.
    External(Box<ExecutionContext<'a>>),
}

impl<'a> Origin<'a> {
    pub(crate) fn external(ctx: ExecutionContext<'a>) -> Self {
        Self::External(Box::new(ctx))
    }
}

pub(crate) async fn run(
    invocation: ToolInvocation,
    args: RunArgs,
    registry: &RunRegistry,
    origin: Origin<'_>,
    lifetime: CancellationToken,
) -> Result<RunSnapshot, FunctionCallError> {
    let timeout = args.timeout_ms.unwrap_or(DEFAULT_WORKFLOW_TIMEOUT_MS);
    if !args.input.is_object() || !(1..=MAX_TIMEOUT_MS).contains(&timeout) {
        return Err(failure(
            "input must be an object and timeout_ms must be in 1..=3600000",
        ));
    }
    if lifetime.is_cancelled() {
        return Err(failure("owning workflow scope has closed"));
    }
    let router = tokio::select! {
        biased;
        _ = lifetime.cancelled() => return Err(failure("owning workflow scope has closed")),
        router = async {
            let turn = invocation.session.try_effective_turn_context(&invocation.turn)
                .await.map_err(failure)?;
            crate::chaos::built_tools(&invocation.session, &turn, &[], &lifetime)
                .await.map_err(failure)
        } => router?,
    };
    if !router
        .visible_specs()
        .iter()
        .any(|spec| spec.name() == "run_workflow")
    {
        return Err(failure("workflows are not enabled for this caller"));
    }
    let prepared = dispatch::prepare(args.tree, &router)?;
    let lease = registry.start(RunKind::Workflow).map_err(failure)?;
    let surface = match &origin {
        Origin::Native => "native",
        Origin::External(_) => "external",
    };
    tracing::debug!(run_id = %lease.id(), surface, "workflow started");
    let cancellation = lease.cancellation();
    let tasks = tokio_util::task::TaskTracker::new();
    let observer = Arc::new(observer::Observer::new(
        lease.recorder(),
        cancellation.clone(),
        prepared.display_names(),
    ));
    let server = Server::builder("chaos-workflow-bridge", "1")
        .with_tools(false)
        // Private adapters only: the tree owns its timeout/retry budget.
        // Downstream tools retain the ordinary kernel/driver protections.
        .with_circuit_breaker(ToolBreakerConfig {
            call_timeout_secs: 0.0,
            failure_threshold: chaos_synopsis::tree::MAX_LEAF_ATTEMPTS + 1,
            ..Default::default()
        })
        .build();
    let caller = dispatch::Caller {
        invocation: Arc::new(invocation),
        cancellation: cancellation.clone(),
        run_id: lease.id(),
        tasks: tasks.clone(),
    };
    for leaf in prepared.leaves {
        server
            .tool_registry()
            .register(dispatch::LeafAdapter {
                leaf,
                observer: observer.clone(),
                caller: caller.clone(),
            })
            .map_err(failure)?;
    }
    let tree = TreeTool::new("execute", prepared.root)
        .map_err(failure)?
        .with_observer(observer.clone());
    let session_id = lease.id().to_string();
    let run =
        async {
            match origin {
                Origin::External(ctx) => {
                    let mut ctx = *ctx;
                    // Keep the real session, requester, progress, originating ID
                    // and task route. Only replace input and the dispatch registry.
                    // The interaction gate rechecks live kernel policy, then uses
                    // only the SDK's negotiated/task-routed external requester.
                    let tree = tree.with_interactions(Arc::new(external::ExternalInteractions {
                        caller,
                        requester: ctx.client_requester().cloned(),
                    }));
                    ctx.params = args.input;
                    Ok(
                        match tree
                            .execute(ctx.with_tool_registry(server.tool_registry()))
                            .await
                        {
                            Ok(output) => output.into_response_value(),
                            Err(error) => json!({"isError":true,"error":error.to_string()}),
                        },
                    )
                }
                Origin::Native => {
                    server
                        .tool_registry()
                        .register(tree.with_interactions(Arc::new(
                            interactions::NativeInteractions { caller },
                        )))
                        .map_err(failure)?;
                    let initialized = server
                        .handle_request(
                            &session_id,
                            JsonRpcRequest::new(
                                json!(1),
                                "initialize",
                                Some(json!({
                                    "protocolVersion":"2025-11-25", "capabilities":{},
                                    "clientInfo":{"name":"chaos-native","version":"1"}
                                })),
                            ),
                        )
                        .await
                        .ok_or_else(|| failure("native bridge initialization failed"))?;
                    if initialized.error.is_some() {
                        return Err(failure("native bridge initialization rejected"));
                    }
                    server
                        .handle_request(
                            &session_id,
                            JsonRpcRequest::notification("notifications/initialized", None),
                        )
                        .await;
                    let reply = server
                        .handle_request(
                            &session_id,
                            JsonRpcRequest::new(
                                json!(2),
                                "tools/call",
                                Some(json!({"name":"execute","arguments":args.input})),
                            ),
                        )
                        .await
                        .ok_or_else(|| failure("missing workflow result"))?;
                    if let Some(error) = reply.error {
                        return Err(failure(error.message));
                    }
                    reply
                        .result
                        .ok_or_else(|| failure("missing workflow payload"))
                }
            }
        };
    let mut run = Box::pin(run);
    let (mut outcome, mut result) = tokio::select! {
        biased;
        _ = lifetime.cancelled() => (RunOutcome::Cancelled, None),
        _ = cancellation.cancelled() => (RunOutcome::Cancelled, None),
        _ = tokio::time::sleep(Duration::from_millis(timeout)) => (RunOutcome::TimedOut, None),
        result = run.as_mut() => match result {
            Ok(mut value) => {
                let outcome = if value["isError"] == true {
                    RunOutcome::Failure
                } else {
                    RunOutcome::Success
                };
                let mut payload = value.as_object_mut()
                    .and_then(|object| object.remove("structuredContent")).unwrap_or(value);
                observer.display_result(&mut payload);
                if outcome == RunOutcome::Success && !payload["steps"].is_array() {
                    (RunOutcome::Error, Some(json!({"error":"invalid SDK workflow result"})))
                } else {
                    (outcome, Some(payload))
                }
            }
            Err(error) => (RunOutcome::Error, Some(json!({"error":error.to_string()}))),
        },
    };
    // SDK scope halt callbacks must run before completing the executor lease.
    drop(run);
    drop(server);
    tasks.close();
    if tokio::time::timeout(Duration::from_secs(5), tasks.wait())
        .await
        .is_err()
    {
        outcome = RunOutcome::Interrupted;
        result = Some(json!({"cleanup_incomplete":true}));
    }
    let snapshot = lease.finish(outcome, result).map_err(failure)?;
    tracing::debug!(run_id = %snapshot.run_id, surface, ?outcome, "workflow finished");
    Ok(snapshot)
}

#[cfg(test)]
mod tests;
