//! Agent leaves on the upstream SDK tree; no agent-specific scheduler.
use super::*;
use crate::chaos::{Session, TurnContext};
use chaos_synopsis::runs::{RunRecorder, StepOutcome};
use chaos_synopsis::tree::{Domain, ValidatedTree, compile_workflow};
use futures::FutureExt;
use mcp_host::bt::{Node as BtNode, Status, Step, StepInfo, TreeContext, TreeObserver, TreeTool};
use mcp_host::prelude::{ExecutionContext, Server, ToolError, ToolOutput as McpOutput};
use mcp_host::protocol::types::JsonRpcRequest;
use mcp_host::registry::tools::{Tool, ToolBreakerConfig, ToolFuture};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) fn failure(error: impl std::fmt::Display) -> FunctionCallError {
    FunctionCallError::RespondToModel(error.to_string())
}

struct Leaf {
    alias: String,
    node_id: String,
    job: NormalizedJob,
}

fn prepare(
    root: TreeNode,
    jobs: &[NormalizedJob],
) -> Result<(BtNode<Step>, Vec<Leaf>), FunctionCallError> {
    let declarations = jobs
        .iter()
        .map(|job| (job.id.clone(), job.allow_replay))
        .collect();
    let mut root = ValidatedTree::new(
        root,
        Domain::Agents {
            jobs: &declarations,
        },
    )
    .map_err(failure)?
    .into_root();
    let mut leaves = Vec::new();
    rewrite(&mut root, jobs, &mut leaves)?;
    let root = compile_workflow(root, &|alias| {
        leaves
            .iter()
            .any(|leaf| leaf.alias == alias && leaf.job.allow_replay)
    })
    .map_err(failure)?;
    Ok((root, leaves))
}

fn rewrite(
    node: &mut TreeNode,
    jobs: &[NormalizedJob],
    leaves: &mut Vec<Leaf>,
) -> Result<(), FunctionCallError> {
    match node {
        TreeNode::Sequence { children, .. }
        | TreeNode::Fallback { children, .. }
        | TreeNode::ParallelAll { children, .. }
        | TreeNode::Race { children, .. } => {
            for child in children {
                rewrite(child, jobs, leaves)?;
            }
        }
        TreeNode::Timeout { child, .. } | TreeNode::Retry { child, .. } => {
            rewrite(child, jobs, leaves)?;
        }
        TreeNode::Job {
            id,
            job_id,
            result_key,
        } => {
            // Complete domain validation above establishes that this job exists.
            let job = jobs
                .iter()
                .find(|job| &job.id == job_id)
                .ok_or_else(|| failure("validated job declaration is missing"))?;
            let alias = format!("job_{}", leaves.len());
            let id = std::mem::take(id);
            let result_key = result_key.take();
            leaves.push(Leaf {
                alias: alias.clone(),
                node_id: id.clone(),
                job: job.clone(),
            });
            *node = TreeNode::Call {
                id,
                tool: alias,
                arguments: json!({}),
                bindings: Vec::new(),
                result_key,
            };
        }
        _ => {}
    }
    Ok(())
}

struct Attempt {
    job_id: String,
    action_id: String,
    state: Arc<ExecutionState>,
}

#[derive(Default)]
struct Attempts(Mutex<Vec<Attempt>>);

