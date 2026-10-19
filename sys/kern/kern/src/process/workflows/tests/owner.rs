use super::*;
use anyhow::Result;
use chaos_ipc::models::{ResponseInputItem, ResponseItem};
use chaos_ipc::protocol::{Op, Submission};
use chaos_synopsis::runs::{RunError, RunKind, RunOutcome, RunStatus, StepOutcome};
use serde_json::json;

/// A dedicated native process's real admission loop, without a model turn.
struct NativeProcess {
    session: Arc<Session>,
    tx: async_channel::Sender<Submission>,
    task: AbortOnDropHandle<()>,
}

impl NativeProcess {
    async fn new() -> anyhow::Result<Self> {
        let (session, _) = make_session_and_context().await;
        let session = Arc::new(session);
        let (tx, rx) = async_channel::bounded(8);
        let task = AbortOnDropHandle::new(tokio::spawn(
            crate::chaos::submission_loop::submission_loop(
                session.clone(),
                session.get_config().await,
                rx,
            ),
        ));
        Ok(Self { session, tx, task })
    }

    async fn open_disabled(&self, owner: &WorkflowOwner) -> anyhow::Result<WorkflowScope> {
        Ok(tokio::time::timeout(
            Duration::from_secs(5),
            self.session
                .services
                .workflow_control
                .open_owned(owner.clone()),
        )
        .await??)
    }

    async fn open(&self, owner: &WorkflowOwner) -> anyhow::Result<WorkflowScope> {
        let scope = self.open_disabled(owner).await?;
        self.session
            .services
            .tool_group_catalog
            .set_groups_enabled(
                self.session.tool_group_state(),
                [
                    crate::tools::groups::WORKFLOWS,
                    crate::tools::groups::AGENTS,
                ],
                true,
            )?;
        Ok(scope)
    }

    async fn idle(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.session.active_turn.lock().await.is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("operation turn ends");
    }

    async fn shutdown(self) {
        self.tx
            .send(Submission {
                id: "shutdown".into(),
                op: Op::Shutdown,
                trace: None,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .expect("submission loop shuts down")
            .unwrap();
    }
}

fn condition() -> Value {
    json!({
        "type":"condition", "id":"check", "pointer":"/input",
        "predicate":{"op":"exists"}
    })
}

#[tokio::test]
async fn sibling_operation_processes_share_mutation_scheduling_but_other_owners_do_not()
-> anyhow::Result<()> {
    let sdk = McpSession::with_id("scheduler-owner");
    let owner = WorkflowOwner::new(&sdk);
    let foreign_owner = WorkflowOwner::new(&McpSession::with_id("scheduler-foreign"));
    let first = NativeProcess::new().await?;
    let second = NativeProcess::new().await?;
    let foreign = NativeProcess::new().await?;
    let first_scope = first.open_disabled(&owner).await?;
    let second_scope = second.open_disabled(&owner).await?;
    let foreign_scope = foreign.open_disabled(&foreign_owner).await?;

    let mutation = first.session.tool_scheduler().acquire(false).await?;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            second.session.tool_scheduler().acquire(false),
        )
        .await
        .is_err(),
        "sibling mutations must not overlap"
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            second.session.tool_scheduler().acquire(true),
        )
        .await
        .is_err(),
        "reads must not overlap a sibling mutation"
    );
    let independent = tokio::time::timeout(
        Duration::from_secs(1),
        foreign.session.tool_scheduler().acquire(false),
    )
    .await??;
    drop(independent);
    drop(mutation);

    let first_read = first.session.tool_scheduler().acquire(true).await?;
    let second_read = tokio::time::timeout(
        Duration::from_secs(1),
        second.session.tool_scheduler().acquire(true),
    )
    .await??;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            first.session.tool_scheduler().acquire(false),
        )
        .await
        .is_err(),
        "mutation waits for both readers"
    );
    drop(first_read);
    drop(second_read);
    drop(second.session.tool_scheduler().acquire(false).await?);
    first_scope.close();
    second_scope.close();
    foreign_scope.close();
    first.shutdown().await;
    second.shutdown().await;
    foreign.shutdown().await;
    Ok(())
}

