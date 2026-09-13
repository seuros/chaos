use super::*;
use crate::config::test_config;

#[derive(Clone)]
struct FakePlan {
    delay: Duration,
    status: AgentStatus,
}

#[derive(Default)]
struct FakeBackend {
    plans: Mutex<HashMap<String, FakePlan>>,
    statuses: Mutex<HashMap<ProcessId, watch::Sender<AgentStatus>>>,
    shutdowns: Mutex<Vec<ProcessId>>,
}

impl FakeBackend {
    fn with_plans(plans: impl IntoIterator<Item = (&'static str, FakePlan)>) -> Arc<Self> {
        Arc::new(Self {
            plans: Mutex::new(
                plans
                    .into_iter()
                    .map(|(id, plan)| (id.to_string(), plan))
                    .collect(),
            ),
            ..Self::default()
        })
    }

    fn shutdown_count(&self) -> usize {
        self.shutdowns
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

impl AgentBackend for FakeBackend {
    fn spawn(
        &self,
        id: ActionId,
        _action: AgentAction,
    ) -> BackendFuture<Result<ProcessId, String>> {
        let plan = self
            .plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id.as_str());
        let Some(plan) = plan else {
            return Box::pin(async move { Err(format!("no plan for {id}")) });
        };
        let process_id = ProcessId::new();
        let (sender, _receiver) = watch::channel(AgentStatus::Running);
        self.statuses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(process_id, sender.clone());
        Box::pin(async move {
            tokio::spawn(async move {
                tokio::time::sleep(plan.delay).await;
                let _ = sender.send(plan.status);
            });
            Ok(process_id)
        })
    }

    fn subscribe_status(
        &self,
        process_id: ProcessId,
    ) -> BackendFuture<Result<watch::Receiver<AgentStatus>, String>> {
        let receiver = self
            .statuses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&process_id)
            .map(watch::Sender::subscribe)
            .ok_or_else(|| "missing status".to_string());
        Box::pin(async move { receiver })
    }

    fn get_status(&self, process_id: ProcessId) -> BackendFuture<AgentStatus> {
        let status = self
            .statuses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&process_id)
            .map(|sender| sender.borrow().clone())
            .unwrap_or(AgentStatus::NotFound);
        Box::pin(async move { status })
    }

    fn get_agent_info(
        &self,
        _process_id: ProcessId,
    ) -> BackendFuture<(Option<String>, Option<String>)> {
        Box::pin(async { (None, None) })
    }

    fn shutdown(&self, process_id: ProcessId) -> BackendFuture<Result<(), String>> {
        self.shutdowns
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(process_id);
        if let Some(sender) = self
            .statuses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&process_id)
        {
            let _ = sender.send(AgentStatus::Shutdown);
        }
        Box::pin(async { Ok(()) })
    }
}

fn job(id: &str) -> NormalizedJob {
    NormalizedJob {
        id: id.to_string(),
        message: format!("run {id}"),
        agent_type: None,
    }
}

fn action(id: &str) -> Node<AgentAction> {
    Node::action(
        id,
        AgentAction {
            config: test_config(),
            input_items: vec![UserInput::Text {
                text: format!("run {id}"),
                text_elements: Vec::new(),
            }],
            session_source: process_spawn_source(ProcessId::new(), 1, None),
        },
    )
}

async fn run_fake(
    root: Node<AgentAction>,
    jobs: &[NormalizedJob],
    backend: Arc<FakeBackend>,
    cancellation: CancellationToken,
) -> (Outcome, Arc<ExecutionState>) {
    let state = Arc::new(ExecutionState::new(jobs));
    let backend_dyn: Arc<dyn AgentBackend> = backend;
    let executor: Arc<dyn ActionExecutor<AgentAction>> = Arc::new(AgentExecutor {
        backend: Arc::clone(&backend_dyn),
        state: Arc::clone(&state),
        run_cancellation: cancellation.clone(),
    });
    let runner = Runner::new(Synopsis::new(root), executor).expect("valid synopsis");
    let outcome = runner.run(cancellation.clone()).await.expect("runner");
    cancellation.cancel();
    state.shutdown_remaining(backend_dyn).await;
    (outcome, state)
}

#[test]
fn normalizes_jobs_and_rejects_duplicates() {
    let jobs = normalize_jobs(vec![
        SynopsisJobArgs {
            id: " first ".to_string(),
            message: "one".to_string(),
            agent_type: Some(" scout ".to_string()),
        },
        SynopsisJobArgs {
            id: "first".to_string(),
            message: "two".to_string(),
            agent_type: None,
        },
    ]);
    assert!(jobs.is_err());
}

