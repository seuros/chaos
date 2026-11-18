use super::*;
use crate::execution::outbox::{MAX_DELIVERY_ATTEMPTS, UNRESTORABLE};

const SCOPE: &str = "deliveries";

struct Fixture {
    _home: tempfile::TempDir,
    store: SqliteJournalStore,
    session_id: String,
}

impl Fixture {
    async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let store = SqliteJournalStore::open(&home.path().join("journal.sqlite"))
            .await
            .unwrap();
        let session_id = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO execution_scopes (scope) VALUES ($1)")
            .bind(SCOPE)
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO execution_sessions (session_id, scope, state, owner_id, expires_at_ms)
             VALUES ($1, $2, 'idle', '', 0)",
        )
        .bind(&session_id)
        .bind(SCOPE)
        .execute(store.pool())
        .await
        .unwrap();
        let outbox = Self {
            _home: home,
            store,
            session_id,
        };
        outbox
            .call(ExecutionDeliveryRequest::Configure {
                scope: SCOPE.into(),
                destination_digest: digest(),
            })
            .await
            .unwrap();
        outbox
    }

    async fn enqueue(&self) -> Uuid {
        let event_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO execution_outbox (event_id, event_key, scope, session_id, kind,
             created_at_ms, body_json, next_attempt_at_ms, observed_at_ms)
             VALUES ($1, $1, $2, $3, 'session.resolved', 1000, '{\"exact\":\"bytes\"}',
                     1000000, 1000000)",
        )
        .bind(event_id.to_string())
        .bind(SCOPE)
        .bind(&self.session_id)
        .execute(self.store.pool())
        .await
        .unwrap();
        event_id
    }

    async fn call(
        &self,
        request: ExecutionDeliveryRequest,
    ) -> Result<ExecutionResponse, JournalError> {
        self.store
            .execution(ExecutionRequest::Delivery { request })
            .await
    }

    async fn claim(&self, owner: &str) -> Option<ExecutionDelivery> {
        match self
            .call(ExecutionDeliveryRequest::Claim {
                scope: SCOPE.into(),
                destination_digest: digest(),
                owner_id: owner.into(),
                ttl_ms: 30_000,
            })
            .await
            .unwrap()
        {
            ExecutionResponse::Delivery(delivery) => delivery,
            other => panic!("expected a claim answer: {other:?}"),
        }
    }

    async fn acknowledge(&self, lease: &ExecutionDeliveryLease) -> Result<(), JournalError> {
        self.call(ExecutionDeliveryRequest::Acknowledge {
            scope: SCOPE.into(),
            destination_digest: digest(),
            lease: lease.clone(),
        })
        .await
        .map(|_| ())
    }

    async fn retry(&self, lease: &ExecutionDeliveryLease) -> Result<(), JournalError> {
        self.call(ExecutionDeliveryRequest::Retry {
            scope: SCOPE.into(),
            destination_digest: digest(),
            lease: lease.clone(),
        })
        .await
        .map(|_| ())
    }

    async fn status(&self, event_id: Uuid) -> ExecutionDeliveryStatus {
        let ExecutionResponse::Deliveries(records) = self
            .call(ExecutionDeliveryRequest::List {
                scope: SCOPE.into(),
                after: None,
                limit: 256,
            })
            .await
            .unwrap()
        else {
            panic!("expected deliveries");
        };
        records
            .into_iter()
            .find(|record| record.event_id == event_id)
            .unwrap()
    }

    async fn rewind(&self, event_id: Uuid, sql: &'static str) {
        sqlx::query(sql)
            .bind(event_id.to_string())
            .execute(self.store.pool())
            .await
            .unwrap();
    }

    async fn backoff_elapses(&self, event_id: Uuid) {
        self.rewind(
            event_id,
            "UPDATE execution_outbox SET next_attempt_at_ms = next_attempt_at_ms - failed_after_ms,
             expires_at_ms = expires_at_ms - failed_after_ms,
             observed_at_ms = observed_at_ms - failed_after_ms WHERE event_id = $1",
        )
        .await;
    }

    async fn lease_lapses(&self, event_id: Uuid) {
        self.rewind(
            event_id,
            "UPDATE execution_outbox SET next_attempt_at_ms = next_attempt_at_ms - 30000,
             expires_at_ms = expires_at_ms - 30000,
             observed_at_ms = observed_at_ms - 30000 WHERE event_id = $1",
        )
        .await;
    }
}

fn digest() -> String {
    "d".repeat(64)
}

#[tokio::test]
async fn delivery_is_claimed_once_and_acknowledged_idempotently() {
    let outbox = Fixture::new().await;
    let event_id = outbox.enqueue().await;
    let delivery = outbox.claim("worker").await.unwrap();
    assert_eq!(delivery.status.event_id, event_id);
    assert_eq!(delivery.status.attempts, 1);
    assert_eq!(delivery.status.state, ExecutionDeliveryState::Claimed);
    assert_eq!(delivery.body_json, "{\"exact\":\"bytes\"}");
    assert!(outbox.claim("rival").await.is_none());

    outbox.acknowledge(&delivery.lease).await.unwrap();
    let delivered = outbox.status(event_id).await;
    assert_eq!(delivered.state, ExecutionDeliveryState::Confirmed);
    assert!(delivered.delivered_at_ms.is_some());
    outbox.acknowledge(&delivery.lease).await.unwrap();
    assert_eq!(outbox.status(event_id).await, delivered);
    assert!(outbox.retry(&delivery.lease).await.is_err());
    assert!(outbox.claim("worker").await.is_none());
}

