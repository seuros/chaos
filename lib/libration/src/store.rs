//! Database-backed store for ration snapshots.

use chaos_ration::Freshness;
use chaos_ration::UsageWindow;
use chaos_storage::ChaosStorageProvider;
use sqlx::PgPool;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;

/// A row from `ration_usage` paired with the caller's freshness verdict.
#[derive(Debug, Clone)]
pub struct LatestWindow {
    pub provider: String,
    pub window: UsageWindow,
    pub freshness: Freshness,
}

/// Persistence for rate-limit snapshots. Upserts the latest reading per
/// (provider, label) into `ration_usage` and appends every snapshot to
/// `ration_history` — history is never pruned.
#[derive(Clone)]
pub struct UsageStore {
    backend: Backend,
}

#[derive(Clone)]
enum Backend {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

impl UsageStore {
    /// Build a store from a chaos-storage provider. Returns `None` if the
    /// provider has no usable pool (should not happen once wired from boot,
    /// but we stay defensive so misconfig surfaces as an error, not a panic).
    pub fn from_provider(provider: &ChaosStorageProvider) -> Option<Self> {
        if let Some(pool) = provider.sqlite_pool_cloned() {
            return Some(Self {
                backend: Backend::Sqlite(pool),
            });
        }
        if let Some(pool) = provider.postgres_pool_cloned() {
            return Some(Self {
                backend: Backend::Postgres(pool),
            });
        }
        None
    }

    /// Persist a batch of windows observed from a single response. Appends
    /// every window to `ration_history`, then upserts `ration_usage` so
    /// fresh reads see the latest values.
    pub async fn record(&self, provider: &str, windows: &[UsageWindow]) -> anyhow::Result<()> {
        if windows.is_empty() {
            return Ok(());
        }
        match &self.backend {
            Backend::Sqlite(pool) => record_sqlite(pool, provider, windows).await,
            Backend::Postgres(pool) => record_postgres(pool, provider, windows).await,
        }
    }

    /// Fetch the latest window for every label under `provider`. Each
    /// window is tagged with its freshness relative to `now` (unix seconds),
    /// so callers can distinguish live data, stale-but-valid cache, and
    /// past-reset "budget recovered" states without recomputing the rule.
    pub async fn latest_for(&self, provider: &str, now: i64) -> anyhow::Result<Vec<LatestWindow>> {
        let rows = match &self.backend {
            Backend::Sqlite(pool) => fetch_latest_sqlite(pool, Some(provider)).await?,
            Backend::Postgres(pool) => fetch_latest_postgres(pool, Some(provider)).await?,
        };
        Ok(tag_freshness(rows, now))
    }

