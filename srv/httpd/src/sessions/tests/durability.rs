//! Exercise real storage commits against the existing fake executor: only
//! ownership, crash boundaries and duplicate side effects belong here.
use super::*;
use crate::sessions::durable::{Client, Owner, Store};
use chaos_journald::{ExecutionRequest, ExecutionResponse, ExecutionTurnState};

mod descendants;
mod invocations;
mod isolation;
mod outbox;
mod recovery;
mod replay;
#[cfg(unix)]
mod restart;
pub(in crate::sessions) mod storage;
use storage::Database;

struct Fixture {
    _home: tempfile::TempDir,
    db: Database,
    store: Arc<Store>,
    manager: Arc<SessionManager>,
    session: Arc<Session>,
    commands: Option<mpsc::Receiver<Command>>,
    capacity: Arc<Semaphore>,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_policy(None).await
    }

    async fn with_policy(
        policy: Option<Arc<crate::sessions::durable::policy::PinnedPolicy>>,
    ) -> Self {
        let home = tempfile::tempdir().unwrap();
        let (db, client) = Database::open(home.path()).await;
        Self::with_storage(home, db, client, policy).await
    }

    async fn with_storage(
        home: tempfile::TempDir,
        db: Database,
        client: Client,
        policy: Option<Arc<crate::sessions::durable::policy::PinnedPolicy>>,
    ) -> Self {
        let store = Arc::new(Store {
            client,
            scope: "test".into(),
            policy_digest: Some(
                policy
                    .as_ref()
                    .map_or_else(|| "a".repeat(64), |pin| pin.digest().to_owned()),
            ),
            policy,
            recovery_only: false,
        });
        let capacity = Arc::new(Semaphore::new(1));
        let (manager, recovered) =
            SessionManager::restore(8, Duration::from_secs(3600), store.clone(), &capacity)
                .await
                .unwrap();
        assert!(recovered.is_empty());
        let manager = Arc::new(manager);
        let process_id = ProcessId::new();
        let (session, commands) = manager.reserve(Some(process_id)).unwrap();
        let (owner, _) = Owner::claim(
            store.clone(),
            Uuid::parse_str(&session.id).unwrap(),
            manager.daemon_id.clone(),
        )
        .await
        .unwrap();
        owner.bind_new(process_id).await.unwrap();
        session.ledger.set(owner).ok().unwrap();
        session.data.lock().unwrap().status = SessionStatus::Idle;
        Self {
            _home: home,
            db,
            store,
            manager,
            session,
            commands: Some(commands),
            capacity,
        }
    }

    fn start(&mut self) -> SupervisorFixture {
        start_supervisor(
            &self.session,
            self.commands.take().unwrap(),
            false,
            Duration::from_secs(3600),
        )
    }

    async fn admit(&self, id: &str) -> Result<TurnSnapshot, ApiError> {
        self.session
            .admit(
                id.into(),
                request("hello"),
                &self.capacity,
                Duration::from_secs(60),
            )
            .await
    }

    async fn persisted_turn(&self, id: &str) -> chaos_journald::ExecutionTurn {
        let ExecutionResponse::Turn(turn) = self
            .store
            .call(ExecutionRequest::GetTurn {
                session_id: Uuid::parse_str(&self.session.id).unwrap(),
                turn_id: Uuid::parse_str(id).unwrap(),
            })
            .await
            .unwrap()
        else {
            panic!("expected turn")
        };
        turn
    }
}

async fn operation(supervisor: &mut SupervisorFixture) -> Operation {
    tokio::time::timeout(Duration::from_secs(5), supervisor.operations.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn status(session: &Session, expected: SessionStatus) {
    tokio::time::timeout(
        Duration::from_secs(5),
        wait_until(session, |data| data.status == expected),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn failed_acceptance_never_dispatches_and_retains_the_reservation() {
    let mut fixture = Fixture::new().await;
    let mut supervisor = fixture.start();
    fixture.db.fail_transition("active").await;
    let id = Uuid::new_v4().to_string();
    assert!(fixture.admit(&id).await.is_err());
    assert_eq!(
        fixture
            .store
            .call(ExecutionRequest::GetTurn {
                session_id: Uuid::parse_str(&fixture.session.id).unwrap(),
                turn_id: Uuid::parse_str(&id).unwrap(),
            })
            .await
            .unwrap_err()
            .code,
        StatusCode::NOT_FOUND,
        "a failed acceptance must roll back its turn insert"
    );
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Shutdown
    ));
    status(&fixture.session, SessionStatus::Unknown).await;
    assert_eq!(fixture.capacity.available_permits(), 0);
    assert_eq!(
        fixture.admit(&id).await.unwrap().status,
        TurnStatus::Unknown
    );
    assert!(
        supervisor.operations.try_recv().is_err(),
        "no executor submission is allowed"
    );
    supervisor.task.abort();
    let _ = supervisor.task.await;
    fixture.db.close().await;
}

#[tokio::test]
async fn cancellation_during_dispatch_commit_never_submits_or_interrupts_another_turn() {
    let mut fixture = Fixture::new().await;
    let id = Uuid::new_v4().to_string();
    fixture.admit(&id).await.unwrap();
    let write_lock = fixture.db.lock(&fixture.session.id).await;
    let mut supervisor = fixture.start();
    tokio::time::timeout(
        Duration::from_secs(5),
        wait_until(&fixture.session, |data| data.turns[&id].request.is_none()),
    )
    .await
    .unwrap();
    fixture.session.interrupt(id).unwrap();
    write_lock.release().await;
    status(&fixture.session, SessionStatus::Idle).await;
    assert_eq!(fixture.capacity.available_permits(), 1);
    assert!(
        supervisor.operations.try_recv().is_err(),
        "no execute or stale interrupt"
    );
    fixture.session.close().unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Shutdown
    ));
    supervisor.task.await.unwrap();
    fixture.db.close().await;
}