#[tokio::test]
async fn failed_delivery_backs_off_then_succeeds_and_fences_the_old_attempt() {
    let outbox = Fixture::new().await;
    let event_id = outbox.enqueue().await;
    let first = outbox.claim("worker").await.unwrap();
    outbox.retry(&first.lease).await.unwrap();
    let pending = outbox.status(event_id).await;
    assert_eq!(pending.state, ExecutionDeliveryState::Pending);
    outbox.retry(&first.lease).await.unwrap();
    assert_eq!(
        outbox.status(event_id).await,
        pending,
        "a repeated retry report changes nothing"
    );
    assert!(outbox.claim("worker").await.is_none(), "backoff holds it");

    outbox.backoff_elapses(event_id).await;
    let second = outbox.claim("worker").await.unwrap();
    assert_eq!(second.status.attempts, 2);
    assert_ne!(second.lease.token, first.lease.token);
    assert!(outbox.acknowledge(&first.lease).await.is_err());
    outbox.acknowledge(&second.lease).await.unwrap();
    assert_eq!(
        outbox.status(event_id).await.state,
        ExecutionDeliveryState::Confirmed
    );
}

#[tokio::test]
async fn exhausted_delivery_dies_and_is_never_redelivered() {
    let outbox = Fixture::new().await;
    let event_id = outbox.enqueue().await;
    let mut last = None;
    for attempt in 1..=MAX_DELIVERY_ATTEMPTS.get() {
        let delivery = outbox.claim("worker").await.unwrap();
        assert_eq!(delivery.status.attempts, u64::from(attempt));
        outbox.retry(&delivery.lease).await.unwrap();
        outbox.backoff_elapses(event_id).await;
        last = Some(delivery.lease);
    }
    let dead = outbox.status(event_id).await;
    assert_eq!(dead.state, ExecutionDeliveryState::Dead);
    assert_eq!(dead.dead_reason.as_deref(), Some("attempts exhausted"));
    assert_eq!(dead.attempts, u64::from(MAX_DELIVERY_ATTEMPTS.get()));
    assert!(outbox.claim("worker").await.is_none());
    let last = last.unwrap();
    assert!(outbox.acknowledge(&last).await.is_err());
    outbox.retry(&last).await.unwrap();
    assert_eq!(outbox.status(event_id).await, dead);
}

#[tokio::test]
async fn lapsed_leases_spend_attempts_until_the_delivery_dies() {
    let outbox = Fixture::new().await;
    let event_id = outbox.enqueue().await;
    for _ in 1..=MAX_DELIVERY_ATTEMPTS.get() {
        let delivery = outbox.claim("worker").await.unwrap();
        assert_eq!(delivery.status.state, ExecutionDeliveryState::Claimed);
        outbox.lease_lapses(event_id).await;
    }
    assert!(outbox.claim("worker").await.is_none());
    let dead = outbox.status(event_id).await;
    assert_eq!(dead.state, ExecutionDeliveryState::Dead);
    assert_eq!(dead.dead_reason.as_deref(), Some("attempts exhausted"));
}

#[tokio::test]
async fn unrestorable_rows_are_buried_instead_of_delivered() {
    let outbox = Fixture::new().await;
    let corrupt = outbox.enqueue().await;
    let healthy = outbox.enqueue().await;
    sqlx::query(
        "UPDATE execution_outbox SET delivery_state = 'claimed', expires_at_ms = 1,
         next_attempt_at_ms = 999 WHERE event_id = $1",
    )
    .bind(corrupt.to_string())
    .execute(outbox.store.pool())
    .await
    .unwrap();
    let delivery = outbox.claim("worker").await.unwrap();
    assert_eq!(delivery.status.event_id, healthy);
    let buried = outbox.status(corrupt).await;
    assert_eq!(buried.state, ExecutionDeliveryState::Dead);
    assert_eq!(buried.dead_reason.as_deref(), Some(UNRESTORABLE));
    assert_eq!(buried.attempts, 0);

    sqlx::query("UPDATE execution_outbox SET last_issued = 0 WHERE event_id = $1")
        .bind(healthy.to_string())
        .execute(outbox.store.pool())
        .await
        .unwrap();
    assert!(matches!(
        outbox.acknowledge(&delivery.lease).await,
        Err(JournalError::ExecutionConflict(_))
    ));
    let refused = outbox.status(healthy).await;
    assert_eq!(refused.state, ExecutionDeliveryState::Dead);
    assert!(refused.delivered_at_ms.is_none());
    assert!(outbox.claim("worker").await.is_none());
}
