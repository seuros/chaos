use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::store::EMBEDDING_DIM;

const MODEL: &str = "integration-test-model";

mod memory;
mod timestamps;

/// Explicitly ignored rather than silently reporting success without a database.
/// Every test owns its schema; no documents in the mounted production schema are touched.
pub(crate) struct Fixture {
    pub store: PgRecallStore,
    admin: PgPool,
    schema: String,
}

impl Fixture {
    pub async fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("set TEST_DATABASE_URL to run ignored PostgreSQL tests");
        let admin = PgPool::connect(&url).await.expect("connect test database");
        let mut tx = admin.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(724310621)")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA public")
            .execute(&mut *tx)
            .await
            .expect("install pgvector in test database");
        tx.commit().await.unwrap();
        let schema = format!(
            "recall_test_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin)
            .await
            .expect("create isolated test schema");
        let search_path = format!("{schema},public");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |conn, _| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query("SELECT set_config('search_path', $1, false)")
                        .bind(search_path)
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .expect("connect scoped test pool");
        Self {
            store: PgRecallStore::new(pool),
            admin,
            schema,
        }
    }

    pub fn pool(&self) -> PgPool {
        self.store.pool.clone()
    }

    pub async fn ready() -> Self {
        let fixture = Self::new().await;
        fixture.store.migrate().await.unwrap();
        fixture.store.bind_model(MODEL).await.unwrap();
        fixture
    }

    pub async fn cleanup(self) {
        self.store.pool.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA {} CASCADE",
            self.schema
        )))
        .execute(&self.admin)
        .await
        .expect("remove test schema");
        self.admin.close().await;
    }
}

fn vector() -> Vec<f32> {
    let mut vector = vec![0.0; EMBEDDING_DIM];
    vector[0] = 1.0;
    vector
}

fn doc(scope: RecallScope, id: &str) -> RecallDoc {
    RecallDoc {
        scope,
        id: id.to_owned(),
        content: "RecallService pgvector exact_identifier provenance".to_owned(),
        embedding: vector(),
        model_fingerprint: MODEL.into(),
        metadata: serde_json::json!({"source": "integration"}),
    }
}

fn filter(scope: RecallScope) -> RetrievalFilter {
    RetrievalFilter {
        scope,
        include_global: false,
        limit: 5,
        model_fingerprint: MODEL.into(),
        automatic_only: false,
    }
}

async fn selections(fixture: &Fixture, scope: &RecallScope) -> Vec<crate::MemorySelection> {
    fixture
        .store
        .lexical_search("RecallService", &filter(scope.clone()))
        .await
        .unwrap()
        .into_iter()
        .map(|item| crate::MemorySelection {
            scope: item.scope,
            id: item.id,
            revision: item.handle.revision,
        })
        .collect()
}

