//! A real SIGKILL/reopen boundary, not task cancellation. SQLite only: the
//! PostgreSQL matrix separately exercises production client fencing/failures.
use super::*;
use chaos_journald::{
    ExecutionDelivery, ExecutionDeliveryRequest as DeliveryRequest, InvocationPurpose,
    InvocationState,
};
use serde::{Deserialize, Serialize};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command as ProcessCommand};

const CHILD_HOME: &str = "CHAOS_HTTPD_CRASH_TEST_HOME";
const CHILD_TEST: &str = "sessions::tests::durability::restart::crash_child";

struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Serialize, Deserialize)]
struct Ready {
    session_id: String,
    turn_id: Uuid,
    invocation_id: Uuid,
    delivery: ExecutionDelivery,
}

async fn sqlite(path: &Path) -> Arc<chaos_journald::SqliteJournalStore> {
    Arc::new(
        chaos_journald::SqliteJournalStore::open(path)
            .await
            .unwrap(),
    )
}

async fn fixture(db: Arc<chaos_journald::SqliteJournalStore>, home: &Path) -> Fixture {
    Fixture::with_storage(
        tempfile::tempdir_in(home).unwrap(),
        Database::Sqlite(db.clone()),
        Client::Sqlite(db),
        None,
    )
    .await
}

async fn claim(store: &Store, owner: &str) -> ExecutionDelivery {
    let ExecutionResponse::Delivery(Some(delivery)) = store
        .call(ExecutionRequest::Delivery {
            request: DeliveryRequest::Claim {
                scope: store.scope.clone(),
                destination_digest: "d".repeat(64),
                owner_id: owner.into(),
                ttl_ms: 60_000,
            },
        })
        .await
        .unwrap()
    else {
        panic!("expected pending delivery")
    };
    delivery
}

#[tokio::test]
#[ignore = "subprocess entry point for killed_process_cannot_replay_or_lose_pending_usage"]
async fn crash_child() {
    let home = std::env::var_os(CHILD_HOME).expect("only invoked by crash drill");
    let home = Path::new(&home);
    let db = sqlite(&home.join("journal.sqlite")).await;
    let completed = fixture(db.clone(), home).await;
    outbox::terminal(&completed, Uuid::new_v4()).await;
    completed
        .session
        .ledger
        .get()
        .unwrap()
        .retire(false)
        .await
        .unwrap();
    completed
        .store
        .call(ExecutionRequest::Delivery {
            request: DeliveryRequest::Configure {
                scope: completed.store.scope.clone(),
                destination_digest: "d".repeat(64),
            },
        })
        .await
        .unwrap();
    let delivery = claim(&completed.store, "killed-delivery-worker").await;
    let mut active = fixture(db, home).await;
    let mut supervisor = active.start();
    let turn_id = Uuid::new_v4();
    active.admit(&turn_id.to_string()).await.unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Execute(turn, _) if turn == turn_id.to_string()
    ));
    let invocation = invocations::start(&active, turn_id, InvocationPurpose::Sampling);
    assert!(
        active
            .session
            .ledger
            .get()
            .unwrap()
            .invocation_sink()
            .begin(invocation.clone())
            .await
            .unwrap()
    );
    // Model a provider/tool effect AFTER its durable begin, with no receipt.
    std::fs::write(home.join("external-effect"), b"one invocation").unwrap();
    let ready = Ready {
        session_id: active.session.id.clone(),
        turn_id,
        invocation_id: invocation.invocation_id,
        delivery,
    };
    std::fs::write(home.join("ready"), serde_json::to_vec(&ready).unwrap()).unwrap();
    // Keep the supervisor/store alive until the parent kills the entire process.
    tokio::time::sleep(Duration::from_secs(60)).await;
    panic!("parent failed to terminate crash drill");
}