#[tokio::test]
async fn failed_terminal_commit_stays_unknown_across_restart_and_cannot_replay() {
    let mut fixture = Fixture::new().await;
    let mut supervisor = fixture.start();
    let id = Uuid::new_v4().to_string();
    fixture.admit(&id).await.unwrap();
    assert!(matches!(operation(&mut supervisor).await, Operation::Execute(turn, _) if turn == id));
    assert_eq!(
        fixture.persisted_turn(&id).await.state,
        ExecutionTurnState::Running
    );
    fixture.db.fail_transition("idle").await;
    supervisor.events.send(complete(&id)).unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Shutdown
    ));
    status(&fixture.session, SessionStatus::Unknown).await;
    assert_eq!(fixture.capacity.available_permits(), 0);
    assert_eq!(
        fixture.persisted_turn(&id).await.state,
        ExecutionTurnState::Running
    );
    supervisor.task.abort();
    let _ = supervisor.task.await;
    fixture.db.expire(&fixture.session.id).await;

    let capacity = Arc::new(Semaphore::new(1));
    let (manager, recovered) = SessionManager::restore(
        8,
        Duration::from_secs(3600),
        fixture.store.clone(),
        &capacity,
    )
    .await
    .unwrap();
    assert!(
        recovered.is_empty(),
        "unknown work must not create a supervisor"
    );
    assert_eq!(capacity.available_permits(), 0);
    let session = manager.get(&fixture.session.id).unwrap();
    assert_eq!(
        session
            .admit(
                id.clone(),
                request("hello"),
                &capacity,
                Duration::from_secs(60)
            )
            .await
            .unwrap()
            .status,
        TurnStatus::Unknown
    );
    assert_eq!(
        session
            .admit(id, request("different"), &capacity, Duration::from_secs(60))
            .await
            .unwrap_err()
            .code,
        StatusCode::CONFLICT
    );
    assert_eq!(
        session
            .admit(
                Uuid::new_v4().to_string(),
                request("next"),
                &capacity,
                Duration::from_secs(60)
            )
            .await
            .unwrap_err()
            .code,
        StatusCode::CONFLICT
    );
    assert!(session.close().is_err(), "restart cannot prove containment");
    fixture.db.close().await;
}

#[tokio::test]
async fn orderly_handoff_restores_outcomes_and_only_dispatches_a_fresh_turn() {
    let mut fixture = Fixture::new().await;
    let mut supervisor = fixture.start();
    let first = Uuid::new_v4().to_string();
    fixture.admit(&first).await.unwrap();
    assert!(
        matches!(operation(&mut supervisor).await, Operation::Execute(turn, _) if turn == first)
    );
    supervisor.events.send(complete(&first)).unwrap();
    status(&fixture.session, SessionStatus::Idle).await;
    assert_eq!(fixture.capacity.available_permits(), 1);
    fixture.manager.close_all();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Shutdown
    ));
    supervisor.task.await.unwrap();
    fixture.manager.wait_stopped().await;

    let capacity = Arc::new(Semaphore::new(1));
    let (manager, mut recovered) = SessionManager::restore(
        8,
        Duration::from_secs(3600),
        fixture.store.clone(),
        &capacity,
    )
    .await
    .unwrap();
    let session = manager.get(&fixture.session.id).unwrap();
    let (_, mut commands, resume) = recovered.pop().unwrap();
    assert_eq!(
        resume.resume_process_id.unwrap().to_string(),
        fixture.session.snapshot().process_id.unwrap()
    );
    let old = session
        .admit(
            first.clone(),
            request("hello"),
            &capacity,
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(old.status, TurnStatus::Completed);
    assert_eq!(old.result.as_deref(), Some("done"));
    assert!(
        commands.try_recv().is_err(),
        "deduplication must not enqueue replay"
    );
    let second = Uuid::new_v4().to_string();
    session
        .admit(
            second.clone(),
            request("next"),
            &capacity,
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    let mut resumed = start_supervisor(&session, commands, false, Duration::from_secs(3600));
    assert!(matches!(operation(&mut resumed).await, Operation::Execute(turn, _) if turn == second));
    resumed.events.send(complete(&second)).unwrap();
    status(&session, SessionStatus::Idle).await;
    session.close().unwrap();
    assert!(matches!(operation(&mut resumed).await, Operation::Shutdown));
    resumed.task.await.unwrap();
    fixture.db.close().await;
}

#[tokio::test]
async fn a_fenced_supervisor_contains_work_without_releasing_unknown_capacity() {
    let mut fixture = Fixture::new().await;
    let mut supervisor = fixture.start();
    let id = Uuid::new_v4().to_string();
    fixture.admit(&id).await.unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Execute(_, _)
    ));
    fixture.db.expire(&fixture.session.id).await;
    let (_, successor) = Owner::claim(
        fixture.store.clone(),
        Uuid::parse_str(&fixture.session.id).unwrap(),
        "successor".into(),
    )
    .await
    .unwrap();
    assert_eq!(
        successor.state,
        chaos_journald::ExecutionSessionState::Unknown
    );
    assert!(fixture.session.ledger.get().unwrap().renew().await.is_err());
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Shutdown
    ));
    status(&fixture.session, SessionStatus::Unknown).await;
    assert_eq!(fixture.capacity.available_permits(), 0);
    assert!(fixture.admit(&Uuid::new_v4().to_string()).await.is_err());
    assert!(supervisor.operations.try_recv().is_err());
    supervisor.task.abort();
    let _ = supervisor.task.await;
    fixture.db.close().await;
}