impl Attempts {
    fn start(&self, job: &NormalizedJob, action_id: String) -> Arc<ExecutionState> {
        let activation = NormalizedJob {
            id: action_id.clone(),
            message: job.message.clone(),
            agent_type: job.agent_type.clone(),
            allow_replay: job.allow_replay,
        };
        let state = Arc::new(ExecutionState::new(&[activation]));
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Attempt {
                job_id: job.id.clone(),
                action_id,
                state: state.clone(),
            });
        state
    }

    async fn cleanup(&self) {
        let states = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|attempt| (attempt.action_id.clone(), attempt.state.clone()))
            .collect::<Vec<_>>();
        for (action_id, state) in states {
            // A timeout may halt configuration/policy refresh before a spawn
            // begins; it is still an attempted job, not an unvisited branch.
            state.mark_cancelled(&ActionId::new(action_id));
            state.shutdown_remaining().await;
        }
    }

    fn snapshot(&self, jobs: &[NormalizedJob]) -> Vec<SynopsisJobResult> {
        let attempts = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        jobs.iter()
            .map(|job| {
                let mut result = attempts
                    .iter()
                    .rev()
                    .find(|attempt| attempt.job_id == job.id)
                    .map(|attempt| {
                        attempt
                            .state
                            .snapshot(std::slice::from_ref(&attempt.action_id))
                            .remove(0)
                    })
                    .unwrap_or_else(|| lifecycle::Job::new(job).snapshot());
                result.id = job.id.clone();
                result
            })
            .collect()
    }
}

struct Observer {
    recorder: RunRecorder,
    cancellation: CancellationToken,
    attempts: Mutex<BTreeMap<String, usize>>,
    labels: BTreeMap<String, String>,
}

impl TreeObserver for Observer {
    fn started(&self, _: TreeContext<'_>, step: &StepInfo, attempt: usize) {
        if let Some(id) = step.id.as_deref()
            && self.recorder.start_step(id, attempt).is_ok()
        {
            self.attempts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(id.into(), attempt);
        } else {
            self.cancellation.cancel();
        }
    }

    fn finished(
        &self,
        _: TreeContext<'_>,
        step: &StepInfo,
        attempt: usize,
        status: Status,
        record: &Value,
    ) {
        let outcome = if status == Status::Success {
            StepOutcome::Success
        } else {
            StepOutcome::Failure
        };
        let mut record = record.clone();
        // Only relabel the SDK's tool field, never arbitrary agent output.
        if let Some(alias) = record.get("tool").and_then(Value::as_str)
            && let Some(job_id) = self.labels.get(alias)
        {
            record["tool"] = Value::String(job_id.clone());
        }
        if let Some(id) = step.id.as_deref()
            && self
                .recorder
                .finish_step(id, attempt, outcome, Some(record))
                .is_err()
        {
            self.cancellation.cancel();
        }
    }

    fn halted(&self, _: TreeContext<'_>, step: &StepInfo, attempt: usize) {
        if let Some(id) = step.id.as_deref() {
            let _ = self
                .recorder
                .finish_step(id, attempt, StepOutcome::Cancelled, None);
        }
    }
}

struct AgentLeaf {
    leaf: Leaf,
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    run_id: RunId,
    observer: Arc<Observer>,
    attempts: Arc<Attempts>,
    cancellation: CancellationToken,
}

impl Tool for AgentLeaf {
    fn name(&self) -> &str {
        &self.leaf.alias
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{},"additionalProperties":false})
    }
    fn execute<'a>(&'a self, _: ExecutionContext<'a>) -> ToolFuture<'a> {
        Box::pin(async move {
            let attempt = self
                .observer
                .attempts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&self.leaf.node_id)
                .copied()
                .ok_or_else(|| ToolError::Execution("missing job activation".into()))?;
            if self.cancellation.is_cancelled() || (attempt > 1 && !self.leaf.job.allow_replay) {
                return Err(ToolError::Execution(
                    "job cancelled or replay not allowed".into(),
                ));
            }
            // A new execution record and child spawn for every activation. Never
            // reuse a terminal job state or a prior attempt's resource lease.
            let action_id = format!("{}/{}/{}", self.run_id, self.leaf.node_id, attempt);
            let state = self.attempts.start(&self.leaf.job, action_id.clone());
            let action = match build_action(
                &self.session,
                &self.turn,
                &self.leaf.job,
                &self.cancellation,
            )
            .await
            {
                Ok(action) => action,
                Err(error) => {
                    state.mark_failed(&ActionId::new(action_id), error.to_string());
                    return Err(ToolError::Execution(error.to_string()));
                }
            };
            execute_job(
                state,
                action_id,
                &self.leaf.job.id,
                action,
                Arc::new(LiveAgentBackend {
                    control: self.session.services.agent_control.clone(),
                }),
                self.cancellation.clone(),
            )
            .await
        })
    }
}