async fn receipt(fixture: &Fixture, scope: &RecallScope) -> crate::UseReceipt {
    use crate::MemoryStore;

    fixture
        .store
        .issue_receipt("owner", "generation", &selections(fixture, scope).await)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and pgvector"]
async fn scoped_round_trip_and_explicit_global_inclusion() {
    let fixture = Fixture::ready().await;
    let project = RecallScope::Project("chaos".into());
    let other = RecallScope::Project("other".into());
    let session = RecallScope::Session("session".into());
    fixture
        .store
        .index_batch(vec![
            doc(project.clone(), "same"),
            doc(other.clone(), "same"),
            doc(session.clone(), "same"),
            doc(RecallScope::Global, "same"),
        ])
        .await
        .unwrap();
    // A post-filtered global top-k would lose the requested scope here.
    fixture
        .store
        .index_batch(
            (0..20)
                .map(|index| doc(other.clone(), &format!("distractor-{index}")))
                .collect(),
        )
        .await
        .unwrap();
    let mut request = SearchRequest {
        filter: filter(project.clone()),
        query_vec: vector(),
    };
    request.filter.limit = 1;
    let results = fixture.store.search(&request).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].scope, project);
    assert!(results[0].score > 0.99);
    assert_eq!(results[0].metadata["source"], "integration");
    let results = fixture
        .store
        .lexical_search("exact_identifier", &request.filter)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].scope, project);
    request.filter.limit = 5;
    request.filter.include_global = true;
    for results in [
        fixture.store.search(&request).await.unwrap(),
        fixture
            .store
            .lexical_search("RecallService", &request.filter)
            .await
            .unwrap(),
    ] {
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .all(|result| result.scope == project || result.scope == RecallScope::Global)
        );
    }
    fixture.store.delete(&project, "same").await.unwrap();
    request.filter.include_global = false;
    assert!(fixture.store.search(&request).await.unwrap().is_empty());
    for scope in [other, session, RecallScope::Global] {
        request.filter.scope = scope;
        let expected = if request.filter.scope == RecallScope::Project("other".into()) {
            5
        } else {
            1
        };
        assert_eq!(
            fixture.store.search(&request).await.unwrap().len(),
            expected
        );
    }
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and pgvector"]
async fn migration_is_idempotent_and_has_an_independent_ledger() {
    let fixture = Fixture::new().await;
    sqlx::raw_sql(
        "CREATE TABLE _sqlx_migrations (marker INTEGER); INSERT INTO _sqlx_migrations VALUES (42)",
    )
    .execute(&fixture.store.pool)
    .await
    .unwrap();
    fixture.store.migrate().await.unwrap();
    fixture.store.migrate().await.unwrap();
    fixture.store.bind_model(MODEL).await.unwrap();
    let scope = RecallScope::Project("project".into());
    fixture
        .store
        .index(doc(scope.clone(), "memory"))
        .await
        .unwrap();
    fixture.store.migrate().await.unwrap();
    assert_eq!(
        fixture
            .store
            .lexical_search("RecallService", &filter(scope))
            .await
            .unwrap()
            .len(),
        1
    );
    let marker: i32 = sqlx::query_scalar("SELECT marker FROM _sqlx_migrations")
        .fetch_one(&fixture.store.pool)
        .await
        .unwrap();
    assert_eq!(marker, 42);
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _recall_migrations WHERE success")
        .fetch_one(&fixture.store.pool)
        .await
        .unwrap();
    assert_eq!(applied, 1);
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT recall_store($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
        )
        .bind("unknown")
        .bind("")
        .bind("invalid")
        .bind("memory")
        .bind(serde_json::json!({}))
        .bind(Vector::from(vector()))
        .bind(MODEL)
        .bind(serde_json::json!([]))
        .bind(false)
        .bind(0.5f64)
        .fetch_one(&fixture.store.pool)
        .await
        .is_err()
    );
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and pgvector"]
async fn model_mismatch_and_invalid_batch_leave_storage_unchanged() {
    let fixture = Fixture::ready().await;
    assert!(matches!(
        fixture.store.bind_model("different-model").await,
        Err(RecallError::ModelMismatch)
    ));
    let valid = doc(RecallScope::Global, "valid");
    let mut invalid = doc(RecallScope::Global, "invalid");
    invalid.embedding[0] = f32::NAN;
    assert!(
        fixture
            .store
            .index_batch(vec![valid.clone(), invalid])
            .await
            .is_err()
    );
    let mut incompatible = doc(RecallScope::Global, "incompatible");
    incompatible.model_fingerprint = "different-model".into();
    assert!(matches!(
        fixture.store.index_batch(vec![valid, incompatible]).await,
        Err(RecallError::ModelMismatch)
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM recall_docs")
        .fetch_one(&fixture.store.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "batch transaction must roll back earlier writes");
    let request = SearchRequest {
        filter: RetrievalFilter {
            model_fingerprint: "different-model".into(),
            ..filter(RecallScope::Global)
        },
        query_vec: vector(),
    };
    assert!(matches!(
        fixture.store.search(&request).await,
        Err(RecallError::ModelMismatch)
    ));
    assert!(matches!(
        fixture
            .store
            .lexical_search("RecallService", &request.filter)
            .await,
        Err(RecallError::ModelMismatch)
    ));
    fixture.cleanup().await;
}