#[tokio::test]
async fn killed_process_cannot_replay_or_lose_pending_usage() {
    let home = tempfile::tempdir().unwrap();
    let mut child = KillOnDrop(
        ProcessCommand::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
            .env(CHILD_HOME, home.path())
            .spawn()
            .unwrap(),
    );
    let ready: Ready = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "crash child exited early"
            );
            if let Ok(bytes) = std::fs::read(home.path().join("ready"))
                && let Ok(ready) = serde_json::from_slice(&bytes)
            {
                break ready;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    child.0.kill().unwrap();
    let exit = child.0.wait().unwrap();
    assert_eq!(exit.signal(), Some(9), "must cross a SIGKILL boundary");
    let db = sqlite(&home.path().join("journal.sqlite")).await;
    let database = Database::Sqlite(db.clone());
    let store = Arc::new(Store {
        client: Client::Sqlite(db),
        scope: "test".into(),
        policy_digest: Some("a".repeat(64)),
        policy: None,
        recovery_only: false,
    });
    let capacity = Arc::new(Semaphore::new(1));
    assert!(
        SessionManager::restore(8, Duration::from_secs(3600), store.clone(), &capacity)
            .await
            .is_err(),
        "process death does not authorize stealing an unexpired lease"
    );
    // Fault-inject the storage clock boundary rather than waiting 90 seconds.
    // Operators must NEVER edit lease timestamps this way.
    database.expire(&ready.session_id).await;
    let (manager, recovered) =
        SessionManager::restore(8, Duration::from_secs(3600), store.clone(), &capacity)
            .await
            .unwrap();
    assert!(
        recovered.is_empty(),
        "must not boot any executor after the crash"
    );
    assert_eq!(capacity.available_permits(), 0);
    let session = manager.get(&ready.session_id).unwrap();
    assert_eq!(session.snapshot().status, SessionStatus::Unknown);
    assert_eq!(
        session
            .admit(
                ready.turn_id.to_string(),
                request("hello"),
                &capacity,
                Duration::from_secs(60)
            )
            .await
            .unwrap()
            .status,
        TurnStatus::Unknown
    );
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
    assert!(
        session.close().is_err(),
        "death is not external containment"
    );
    let accounting = session
        .ledger
        .get()
        .unwrap()
        .accounting(ready.turn_id)
        .await
        .unwrap();
    assert_eq!(accounting.invocations.len(), 1);
    assert_eq!(
        accounting.invocations[0].request.invocation_id,
        ready.invocation_id
    );
    assert_eq!(accounting.invocations[0].state, InvocationState::Unknown);
    assert!(accounting.invocations[0].receipt.is_none());
    assert!(accounting.summary.unwrap().known_usage.is_none());
    assert!(
        matches!(
            store
                .call(ExecutionRequest::Delivery {
                    request: DeliveryRequest::Claim {
                        scope: store.scope.clone(),
                        destination_digest: "d".repeat(64),
                        owner_id: "too-early".into(),
                        ttl_ms: 60_000,
                    },
                })
                .await
                .unwrap(),
            ExecutionResponse::Delivery(None)
        ),
        "crash does not expire the independent delivery lease"
    );
    database
        .make_delivery_due(ready.delivery.status.event_id)
        .await;
    let retried = claim(&store, "restarted-delivery-worker").await;
    assert_eq!(retried.body_json, ready.delivery.body_json);
    assert_eq!(retried.status.event_id, ready.delivery.status.event_id);
    assert_ne!(retried.lease.token, ready.delivery.lease.token);
    assert!(
        store
            .call(ExecutionRequest::Delivery {
                request: DeliveryRequest::Acknowledge {
                    scope: store.scope.clone(),
                    destination_digest: "d".repeat(64),
                    lease: ready.delivery.lease,
                }
            })
            .await
            .is_err(),
        "old acknowledgement cannot settle a new claim"
    );
    store
        .call(ExecutionRequest::Delivery {
            request: DeliveryRequest::Acknowledge {
                scope: store.scope.clone(),
                destination_digest: "d".repeat(64),
                lease: retried.lease,
            },
        })
        .await
        .unwrap();
    assert_eq!(
        capacity.available_permits(),
        0,
        "delivery cannot release execution"
    );
    assert_eq!(
        std::fs::read(home.path().join("external-effect")).unwrap(),
        b"one invocation"
    );
    let ExecutionResponse::Session(record) = store
        .call(ExecutionRequest::GetSession {
            session_id: Uuid::parse_str(&ready.session_id).unwrap(),
        })
        .await
        .unwrap()
    else {
        panic!("expected session");
    };
    session
        .resolve(crate::sessions::durable::recovery::ResolveRequest {
            resolution_id: Uuid::new_v4(),
            expected_process_id: record.process_id,
            expected_turn_id: Some(ready.turn_id),
            expected_recovery_epoch: record.recovery_epoch,
            containment_confirmed: true,
            actor: "crash-drill".into(),
            reason: "Fake worker SIGKILLed and reaped; only external effect was a completed marker write".into(),
        })
        .await
        .unwrap();
    assert_eq!(capacity.available_permits(), 1);
    let resolved = session
        .ledger
        .get()
        .unwrap()
        .accounting(ready.turn_id)
        .await
        .unwrap();
    assert_eq!(resolved.invocations, accounting.invocations);
    database.close().await;
}
