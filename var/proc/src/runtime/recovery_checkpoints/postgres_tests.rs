use super::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn recovery_checkpoints_postgres_contract() -> anyhow::Result<()> {
    let url = std::env::var("TEST_DATABASE_URL")?;
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;
    let schema = format!("recovery_test_{}", Uuid::new_v4().simple());
    // The schema name contains only a fixed prefix and a locally generated UUID.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await?;
    let options = PgConnectOptions::from_str(&url)?.options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await?;
    let result = async {
        crate::migrations::POSTGRES_STATE_MIGRATOR
            .run(&pool)
            .await?;
        let home = tempfile::tempdir()?;
        let db =
            RuntimeDbHandle::from_postgres_pool(home.path().into(), "test".into(), pool.clone());
        let process = super::tests::owner(&db, home.path()).await?;
        super::tests::contract(&db, process).await?;
        let other = super::tests::owner(&db, home.path()).await?;
        super::tests::concurrent_saves(&db, other).await
    }
    .await;
    pool.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await?;
    admin.close().await;
    result
}
