use super::*;
use crate::config::test_config;
use chaos_synopsis::runs::{RunRegistry, RunSnapshot, RunStatus, StepOutcome, StepStatus};

fn job(id: &str, allow_replay: bool) -> NormalizedJob {
    NormalizedJob {
        id: id.into(),
        message: "work".into(),
        agent_type: None,
        allow_replay,
    }
}

fn node(value: Value) -> TreeNode {
    serde_json::from_value(value).expect("tree")
}

#[test]
fn validates_complete_job_coverage_and_replay_before_compiling() {
    let leaf = json!({"type":"job","id":"leaf","job_id":"work"});
    assert!(prepare(node(leaf.clone()), &[job("work", false)]).is_ok());
    assert!(prepare(node(leaf.clone()), &[job("missing", false)]).is_err());
    assert!(
        prepare(
            node(leaf.clone()),
            &[job("work", false), job("unused", false)]
        )
        .is_err()
    );
    assert!(prepare(node(json!({
        "type":"sequence","id":"root","children":[leaf,{"type":"job","id":"other","job_id":"work"}]
    })), &[job("work", false)]).is_err());
    let retry = json!({"type":"retry","id":"retry","attempts":2,"child":leaf});
    assert!(prepare(node(retry.clone()), &[job("work", false)]).is_err());
    assert!(prepare(node(retry), &[job("work", true)]).is_ok());
    assert!(
        prepare(
            node(json!({"type":"call","id":"call","tool":"anything"})),
            &[job("work", false)]
        )
        .is_err()
    );
}

#[tokio::test]
async fn cleanup_distinguishes_halted_setup_from_unvisited_jobs() {
    let attempts = Attempts::default();
    let jobs = vec![job("started", false), job("unvisited", false)];
    attempts.start(&jobs[0], "action/1".into());
    attempts.cleanup().await;
    let results = attempts.snapshot(&jobs);
    assert_eq!(results[0].state, SynopsisJobState::Cancelled);
    assert_eq!(results[1].state, SynopsisJobState::Pending);
}

#[derive(Default)]
struct Backend {
    spawned: Mutex<Vec<(ActionId, ProcessId)>>,
    statuses: Mutex<HashMap<ProcessId, watch::Sender<AgentStatus>>>,
    closed: Mutex<Vec<ProcessId>>,
}

impl AgentBackend for Backend {
    fn spawn(&self, id: ActionId, _: AgentAction) -> BackendFuture<Result<ProcessId, String>> {
        let process = ProcessId::new();
        let mut parts = id.as_str().rsplit('/');
        let attempt = parts.next().expect("attempt");
        let node = parts.next().expect("node");
        let status = match (node, attempt) {
            ("panicking", _) => AgentStatus::Completed(Some("panic".into())),
            ("worker", "1") | ("job_0", "1") => AgentStatus::Errored("first attempt".into()),
            ("slow", "1") | ("late", "1") => AgentStatus::Running,
            ("long", _) => AgentStatus::Running,
            ("many", attempt) if attempt.parse::<usize>().unwrap() <= 6 => {
                AgentStatus::Errored("retryable attempt".into())
            }
            _ => AgentStatus::Completed(Some("done".into())),
        };
        let delay = node == "late";
        let long = node == "long";
        self.spawned.lock().unwrap().push((id, process));
        let sender = watch::channel(status).0;
        if long {
            let sender = sender.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(121)).await;
                let _ = sender.send(AgentStatus::Completed(Some("long job done".into())));
            });
        }
        self.statuses.lock().unwrap().insert(process, sender);
        Box::pin(async move {
            if delay {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            Ok(process)
        })
    }
    fn subscribe_status(
        &self,
        id: ProcessId,
    ) -> BackendFuture<Result<watch::Receiver<AgentStatus>, String>> {
        let receiver = self.statuses.lock().unwrap().get(&id).unwrap().subscribe();
        Box::pin(async move { Ok(receiver) })
    }
    fn get_status(&self, id: ProcessId) -> BackendFuture<AgentStatus> {
        let status = self
            .statuses
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .borrow()
            .clone();
        Box::pin(async move { status })
    }
    fn get_agent_info(&self, id: ProcessId) -> BackendFuture<(Option<String>, Option<String>)> {
        let panics = matches!(
            &*self.statuses.lock().unwrap().get(&id).unwrap().borrow(),
            AgentStatus::Completed(Some(message)) if message == "panic"
        );
        Box::pin(async move {
            assert!(!panics, "simulated agent metadata panic");
            (None, None)
        })
    }
    fn shutdown(&self, id: ProcessId) -> BackendFuture<Result<(), String>> {
        self.closed.lock().unwrap().push(id);
        Box::pin(async { Ok(()) })
    }
}