async fn execute_job(
    state: Arc<ExecutionState>,
    action_id: String,
    job_id: &str,
    action: AgentAction,
    backend: Arc<dyn AgentBackend>,
    cancellation: CancellationToken,
) -> Result<McpOutput, ToolError> {
    // Also cancel detached spawns if a decorator halts this activation.
    let activation = cancellation.child_token();
    let _guard = activation.clone().drop_guard();
    let executor = AgentExecutor {
        backend,
        state: state.clone(),
        run_cancellation: cancellation,
    };
    let outcome = executor
        .execute(ActionId::new(action_id.clone()), action, activation)
        .await;
    let mut result = state.snapshot(&[action_id]).remove(0);
    result.id = job_id.into();
    let value =
        serde_json::to_value(result).map_err(|error| ToolError::Execution(error.to_string()))?;
    if outcome == ActionOutcome::Success {
        let Value::Object(value) = value else {
            return Err(ToolError::Execution("invalid agent job result".into()));
        };
        Ok(McpOutput::json(value))
    } else {
        Err(ToolError::Execution(value.to_string()))
    }
}

async fn build_action(
    session: &Session,
    turn: &Arc<TurnContext>,
    job: &NormalizedJob,
    cancellation: &CancellationToken,
) -> Result<AgentAction, FunctionCallError> {
    let turn = session.effective_turn_context(turn).await;
    let router = crate::chaos::built_tools(session, &turn, &[], cancellation)
        .await
        .map_err(failure)?;
    if !router
        .visible_specs()
        .iter()
        .any(|spec| spec.name() == "run_synopsis")
    {
        return Err(failure("agent synopses are not enabled for this caller"));
    }
    let child_depth = check_depth_limit(&turn.session_source, turn.config.agent_max_depth)?;
    let policy = session
        .child_mode_policy(turn.as_ref(), None, None, None)
        .await
        .map_err(failure)?;
    let mut config =
        build_agent_spawn_config(&session.get_base_instructions().await, turn.as_ref())?;
    apply_role_to_config(&mut config, job.agent_type.as_deref())
        .await
        .map_err(failure)?;
    apply_spawn_agent_runtime_overrides(&mut config, turn.as_ref())?;
    apply_spawn_agent_overrides(&mut config, child_depth);
    config.mode_policy_override = Some(policy);
    if cancellation.is_cancelled() {
        return Err(failure("synopsis cancelled"));
    }
    Ok(AgentAction {
        config,
        input_items: vec![UserInput::Text {
            text: job.message.clone(),
            text_elements: Vec::new(),
        }],
        session_source: process_spawn_source(
            session.conversation_id,
            child_depth,
            job.agent_type.as_deref(),
        ),
    })
}