#[tokio::test(start_paused = true)]
async fn parallel_agents_complete_and_are_closed() {
    let jobs = vec![job("left"), job("right")];
    let backend = FakeBackend::with_plans([
        (
            "left",
            FakePlan {
                delay: Duration::from_millis(10),
                status: AgentStatus::Completed(Some("left done".to_string())),
            },
        ),
        (
            "right",
            FakePlan {
                delay: Duration::from_millis(20),
                status: AgentStatus::Completed(Some("right done".to_string())),
            },
        ),
    ]);

    let (outcome, state) = run_fake(
        Node::parallel_all([action("left"), action("right")]),
        &jobs,
        Arc::clone(&backend),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(outcome, Outcome::Success);
    assert_eq!(backend.shutdown_count(), 2);
    assert!(
        state
            .snapshot(&["left".to_string(), "right".to_string()])
            .iter()
            .all(|job| job.state == SynopsisJobState::Completed)
    );
}

#[tokio::test(start_paused = true)]
async fn errored_agent_fails_the_synopsis() {
    let jobs = vec![job("broken")];
    let backend = FakeBackend::with_plans([(
        "broken",
        FakePlan {
            delay: Duration::from_millis(10),
            status: AgentStatus::Errored("boom".to_string()),
        },
    )]);

    let (outcome, state) = run_fake(
        action("broken"),
        &jobs,
        Arc::clone(&backend),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(outcome, Outcome::Failure);
    assert_eq!(
        state.snapshot(&["broken".to_string()])[0].state,
        SynopsisJobState::Failed
    );
    assert_eq!(backend.shutdown_count(), 1);
}

#[tokio::test(start_paused = true)]
async fn fallback_runs_backup_after_errored_agent() {
    let jobs = vec![job("primary"), job("backup")];
    let backend = FakeBackend::with_plans([
        (
            "primary",
            FakePlan {
                delay: Duration::from_millis(10),
                status: AgentStatus::Errored("primary failed".to_string()),
            },
        ),
        (
            "backup",
            FakePlan {
                delay: Duration::from_millis(10),
                status: AgentStatus::Completed(Some("backup done".to_string())),
            },
        ),
    ]);

    let (outcome, state) = run_fake(
        Node::fallback([action("primary"), action("backup")]),
        &jobs,
        Arc::clone(&backend),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(outcome, Outcome::Success);
    assert_eq!(backend.shutdown_count(), 2);
    let results = state.snapshot(&["primary".to_string(), "backup".to_string()]);
    assert_eq!(results[0].state, SynopsisJobState::Failed);
    assert_eq!(
        results[0].status,
        Some(AgentStatus::Errored("primary failed".to_string()))
    );
    assert_eq!(results[1].state, SynopsisJobState::Completed);
    assert_eq!(
        results[1].status,
        Some(AgentStatus::Completed(Some("backup done".to_string())))
    );
}

#[tokio::test(start_paused = true)]
async fn fallback_runs_backup_after_spawn_failure_without_leaking_agent() {
    let jobs = vec![job("primary"), job("backup")];
    let backend = FakeBackend::with_plans([(
        "backup",
        FakePlan {
            delay: Duration::from_millis(10),
            status: AgentStatus::Completed(Some("backup done".to_string())),
        },
    )]);

    let (outcome, state) = run_fake(
        Node::fallback([action("primary"), action("backup")]),
        &jobs,
        Arc::clone(&backend),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(outcome, Outcome::Success);
    assert_eq!(backend.shutdown_count(), 1);
    let results = state.snapshot(&["primary".to_string(), "backup".to_string()]);
    assert_eq!(results[0].state, SynopsisJobState::Failed);
    assert_eq!(results[0].agent_id, None);
    assert_eq!(results[0].error.as_deref(), Some("no plan for primary"));
    assert_eq!(results[1].state, SynopsisJobState::Completed);
    assert!(results[1].agent_id.is_some());
}

#[tokio::test(start_paused = true)]
async fn race_cancels_and_closes_the_loser() {
    let jobs = vec![job("winner"), job("loser")];
    let backend = FakeBackend::with_plans([
        (
            "winner",
            FakePlan {
                delay: Duration::from_millis(10),
                status: AgentStatus::Completed(Some("won".to_string())),
            },
        ),
        (
            "loser",
            FakePlan {
                delay: Duration::from_secs(60),
                status: AgentStatus::Completed(Some("late".to_string())),
            },
        ),
    ]);

    let (outcome, state) = run_fake(
        Node::race([action("winner"), action("loser")]),
        &jobs,
        Arc::clone(&backend),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(outcome, Outcome::Success);
    assert_eq!(backend.shutdown_count(), 2);
    let results = state.snapshot(&["winner".to_string(), "loser".to_string()]);
    assert_eq!(results[0].state, SynopsisJobState::Completed);
    assert_eq!(results[1].state, SynopsisJobState::Cancelled);
}

#[tokio::test(start_paused = true)]
async fn external_cancellation_closes_running_agents() {
    let backend = FakeBackend::with_plans([(
        "slow",
        FakePlan {
            delay: Duration::from_secs(60),
            status: AgentStatus::Completed(Some("late".to_string())),
        },
    )]);
    let cancellation = CancellationToken::new();
    let runner_cancellation = cancellation.clone();
    let backend_for_task = Arc::clone(&backend);
    let handle = tokio::spawn(async move {
        run_fake(
            action("slow"),
            &[job("slow")],
            backend_for_task,
            runner_cancellation,
        )
        .await
    });
    tokio::task::yield_now().await;
    cancellation.cancel();

    let (outcome, state) = handle.await.expect("join");
    assert_eq!(outcome, Outcome::Cancelled);
    assert_eq!(backend.shutdown_count(), 1);
    assert_eq!(
        state.snapshot(&["slow".to_string()])[0].state,
        SynopsisJobState::Cancelled
    );
}