struct TestLeaf {
    leaf: Leaf,
    observer: Arc<Observer>,
    attempts: Arc<Attempts>,
    backend: Arc<Backend>,
    run_id: RunId,
    cancellation: CancellationToken,
}

impl Tool for TestLeaf {
    fn name(&self) -> &str {
        &self.leaf.alias
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object"})
    }
    fn execute<'a>(&'a self, _: ExecutionContext<'a>) -> ToolFuture<'a> {
        Box::pin(async move {
            let attempt = self
                .observer
                .attempt(&self.leaf.node_id)
                .expect("started attempt");
            let id = format!("{}/{}/{}", self.run_id, self.leaf.node_id, attempt);
            let state = self.attempts.start(&self.leaf.job, id.clone());
            execute_job(
                state,
                id,
                &self.leaf.job.id,
                AgentAction {
                    config: test_config(),
                    input_items: Vec::new(),
                    session_source: process_spawn_source(ProcessId::new(), 1, None),
                },
                self.backend.clone(),
                self.cancellation.clone(),
            )
            .await
        })
    }
}

async fn run(
    root: TreeNode,
    jobs: Vec<NormalizedJob>,
) -> (RunSnapshot, Vec<SynopsisJobResult>, Arc<Backend>) {
    run_with_cancel(root, jobs, None).await
}

async fn run_with_cancel(
    root: TreeNode,
    jobs: Vec<NormalizedJob>,
    cancel_after: Option<Duration>,
) -> (RunSnapshot, Vec<SynopsisJobResult>, Arc<Backend>) {
    let (root, leaves) = prepare(root, &jobs).expect("preflight");
    let registry = RunRegistry::default();
    let lease = registry.start(RunKind::Synopsis).unwrap();
    let cancellation = lease.cancellation();
    let observer = Arc::new(Observer::new(
        lease.recorder(),
        cancellation.clone(),
        leaves
            .iter()
            .map(|leaf| (leaf.alias.clone(), leaf.job.id.clone()))
            .collect(),
    ));
    let attempts = Arc::new(Attempts::default());
    let backend = Arc::new(Backend::default());
    let server = bridge_server();
    for leaf in leaves {
        server
            .tool_registry()
            .register(TestLeaf {
                leaf,
                observer: observer.clone(),
                attempts: attempts.clone(),
                backend: backend.clone(),
                run_id: lease.id(),
                cancellation: cancellation.clone(),
            })
            .unwrap();
    }
    server
        .tool_registry()
        .register(
            TreeTool::new("execute", root)
                .unwrap()
                .with_observer(observer),
        )
        .unwrap();
    let session_id = lease.id().to_string();
    let mut execution = Box::pin(execute(&server, &session_id));
    let outcome = tokio::select! {
        result = execution.as_mut() => match result {
            Ok(value) if value["isError"] == true => RunOutcome::Failure,
            Ok(_) => RunOutcome::Success,
            Err(_) => RunOutcome::Error,
        },
        _ = async {
            match cancel_after {
                Some(delay) => tokio::time::sleep(delay).await,
                None => std::future::pending().await,
            }
        } => {
            let snapshot = registry.cancel(lease.id()).unwrap();
            assert_eq!(snapshot.status, RunStatus::Cancelling);
            RunOutcome::Cancelled
        }
    };
    drop(execution);
    drop(server);
    cancellation.cancel();
    attempts.cleanup().await;
    if outcome == RunOutcome::Cancelled {
        assert_eq!(
            registry.status(lease.id()).unwrap().status,
            RunStatus::Cancelling
        );
    }
    let snapshot = lease.finish(outcome, None).unwrap();
    let jobs = attempts.snapshot(&jobs);
    let spawned = backend.spawned.lock().unwrap();
    let closed = backend.closed.lock().unwrap();
    assert_eq!(spawned.len(), closed.len());
    for (_, process) in spawned.iter() {
        assert_eq!(closed.iter().filter(|id| *id == process).count(), 1);
    }
    drop(closed);
    drop(spawned);
    (snapshot, jobs, backend)
}