pub(super) async fn run(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    root: TreeNode,
    jobs: Vec<NormalizedJob>,
    timeout_ms: i64,
) -> Result<RunSynopsisResult, FunctionCallError> {
    let (root, leaves) = prepare(root, &jobs)?;
    // Fail the entire setup before allocating a run slot or starting children,
    // including invalid roles in branches that might never execute.
    let preflight = CancellationToken::new();
    for job in &jobs {
        build_action(&session, &turn, job, &preflight).await?;
    }
    let labels = leaves
        .iter()
        .map(|leaf| (leaf.alias.clone(), leaf.job.id.clone()))
        .collect();
    let lease = session
        .workflow_runs()
        .start(RunKind::Synopsis)
        .map_err(failure)?;
    let cancellation = lease.cancellation();
    let mut guard = RunCancellationGuard::new(cancellation.clone());
    let observer = Arc::new(Observer {
        recorder: lease.recorder(),
        cancellation: cancellation.clone(),
        attempts: Mutex::default(),
        labels,
    });
    let attempts = Arc::new(Attempts::default());
    let server = bridge_server();
    for leaf in leaves {
        server
            .tool_registry()
            .register(AgentLeaf {
                leaf,
                session: session.clone(),
                turn: turn.clone(),
                run_id: lease.id(),
                observer: observer.clone(),
                attempts: attempts.clone(),
                cancellation: cancellation.clone(),
            })
            .map_err(failure)?;
    }
    server
        .tool_registry()
        .register(
            TreeTool::new("execute", root)
                .map_err(failure)?
                .with_observer(observer),
        )
        .map_err(failure)?;
    let session_id = lease.id().to_string();
    let mut execution = Box::pin(execute(&server, &session_id));
    let (outcome, error) = tokio::select! {
        biased;
        _ = cancellation.cancelled() => (RunSynopsisOutcome::Cancelled, None),
        _ = tokio::time::sleep(Duration::from_millis(timeout_ms as u64)) =>
            (RunSynopsisOutcome::TimedOut, None),
        result = execution.as_mut() => match result {
            Ok(value) if value["isError"] == true => (RunSynopsisOutcome::Failure, None),
            Ok(_) => (RunSynopsisOutcome::Success, None),
            Err(error) => (RunSynopsisOutcome::Error, Some(error.to_string())),
        },
    };
    // Drop the SDK scope first: halted callbacks record cancelled attempts;
    // then drain late spawns and close children before recording completion.
    drop(execution);
    drop(server);
    cancellation.cancel();
    attempts.cleanup().await;
    guard.disarm();
    let result = RunSynopsisResult {
        run_id: lease.id(),
        outcome,
        jobs: attempts.snapshot(&jobs),
        error,
    };
    lease
        .finish(outcome.into(), serde_json::to_value(&result).ok())
        .map_err(failure)?;
    Ok(result)
}

fn bridge_server() -> Server {
    Server::builder("chaos-agent-synopsis", "1")
        .with_tools(false)
        // The bounded tree owns retries and the outer deadline, not the
        // registry's default 120s timeout or five-failure circuit breaker.
        .with_circuit_breaker(ToolBreakerConfig {
            call_timeout_secs: 0.0,
            failure_threshold: chaos_synopsis::tree::MAX_LEAF_ATTEMPTS + 1,
            ..Default::default()
        })
        .build()
}

async fn execute(server: &Server, session_id: &str) -> Result<Value, FunctionCallError> {
    std::panic::AssertUnwindSafe(execute_inner(server, session_id))
        .catch_unwind()
        .await
        .map_err(|_| failure("agent synopsis execution panicked"))?
}

async fn execute_inner(server: &Server, session_id: &str) -> Result<Value, FunctionCallError> {
    let initialized = server
        .handle_request(
            session_id,
            JsonRpcRequest::new(
                json!(1),
                "initialize",
                Some(json!({
                    "protocolVersion":"2025-11-25","capabilities":{},
                    "clientInfo":{"name":"chaos-native","version":"1"}
                })),
            ),
        )
        .await
        .ok_or_else(|| failure("synopsis bridge initialization failed"))?;
    if initialized.error.is_some() {
        return Err(failure("synopsis bridge initialization rejected"));
    }
    server
        .handle_request(
            session_id,
            JsonRpcRequest::notification("notifications/initialized", None),
        )
        .await;
    let reply = server
        .handle_request(
            session_id,
            JsonRpcRequest::new(
                json!(2),
                "tools/call",
                Some(json!({"name":"execute","arguments":{}})),
            ),
        )
        .await
        .ok_or_else(|| failure("missing synopsis result"))?;
    if let Some(error) = reply.error {
        return Err(failure(error.message));
    }
    reply
        .result
        .ok_or_else(|| failure("missing synopsis payload"))
}

#[cfg(test)]
mod tests;
