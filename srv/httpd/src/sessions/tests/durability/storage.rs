//! Run the same fault scenarios on SQLite or, when TEST_DATABASE_URL is set,
//! the production PostgreSQL client. Each PostgreSQL fixture owns its schema.
use super::*;
use sqlx::{AssertSqlSafe, PgPool, Postgres, Sqlite, Transaction};

pub(super) enum Database {
    Sqlite(Arc<chaos_journald::SqliteJournalStore>),
    Postgres {
        pool: PgPool,
        admin: PgPool,
        schema: String,
    },
}

pub(super) enum WriteLock {
    Sqlite(Transaction<'static, Sqlite>),
    Postgres(Transaction<'static, Postgres>),
}

impl WriteLock {
    pub(super) async fn release(self) {
        match self {
            Self::Sqlite(tx) => tx.rollback().await.unwrap(),
            Self::Postgres(tx) => tx.rollback().await.unwrap(),
        }
    }
}

impl Database {
    pub(super) async fn open(home: &std::path::Path) -> (Self, Client) {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            let db = Arc::new(
                chaos_journald::SqliteJournalStore::open(&home.join("journal.sqlite"))
                    .await
                    .unwrap(),
            );
            return (Self::Sqlite(db.clone()), Client::Sqlite(db));
        };
        let admin = PgPool::connect(&database_url).await.unwrap();
        let schema = format!("httpd_test_{}", Uuid::new_v4().simple());
        // The identifier contains only a fixed prefix and a generated hex UUID.
        sqlx::raw_sql(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin)
            .await
            .unwrap();
        let mut url = url::Url::parse(&database_url).unwrap();
        url.query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        // Use the real runtime migrator and production JournalClient, not a
        // test-only PostgreSQL implementation of the execution API.
        let pool = chaos_proc::open_runtime_db_postgres_url(url.as_str())
            .await
            .unwrap();
        let client = Client::Journal(chaos_journald::JournalClient::postgres_pool(pool.clone()));
        (
            Self::Postgres {
                pool,
                admin,
                schema,
            },
            client,
        )
    }

    pub(super) async fn lock(&self, session_id: &str) -> WriteLock {
        match self {
            Self::Sqlite(db) => {
                WriteLock::Sqlite(db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap())
            }
            Self::Postgres { pool, .. } => {
                let mut tx = pool.begin().await.unwrap();
                sqlx::query(
                    "SELECT session_id FROM execution_sessions WHERE session_id = $1 FOR UPDATE",
                )
                .bind(session_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
                WriteLock::Postgres(tx)
            }
        }
    }

    pub(super) async fn expire(&self, session_id: &str) {
        let sql = "UPDATE execution_sessions SET expires_at_ms = 1 WHERE session_id = $1";
        match self {
            Self::Sqlite(db) => {
                sqlx::query(sql)
                    .bind(session_id)
                    .execute(db.pool())
                    .await
                    .unwrap();
            }
            Self::Postgres { pool, .. } => {
                sqlx::query(sql)
                    .bind(session_id)
                    .execute(pool)
                    .await
                    .unwrap();
            }
        }
    }

    pub(super) async fn fail_transition(&self, state: &str) {
        // Fixed fault-injection transitions, never caller-controlled SQL.
        assert!(matches!(state, "active" | "idle" | "closed"));
        match self {
            Self::Sqlite(db) => {
                sqlx::raw_sql(AssertSqlSafe(format!(
                    "CREATE TRIGGER fail_transition BEFORE UPDATE OF state ON execution_sessions
                     WHEN NEW.state = '{state}'
                     BEGIN SELECT RAISE(ABORT, 'injected transition failure'); END;"
                )))
                .execute(db.pool())
                .await
                .unwrap();
            }
            Self::Postgres { pool, .. } => {
                sqlx::raw_sql(AssertSqlSafe(format!(
                    "CREATE FUNCTION fail_transition() RETURNS trigger LANGUAGE plpgsql AS $$
                     BEGIN
                       IF NEW.state = '{state}' THEN
                         RAISE EXCEPTION 'injected transition failure';
                       END IF;
                       RETURN NEW;
                     END;
                     $$;
                     CREATE TRIGGER fail_transition BEFORE UPDATE OF state ON execution_sessions
                     FOR EACH ROW EXECUTE FUNCTION fail_transition();"
                )))
                .execute(pool)
                .await
                .unwrap();
            }
        }
    }

    pub(super) async fn clear_transition_fault(&self) {
        match self {
            Self::Sqlite(db) => {
                sqlx::raw_sql("DROP TRIGGER fail_transition")
                    .execute(db.pool())
                    .await
                    .unwrap();
            }
            Self::Postgres { pool, .. } => {
                sqlx::raw_sql(
                    "DROP TRIGGER fail_transition ON execution_sessions; DROP FUNCTION fail_transition();"
                ).execute(pool).await.unwrap();
            }
        }
    }

    pub(super) async fn close(&self) {
        match self {
            Self::Sqlite(db) => db.pool().close().await,
            Self::Postgres {
                pool,
                admin,
                schema,
            } => {
                pool.close().await;
                sqlx::raw_sql(AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
                    .execute(admin)
                    .await
                    .unwrap();
                admin.close().await;
            }
        }
    }
}