async fn native_workflow(scope: &WorkflowScope) -> anyhow::Result<Value> {
    use crate::tools::parallel::ToolCallRuntime;
    use crate::tools::router::{ToolCallSource, ToolRouter};

    let invocation = &scope.0.invocation;
    let router = crate::chaos::built_tools(
        &invocation.session,
        &invocation.turn,
        &[],
        &scope.0.lifetime,
    )
    .await?;
    let call = ToolRouter::build_tool_call(
        &invocation.session,
        ResponseItem::FunctionCall {
            id: None,
            name: "run_workflow".into(),
            namespace: None,
            arguments: json!({"tree":condition()}).to_string(),
            call_id: "native-owner-test".into(),
            provider_metadata: None,
        },
    )
    .await?
    .expect("native workflow call");
    let result = ToolCallRuntime::new(
        router,
        invocation.session.clone(),
        invocation.turn.clone(),
        invocation.tracker.clone(),
    )
    .handle_tool_call_with_source(call, ToolCallSource::Direct, scope.0.lifetime.clone())
    .await?;
    anyhow::ensure!(
        result.result.success_for_logging(),
        "{}",
        result.result.log_preview()
    );
    let ResponseInputItem::FunctionCallOutput { output, .. } = result.into_response() else {
        panic!("expected native workflow output");
    };
    Ok(serde_json::from_str(
        &output.body.to_text().expect("workflow snapshot"),
    )?)
}

async fn model_sees(scope: &WorkflowScope, tool_name: &str) -> anyhow::Result<bool> {
    let invocation = &scope.0.invocation;
    let turn = invocation
        .session
        .try_effective_turn_context(&invocation.turn)
        .await?;
    let router =
        crate::chaos::built_tools(&invocation.session, &turn, &[], &scope.0.lifetime).await?;
    Ok(router
        .model_visible_specs()
        .iter()
        .any(|spec| spec.name() == tool_name))
}