#[tokio::test]
async fn panicking_agent_returns_error_and_closes_spawned_child() {
    let (snapshot, _, backend) = run(
        node(json!({"type":"job","id":"panicking","job_id":"work"})),
        vec![job("work", false)],
    )
    .await;
    assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Error));
    let spawned = backend.spawned.lock().unwrap();
    assert_eq!(spawned.len(), 1);
    assert_eq!(*backend.closed.lock().unwrap(), vec![spawned[0].1]);
}

#[tokio::test(start_paused = true)]
async fn sdk_retry_uses_fresh_actions_children_and_structured_results() {
    let (snapshot, jobs, backend) = run(
        node(json!({
            "type":"sequence","id":"root","children":[
                {"type":"retry","id":"retry","attempts":2,"child":
                    {"type":"job","id":"worker","job_id":"work","result_key":"output"}},
                {"type":"condition","id":"check","pointer":"/output/state",
                    "predicate":{"op":"equals","value":"completed"}}
            ]
        })),
        vec![job("work", true)],
    )
    .await;
    assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Success));
    assert_eq!(jobs[0].id, "work");
    assert_eq!(jobs[0].state, SynopsisJobState::Completed);
    let worker = snapshot
        .steps
        .iter()
        .filter(|step| step.node_id == "worker")
        .collect::<Vec<_>>();
    assert_eq!(worker.len(), 2);
    assert_eq!(worker[0].status, StepStatus::Terminal(StepOutcome::Failure));
    assert_eq!(worker[1].attempt, 2);
    assert_eq!(worker[1].status, StepStatus::Terminal(StepOutcome::Success));
    assert_eq!(worker[1].detail.as_ref().unwrap()["tool"], "work");
    let spawned = backend.spawned.lock().unwrap();
    assert_eq!(spawned.len(), 2);
    assert_ne!(spawned[0].0, spawned[1].0);
    assert_ne!(spawned[0].1, spawned[1].1);
}

#[tokio::test(start_paused = true)]
async fn timeout_retry_closes_abandoned_attempt_without_overwriting_new_attempt() {
    let (snapshot, jobs, backend) = run(
        node(json!({
            "type":"retry","id":"retry","attempts":2,"child":{
                "type":"timeout","id":"timeout","timeout_ms":10,"child":{
                    "type":"job","id":"slow","job_id":"work"
                }
            }
        })),
        vec![job("work", true)],
    )
    .await;
    assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Success));
    assert_eq!(
        snapshot.steps[0].status,
        StepStatus::Terminal(StepOutcome::Cancelled)
    );
    assert_eq!(jobs[0].state, SynopsisJobState::Completed);
    assert_eq!(backend.spawned.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn timeout_drains_a_late_spawn_and_preserves_skipped_jobs() {
    let (snapshot, jobs, backend) = run(
        node(json!({
            "type":"sequence","id":"root","children":[
                {"type":"timeout","id":"timeout","timeout_ms":10,"child":
                    {"type":"job","id":"late","job_id":"work"}},
                {"type":"job","id":"skipped","job_id":"skipped"}
            ]
        })),
        vec![job("work", false), job("skipped", false)],
    )
    .await;
    assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Failure));
    assert_eq!(jobs[0].state, SynopsisJobState::Cancelled);
    assert_eq!(jobs[1].state, SynopsisJobState::Pending);
    assert_eq!(backend.spawned.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn race_closes_losers() {
    let (snapshot, _, _) = run(
        node(json!({
            "type":"race","id":"race","children":[
                {"type":"job","id":"slow","job_id":"slow"},
                {"type":"job","id":"done","job_id":"done"}
            ]
        })),
        vec![job("slow", false), job("done", false)],
    )
    .await;
    assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Success));
}