#[tokio::test]
async fn restart_during_creation_or_boot_cannot_manufacture_or_resume_work() {
    let fixture = Fixture::new().await;
    for booted in [false, true] {
        let store = Arc::new(Store {
            client: fixture.store.client.clone(),
            scope: format!("startup-{booted}"),
            policy_digest: Some("a".repeat(64)),
            policy: None,
            recovery_only: false,
        });
        let id = Uuid::new_v4();
        let (owner, _) = Owner::claim(store.clone(), id, "boot-owner".into())
            .await
            .unwrap();
        if booted {
            owner.quarantine().await.unwrap();
            // Binding an allocated ID must not erase the boot quarantine.
            owner.bind_new(ProcessId::new()).await.unwrap();
        }
        fixture.db.expire(&id.to_string()).await;
        let capacity = Arc::new(Semaphore::new(1));
        let (manager, recovered) =
            SessionManager::restore(8, Duration::from_secs(3600), store, &capacity)
                .await
                .unwrap();
        assert!(
            recovered.is_empty(),
            "interrupted boot must not start another kernel"
        );
        let session = manager.get(&id.to_string()).unwrap();
        assert_eq!(
            session.snapshot().status,
            if booted {
                SessionStatus::Unknown
            } else {
                SessionStatus::Closed
            }
        );
        assert_eq!(capacity.available_permits(), usize::from(!booted));
        assert!(
            session
                .admit(
                    Uuid::new_v4().to_string(),
                    request("next"),
                    &capacity,
                    Duration::from_secs(60)
                )
                .await
                .is_err()
        );
    }
    fixture.db.close().await;
}

#[tokio::test]
async fn http_disconnect_cannot_cancel_the_acceptance_owner_or_free_its_ingress_slot() {
    use rama::Service;
    use rama::http::{Body, Request};
    let mut fixture = Fixture::new().await;
    let mut supervisor = fixture.start();
    let (_home, mut state) = crate::test_support::server_state(1024, false).await;
    let unique = Arc::get_mut(&mut state).unwrap();
    unique.sessions = Some(fixture.manager.clone());
    unique.semaphore = fixture.capacity.clone();
    unique.timeout = Duration::from_secs(60);
    let write_lock = fixture.db.lock(&fixture.session.id).await;
    let id = Uuid::new_v4().to_string();
    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/sessions/{}/turns/{id}", fixture.session.id))
        .header("authorization", "Bearer test-token")
        .header("x-chaos-daemon-id", &fixture.manager.daemon_id)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"request":"hello"}"#))
        .unwrap();
    let caller = tokio::spawn(async move { crate::api::http_service(state).serve(request).await });
    tokio::time::timeout(
        Duration::from_secs(5),
        wait_until(&fixture.session, |_| {
            fixture.session.admission.try_lock().is_err()
        }),
    )
    .await
    .unwrap();
    caller.abort();
    let _ = caller.await;
    assert_eq!(fixture.manager.ingress.available_permits(), 15);
    write_lock.release().await;
    assert!(matches!(operation(&mut supervisor).await, Operation::Execute(turn, _) if turn == id));
    supervisor.events.send(complete(&id)).unwrap();
    status(&fixture.session, SessionStatus::Idle).await;
    fixture.session.close().unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Shutdown
    ));
    supervisor.task.await.unwrap();
    fixture.db.close().await;
}