#[tokio::test]
async fn dispatched_group_controls_share_only_the_owner_and_revoke_sibling_roots()
-> anyhow::Result<()> {
    let sdk = McpSession::with_id("shared-groups");
    let foreign_sdk = McpSession::with_id("foreign-groups");
    let owner = WorkflowOwner::new(&sdk);
    // Even a different owner with the same SDK ID has independent group state.
    let same_id_owner = WorkflowOwner::new(&sdk);
    let foreign_owner = WorkflowOwner::new(&foreign_sdk);
    let first = NativeProcess::new().await?;
    let second = NativeProcess::new().await?;
    let same_id = NativeProcess::new().await?;
    let foreign = NativeProcess::new().await?;
    let native = NativeProcess::new().await?;
    let first_scope = first.open_disabled(&owner).await?;
    let sibling = second.open_disabled(&owner).await?;
    let same_id_scope = same_id.open_disabled(&same_id_owner).await?;
    let foreign_scope = foreign.open_disabled(&foreign_owner).await?;
    let native_scope = native
        .session
        .services
        .workflow_control
        .open("native")
        .await?;
    let server = Server::builder("group-owner-tests", "1").build();
    let logger = server.logger_for_session(sdk.id());

    for scope in [
        &first_scope,
        &sibling,
        &same_id_scope,
        &foreign_scope,
        &native_scope,
    ] {
        assert!(!model_sees(scope, "run_workflow").await?);
        assert!(!model_sees(scope, "run_synopsis").await?);
    }
    for scope in [&first_scope, &sibling] {
        assert!(matches!(
            scope.run(ExecutionContext::new(json!({"tree":condition()}), &sdk, &logger)).await,
            Err(ToolError::Execution(message)) if message.contains("workflows are not enabled")
        ));
    }

    // No direct catalog writes: this is the real top-level enable_tools
    // dispatcher running on the first dedicated operation process.
    first_scope
        .configure_groups(
            ExecutionContext::new(json!({"groups":["workflows"]}), &sdk, &logger),
            true,
        )
        .await?;
    assert!(model_sees(&first_scope, "run_workflow").await?);
    assert!(model_sees(&sibling, "run_workflow").await?);
    assert!(!model_sees(&sibling, "run_synopsis").await?);
    let result = sibling
        .run(ExecutionContext::new(
            json!({"tree":condition()}),
            &sdk,
            &logger,
        ))
        .await?
        .into_payload();
    assert_eq!(result["status"]["outcome"], "success");
    assert_eq!(
        native_workflow(&sibling).await?["status"]["outcome"],
        "success"
    );
    for (scope, session) in [(&same_id_scope, &sdk), (&foreign_scope, &foreign_sdk)] {
        assert!(!model_sees(scope, "run_workflow").await?);
        assert!(matches!(
            scope.run(ExecutionContext::new(json!({"tree":condition()}), session, &logger)).await,
            Err(ToolError::Execution(message)) if message.contains("workflows are not enabled")
        ));
    }
    assert!(!model_sees(&native_scope, "run_workflow").await?);
    assert!(native_workflow(&native_scope).await.is_err());

    first_scope
        .configure_groups(
            ExecutionContext::new(json!({"groups":["workflows"]}), &sdk, &logger),
            false,
        )
        .await?;
    assert!(!model_sees(&sibling, "run_workflow").await?);
    assert!(matches!(
        sibling.run(ExecutionContext::new(json!({"tree":condition()}), &sdk, &logger)).await,
        Err(ToolError::Execution(message)) if message.contains("workflows are not enabled")
    ));
    assert!(native_workflow(&sibling).await.is_err());
    // Revocation changes future dispatch, not retained status or native tasks.
    let retained = owner
        .status(ExecutionContext::new(
            json!({"run_id":result["run_id"]}),
            &sdk,
            &logger,
        ))?
        .into_payload();
    assert_eq!(retained["status"]["outcome"], "success");
    assert!(!sibling.0.lifetime.is_cancelled());

    first_scope
        .configure_groups(
            ExecutionContext::new(json!({"groups":["agents"]}), &sdk, &logger),
            true,
        )
        .await?;
    assert!(model_sees(&sibling, "run_synopsis").await?);
    assert!(!model_sees(&sibling, "run_workflow").await?);
    for scope in [&same_id_scope, &foreign_scope, &native_scope] {
        assert!(!model_sees(scope, "run_synopsis").await?);
        assert!(!model_sees(scope, "run_workflow").await?);
    }
    // The validated job is never visited; synopsis itself remains model-free.
    let synopsis_args = json!({
        "tree":{"type":"fallback","id":"root","children":[
            condition(), {"type":"job","id":"unused","job_id":"work"}
        ]},
        "jobs":[{"id":"work","message":"must not execute"}]
    });
    let synopsis = sibling
        .run_synopsis(ExecutionContext::new(synopsis_args.clone(), &sdk, &logger))
        .await?
        .into_payload();
    assert_eq!(synopsis["outcome"], "success");
    assert_eq!(synopsis["jobs"][0]["state"], "pending");
    first_scope
        .configure_groups(
            ExecutionContext::new(json!({"groups":["agents"]}), &sdk, &logger),
            false,
        )
        .await?;
    assert!(!model_sees(&sibling, "run_synopsis").await?);
    assert!(matches!(
        sibling
            .run_synopsis(ExecutionContext::new(synopsis_args, &sdk, &logger))
            .await,
        Err(ToolError::Execution(_))
    ));
    assert!(first.session.total_token_usage().await.is_none());
    assert!(second.session.total_token_usage().await.is_none());
    for scope in [
        &first_scope,
        &sibling,
        &same_id_scope,
        &foreign_scope,
        &native_scope,
    ] {
        scope.close();
    }
    first.shutdown().await;
    second.shutdown().await;
    same_id.shutdown().await;
    foreign.shutdown().await;
    native.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn owner_shares_four_slots_across_dedicated_native_processes() -> anyhow::Result<()> {
    let sdk = McpSession::with_id("shared-owner");
    let owner = WorkflowOwner::new(&sdk);
    let first = NativeProcess::new().await?;
    let second = NativeProcess::new().await?;
    let first_scope = first.open(&owner).await?;
    let second_scope = second.open(&owner).await?;
    assert!(!Arc::ptr_eq(&first.session, &second.session));
    assert_ne!(
        first_scope.0.invocation.turn.sub_id,
        second_scope.0.invocation.turn.sub_id
    );

    // Hold executor leases so the budget test cannot race a fast condition.
    let mut leases = vec![
        first_scope.0.registry.start(RunKind::Workflow)?,
        second_scope.0.registry.start(RunKind::Synopsis)?,
        first.session.workflow_runs().start(RunKind::Synopsis)?,
        second.session.workflow_runs().start(RunKind::Workflow)?,
    ];
    for registry in [
        owner.registry(),
        &first_scope.0.registry,
        &second_scope.0.registry,
        first.session.workflow_runs(),
        second.session.workflow_runs(),
    ] {
        assert!(matches!(
            registry.start(RunKind::Workflow),
            Err(RunError::ActiveLimit)
        ));
        for lease in &leases {
            assert_eq!(registry.status(lease.id())?.status, RunStatus::Running);
        }
    }
    let server = Server::builder("owner-tests", "1").build();
    let logger = server.logger_for_session(sdk.id());
    let error = second_scope
        .run(ExecutionContext::new(
            json!({"tree":condition()}),
            &sdk,
            &logger,
        ))
        .await
        .expect_err("fifth workflow refused");
    assert!(error.to_string().contains("four active runs"), "{error}");

    drop(leases.pop());
    let completed = second_scope
        .run(ExecutionContext::new(
            json!({"tree":condition()}),
            &sdk,
            &logger,
        ))
        .await?
        .into_payload();
    assert_eq!(completed["status"]["outcome"], "success");
    let run_id: RunId = serde_json::from_value(completed["run_id"].clone())?;
    assert_eq!(owner.registry().status(run_id)?.kind, RunKind::Workflow);

    owner.close();
    owner.close();
    for lease in &leases {
        assert!(lease.cancellation().is_cancelled());
        assert_eq!(
            second_scope.0.registry.status(lease.id())?.status,
            RunStatus::Cancelling
        );
    }
    assert!(matches!(
        first_scope.0.registry.start(RunKind::Workflow),
        Err(RunError::SessionClosed)
    ));
    drop(leases);
    first_scope.close();
    second_scope.close();
    first.shutdown().await;
    second.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn scope_close_drop_and_process_shutdown_leave_sibling_owner_work_alive() -> Result<()> {
    let sdk = McpSession::with_id("independent-operations");
    let owner = WorkflowOwner::new(&sdk);
    let first = NativeProcess::new().await?;
    let second = NativeProcess::new().await?;
    let scope = first.open(&owner).await?;
    let sibling = second.open(&owner).await?;
    let lease = sibling.0.registry.start(RunKind::Synopsis)?;

    scope.close();
    first.idle().await;
    assert!(!sibling.0.lifetime.is_cancelled());
    assert!(!lease.cancellation().is_cancelled());
    drop(scope);
    let reopened = first.open(&owner).await?;
    let lifetime = reopened.0.lifetime.clone();
    drop(reopened);
    assert!(
        lifetime.is_cancelled(),
        "last scope drop ends only its operation"
    );
    first.idle().await;
    first.shutdown().await;

    assert!(!sibling.0.lifetime.is_cancelled());
    assert!(!lease.cancellation().is_cancelled());
    assert_eq!(
        owner.registry().status(lease.id())?.status,
        RunStatus::Running
    );
    let server = Server::builder("owner-tests", "1").build();
    let logger = server.logger_for_session(sdk.id());
    let result = sibling
        .run(ExecutionContext::new(
            json!({"tree":condition()}),
            &sdk,
            &logger,
        ))
        .await?
        .into_payload();
    assert_eq!(result["status"]["outcome"], "success");
    // Reserving another slot after the first process is gone proves shutdown
    // did not close the shared registry while a sibling lease survives.
    let extra = owner.registry().start(RunKind::Workflow)?;
    assert!(!extra.cancellation().is_cancelled());
    owner.close();
    assert!(lease.cancellation().is_cancelled());
    assert!(extra.cancellation().is_cancelled());
    sibling.close();
    second.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn foreign_sdk_context_is_denied_before_owner_argument_parsing() -> anyhow::Result<()> {
    let sdk = McpSession::with_id("owner");
    let foreign = McpSession::with_id("foreign");
    let owner = WorkflowOwner::new(&sdk);
    let process = NativeProcess::new().await?;
    let scope = process.open(&owner).await?;
    let lease = owner.registry().start(RunKind::Workflow)?;
    let server = Server::builder("owner-tests", "1").build();
    let logger = server.logger_for_session(sdk.id());

    for params in [Value::Null, json!({"run_id":lease.id()})] {
        for result in [
            owner.status(ExecutionContext::new(params.clone(), &foreign, &logger)),
            owner.cancel(ExecutionContext::new(params.clone(), &foreign, &logger)),
            owner.dashboard(ExecutionContext::new(params, &foreign, &logger)),
        ] {
            assert!(matches!(
                result,
                Err(ToolError::Execution(message))
                    if message == "workflow owner belongs to another session"
            ));
        }
    }
    for result in [
        owner.status(ExecutionContext::new(Value::Null, &sdk, &logger)),
        owner.cancel(ExecutionContext::new(Value::Null, &sdk, &logger)),
        owner.dashboard(ExecutionContext::new(Value::Null, &sdk, &logger)),
    ] {
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }
    assert!(matches!(
        scope.run_synopsis(ExecutionContext::new(Value::Null, &foreign, &logger)).await,
        Err(ToolError::Execution(message))
            if message == "workflow scope belongs to another session"
    ));
    assert!(!lease.cancellation().is_cancelled());
    let cancelled = owner
        .cancel(ExecutionContext::new(
            json!({"run_id":lease.id()}),
            &sdk,
            &logger,
        ))?
        .into_payload();
    assert_eq!(cancelled["status"]["state"], "cancelling");
    assert!(lease.cancellation().is_cancelled());
    scope.close();
    process.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn same_sdk_id_cannot_replace_bound_owner_and_closed_owner_cannot_admit() -> Result<()> {
    let sdk = McpSession::with_id("same-id");
    let same_id_sdk = McpSession::with_id("same-id");
    let owner = WorkflowOwner::new(&sdk);
    let replacement = WorkflowOwner::new(&same_id_sdk);
    let process = NativeProcess::new().await?;
    let scope = process.open(&owner).await?;
    scope.close();
    drop(scope);
    process.idle().await;

    let error = process
        .open(&replacement)
        .await
        .err()
        .expect("no owner rebinding");
    assert!(error.to_string().contains("another owner"), "{error}");
    assert!(process.session.active_turn.lock().await.is_none());
    let error = process
        .session
        .services
        .workflow_control
        .open(sdk.id())
        .await
        .err()
        .expect("unowned admission cannot bypass binding");
    assert!(error.to_string().contains("bound owner"), "{error}");
    let scope = process.open(&owner.clone()).await?;
    scope.close();
    drop(scope);
    process.idle().await;

    owner.close();
    let fresh = NativeProcess::new().await?;
    for candidate in [&process, &fresh] {
        let error = candidate.open(&owner).await.err().expect("closed owner");
        assert!(error.to_string().contains("owner has closed"), "{error}");
        assert!(candidate.session.active_turn.lock().await.is_none());
    }
    // Refusal on a fresh process must not bind that process to a dead owner.
    let scope = fresh.open(&replacement).await?;
    scope.close();
    process.shutdown().await;
    fresh.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn native_workflow_and_synopsis_publish_into_external_owner_registry() -> anyhow::Result<()> {
    let sdk = McpSession::with_id("native-and-synopsis");
    let owner = WorkflowOwner::new(&sdk);
    let process = NativeProcess::new().await?;
    let scope = process.open(&owner).await?;
    let workflow = native_workflow(&scope).await?;
    assert_eq!(workflow["status"]["outcome"], "success");
    let server = Server::builder("owner-tests", "1").build();
    let logger = server.logger_for_session(sdk.id());
    // Every job is declared and validated, but the successful first branch
    // prevents agent spawning and therefore any provider/model request.
    let synopsis_args = json!({
        "tree":{"type":"fallback","id":"root","children":[
            condition(), {"type":"job","id":"unused","job_id":"work"}
        ]},
        "jobs":[{"id":"work","message":"must not execute"}]
    });
    let occupied = (0..4)
        .map(|_| owner.registry().start(RunKind::Workflow))
        .collect::<Result<Vec<_>, _>>()?;
    let error = native_workflow(&scope)
        .await
        .expect_err("native shared limit");
    assert!(error.to_string().contains("four active runs"), "{error}");
    let error = scope
        .run_synopsis(ExecutionContext::new(synopsis_args.clone(), &sdk, &logger))
        .await
        .expect_err("synopsis shared limit");
    assert!(error.to_string().contains("four active runs"), "{error}");
    drop(occupied);
    let synopsis = scope
        .run_synopsis(ExecutionContext::new(synopsis_args, &sdk, &logger))
        .await?
        .into_payload();
    assert_eq!(synopsis["outcome"], "success");
    assert_eq!(synopsis["jobs"][0]["state"], "pending");
    assert_ne!(workflow["run_id"], synopsis["run_id"]);
    for (result, kind) in [
        (&workflow, RunKind::Workflow),
        (&synopsis, RunKind::Synopsis),
    ] {
        let run_id: RunId = serde_json::from_value(result["run_id"].clone())?;
        let snapshot = owner.registry().status(run_id)?;
        assert_eq!(snapshot.kind, kind);
        assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Success));
        assert!(
            matches!(
                process.session.services.workflow_runs.status(run_id),
                Err(RunError::UnknownRun(_))
            ),
            "bound dispatch must not use the native fallback registry"
        );
        let status = owner
            .status(ExecutionContext::new(
                json!({"run_id":run_id}),
                &sdk,
                &logger,
            ))?
            .into_payload();
        assert_eq!(status["run_id"], result["run_id"]);
    }
    assert!(process.session.total_token_usage().await.is_none());
    assert!(process.session.clone_history().await.raw_items().is_empty());
    scope.close();
    process.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn owner_dashboard_is_bounded_metadata_only_and_status_retains_payloads() -> Result<()> {
    let sdk = McpSession::with_id("dashboard");
    let owner = WorkflowOwner::new(&sdk);
    let mut ids = Vec::new();
    for _ in 0..12 {
        let lease = owner.registry().start(RunKind::Workflow)?;
        lease.start_step("private-step", 1)?;
        lease.finish_step(
            "private-step",
            1,
            StepOutcome::Success,
            Some(json!({"secret":"detail"})),
        )?;
        ids.push(lease.id());
        lease.finish(RunOutcome::Success, Some(json!({"secret":"root"})))?;
    }
    let server = Server::builder("owner-tests", "1").build();
    let logger = server.logger_for_session(sdk.id());
    let mut seen = Vec::new();
    for (offset, count, next) in [(0, 10, json!(10)), (10, 2, Value::Null)] {
        let page = owner
            .dashboard(ExecutionContext::new(
                json!({"offset":offset}),
                &sdk,
                &logger,
            ))?
            .into_payload();
        assert_eq!(page["details_omitted"], true);
        assert_eq!(page["next_offset"], next);
        let runs = page["runs"].as_array().expect("dashboard entries");
        assert_eq!(runs.len(), count);
        for run in runs {
            assert!(run.get("result").is_none());
            assert!(run["steps"][0].get("detail").is_none());
            seen.push(serde_json::from_value::<RunId>(run["run_id"].clone())?);
        }
        assert!(!page.to_string().contains("secret"));
    }
    seen.sort();
    ids.sort();
    assert_eq!(seen, ids);
    let status = owner
        .status(ExecutionContext::new(
            json!({"run_id":ids[0]}),
            &sdk,
            &logger,
        ))?
        .into_payload();
    assert_eq!(status["result"]["secret"], "root");
    assert_eq!(status["steps"][0]["detail"]["secret"], "detail");
    Ok(())
}