#[tokio::test(start_paused = true)]
async fn owned_run_cancellation_waits_for_late_spawn_cleanup() {
    let (snapshot, jobs, backend) = run_with_cancel(
        node(json!({"type":"job","id":"late","job_id":"work"})),
        vec![job("work", false)],
        Some(Duration::from_millis(10)),
    )
    .await;
    assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Cancelled));
    assert_eq!(
        snapshot.steps[0].status,
        StepStatus::Terminal(StepOutcome::Cancelled)
    );
    assert_eq!(jobs[0].state, SynopsisJobState::Cancelled);
    assert_eq!(backend.spawned.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn legacy_fallback_keeps_order_and_uses_the_same_step_lifecycle() {
    let jobs = vec![
        job("original/primary", false),
        job("original backup", false),
    ];
    let root = build_legacy_tree(SynopsisMode::Fallback, &jobs);
    let (snapshot, results, _) = run(root, jobs).await;
    assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Success));
    assert_eq!(results[0].id, "original/primary");
    assert_eq!(results[0].state, SynopsisJobState::Failed);
    assert_eq!(results[1].id, "original backup");
    assert_eq!(results[1].state, SynopsisJobState::Completed);
    assert_eq!(snapshot.steps.len(), 2);
    assert_eq!(snapshot.steps[0].attempt, 1);
    assert_eq!(
        snapshot.steps[1].detail.as_ref().unwrap()["tool"],
        "original backup"
    );
}

#[tokio::test(start_paused = true)]
async fn bridge_waits_for_agent_beyond_default_120_second_registry_timeout() {
    let started = tokio::time::Instant::now();
    let (snapshot, jobs, backend) = run(
        node(json!({"type":"job","id":"long","job_id":"work"})),
        vec![job("work", false)],
    )
    .await;
    assert!(started.elapsed() >= Duration::from_secs(121));
    assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Success));
    assert_eq!(jobs[0].state, SynopsisJobState::Completed);
    assert_eq!(backend.closed.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn nested_retries_are_not_cut_off_by_default_five_failure_breaker() {
    let (snapshot, jobs, backend) = run(
        node(json!({
            "type":"retry","id":"outer","attempts":3,"child":{
                "type":"retry","id":"inner","attempts":3,"child":{
                    "type":"job","id":"many","job_id":"work"
                }
            }
        })),
        vec![job("work", true)],
    )
    .await;
    assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Success));
    assert_eq!(jobs[0].state, SynopsisJobState::Completed);
    assert_eq!(snapshot.steps.len(), 7);
    assert!(
        snapshot.steps[..6]
            .iter()
            .all(|step| step.status == StepStatus::Terminal(StepOutcome::Failure))
    );
    assert_eq!(snapshot.steps[6].attempt, 7);
    assert_eq!(
        snapshot.steps[6].status,
        StepStatus::Terminal(StepOutcome::Success)
    );
    let spawned = backend.spawned.lock().unwrap();
    assert_eq!(spawned.len(), 7);
    assert_eq!(
        spawned
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<HashSet<_>>()
            .len(),
        7
    );
    assert_eq!(
        spawned
            .iter()
            .map(|(_, process)| *process)
            .collect::<HashSet<_>>()
            .len(),
        7
    );
}