    /// Fetch the latest window for every (provider, label) in the store.
    pub async fn latest_all(&self, now: i64) -> anyhow::Result<Vec<LatestWindow>> {
        let rows = match &self.backend {
            Backend::Sqlite(pool) => fetch_latest_sqlite(pool, None).await?,
            Backend::Postgres(pool) => fetch_latest_postgres(pool, None).await?,
        };
        Ok(tag_freshness(rows, now))
    }
}

fn tag_freshness(rows: Vec<(String, UsageWindow)>, now: i64) -> Vec<LatestWindow> {
    rows.into_iter()
        .map(|(provider, window)| {
            let freshness = window.freshness(now);
            LatestWindow {
                provider,
                window,
                freshness,
            }
        })
        .collect()
}

async fn record_sqlite(
    pool: &SqlitePool,
    provider: &str,
    windows: &[UsageWindow],
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    for w in windows {
        let limit = w.limit.map(|v| v as i64);
        let remaining = w.remaining.map(|v| v as i64);

        sqlx::query(
            "INSERT INTO ration_history \
             (provider, label, limit_value, remaining, utilization, resets_at, observed_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(provider)
        .bind(&w.label)
        .bind(limit)
        .bind(remaining)
        .bind(w.utilization)
        .bind(w.resets_at)
        .bind(w.observed_at)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO ration_usage \
             (provider, label, limit_value, remaining, utilization, resets_at, observed_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, UNIXEPOCH()) \
             ON CONFLICT(provider, label) DO UPDATE SET \
                limit_value = excluded.limit_value, \
                remaining = excluded.remaining, \
                utilization = excluded.utilization, \
                resets_at = excluded.resets_at, \
                observed_at = excluded.observed_at, \
                updated_at = UNIXEPOCH()",
        )
        .bind(provider)
        .bind(&w.label)
        .bind(limit)
        .bind(remaining)
        .bind(w.utilization)
        .bind(w.resets_at)
        .bind(w.observed_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn record_postgres(
    pool: &PgPool,
    provider: &str,
    windows: &[UsageWindow],
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    for w in windows {
        let limit = w.limit.map(|v| v as i64);
        let remaining = w.remaining.map(|v| v as i64);

        sqlx::query(
            "INSERT INTO ration_history \
             (provider, label, limit_value, remaining, utilization, resets_at, observed_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(provider)
        .bind(&w.label)
        .bind(limit)
        .bind(remaining)
        .bind(w.utilization)
        .bind(w.resets_at)
        .bind(w.observed_at)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO ration_usage \
             (provider, label, limit_value, remaining, utilization, resets_at, observed_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, EXTRACT(EPOCH FROM clock_timestamp())::BIGINT) \
             ON CONFLICT (provider, label) DO UPDATE SET \
                limit_value = EXCLUDED.limit_value, \
                remaining = EXCLUDED.remaining, \
                utilization = EXCLUDED.utilization, \
                resets_at = EXCLUDED.resets_at, \
                observed_at = EXCLUDED.observed_at, \
                updated_at = EXTRACT(EPOCH FROM clock_timestamp())::BIGINT",
        )
        .bind(provider)
        .bind(&w.label)
        .bind(limit)
        .bind(remaining)
        .bind(w.utilization)
        .bind(w.resets_at)
        .bind(w.observed_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn fetch_latest_sqlite(
    pool: &SqlitePool,
    provider: Option<&str>,
) -> anyhow::Result<Vec<(String, UsageWindow)>> {
    let rows = if let Some(p) = provider {
        sqlx::query(
            "SELECT provider, label, limit_value, remaining, utilization, resets_at, observed_at \
             FROM ration_usage WHERE provider = ? ORDER BY label ASC",
        )
        .bind(p)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            "SELECT provider, label, limit_value, remaining, utilization, resets_at, observed_at \
             FROM ration_usage ORDER BY provider ASC, label ASC",
        )
        .fetch_all(pool)
        .await?
    };
    Ok(rows.into_iter().map(sqlite_row_to_window).collect())
}

async fn fetch_latest_postgres(
    pool: &PgPool,
    provider: Option<&str>,
) -> anyhow::Result<Vec<(String, UsageWindow)>> {
    let rows = if let Some(p) = provider {
        sqlx::query(
            "SELECT provider, label, limit_value, remaining, utilization, resets_at, observed_at \
             FROM ration_usage WHERE provider = $1 ORDER BY label ASC",
        )
        .bind(p)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            "SELECT provider, label, limit_value, remaining, utilization, resets_at, observed_at \
             FROM ration_usage ORDER BY provider ASC, label ASC",
        )
        .fetch_all(pool)
        .await?
    };
    Ok(rows.into_iter().map(postgres_row_to_window).collect())
}

fn sqlite_row_to_window(row: SqliteRow) -> (String, UsageWindow) {
    let provider: String = row.get("provider");
    let limit: Option<i64> = row.get("limit_value");
    let remaining: Option<i64> = row.get("remaining");
    let window = UsageWindow {
        label: row.get("label"),
        limit: limit.map(|v| v as u64),
        remaining: remaining.map(|v| v as u64),
        utilization: row.get("utilization"),
        resets_at: row.get("resets_at"),
        observed_at: row.get("observed_at"),
    };
    (provider, window)
}

fn postgres_row_to_window(row: PgRow) -> (String, UsageWindow) {
    let provider: String = row.get("provider");
    let limit: Option<i64> = row.get("limit_value");
    let remaining: Option<i64> = row.get("remaining");
    let window = UsageWindow {
        label: row.get("label"),
        limit: limit.map(|v| v as u64),
        remaining: remaining.map(|v| v as u64),
        utilization: row.get("utilization"),
        resets_at: row.get("resets_at"),
        observed_at: row.get("observed_at"),
    };
    (provider, window)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn open_sqlite_store() -> (tempfile::TempDir, UsageStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = chaos_proc::open_runtime_db(dir.path())
            .await
            .expect("open runtime db");
        let provider = ChaosStorageProvider::from_sqlite_pool(pool);
        let store = UsageStore::from_provider(&provider).expect("store");
        (dir, store)
    }

    #[tokio::test]
    async fn record_upsert_and_freshness_progression() {
        let (_dir, store) = open_sqlite_store().await;

        // First snapshot: 40k limit, 34k remaining at t=1000 — "85% left".
        let first = UsageWindow::from_raw("tokens", 40_000, 34_000, Some(2_000), 1_000);
        store.record("xai", &[first]).await.expect("record first");

        // Latest reflects the snapshot; observed within 60s of now=1030 → Live.
        let live = store.latest_for("xai", 1_030).await.expect("latest");
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].freshness, Freshness::Live);
        assert_eq!(live[0].window.remaining_percent(), 85);

        // Past the observation minute but before reset → Cached.
        let cached = store.latest_for("xai", 1_500).await.expect("latest");
        assert_eq!(cached[0].freshness, Freshness::Cached);

        // Past resets_at → Reset: budget should be considered recovered.
        let reset = store.latest_for("xai", 2_000).await.expect("latest");
        assert_eq!(reset[0].freshness, Freshness::Reset);

        // Second snapshot upserts: 40k/10k at t=3000 — "25% left".
        let second = UsageWindow::from_raw("tokens", 40_000, 10_000, Some(4_000), 3_000);
        store.record("xai", &[second]).await.expect("record second");

        let after = store.latest_for("xai", 3_005).await.expect("latest");
        assert_eq!(after.len(), 1, "upsert keeps one row per (provider,label)");
        assert_eq!(after[0].window.remaining_percent(), 25);
        assert_eq!(after[0].freshness, Freshness::Live);

        // latest_all still works across providers.
        let all = store.latest_all(3_005).await.expect("latest_all");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].provider, "xai");
    }
}
