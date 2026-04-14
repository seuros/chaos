//! Database-backed store for ration snapshots.

use chaos_ration::Freshness;
use chaos_ration::UsageWindow;
use chaos_storage::ChaosStorageProvider;
use sqlx::PgPool;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Capacity of the background-writer queue. A DB outage won't stall the
/// HTTP hot path or spawn an unbounded fleet of tasks: once this many
/// snapshots are pending, new ones are dropped with a warning and the
/// sniffer carries on.
const WRITER_QUEUE_CAPACITY: usize = 256;

/// One pending write's payload, shipped over the mpsc channel to the
/// single consumer task that owns the database pool.
struct WriteJob {
    provider: String,
    base_url: String,
    windows: Vec<UsageWindow>,
}

/// A row from `ration_usage` paired with the caller's freshness verdict.
///
/// `base_url` is part of the identity: two configs that share a provider
/// tag but point at different endpoints (multi-account, proxies, staging
/// mirrors) keep independent snapshots so their budgets don't stomp.
#[derive(Debug, Clone)]
pub struct LatestWindow {
    pub provider: String,
    pub base_url: String,
    pub window: UsageWindow,
    pub freshness: Freshness,
}

/// Persistence for rate-limit snapshots. Upserts the latest reading per
/// (provider, base_url, label) into `ration_usage` and appends every
/// snapshot to `ration_history` — history is never pruned.
///
/// Writes from the HTTP hot path flow through a bounded mpsc channel
/// into a single long-lived consumer task that owns the database pool.
/// That shape turns what used to be one-spawn-per-response into a
/// serialized queue: a DB outage fills the channel, and new snapshots
/// are dropped with a warning instead of stacking up unbounded tasks.
pub struct UsageStore {
    backend: Backend,
    writer: mpsc::Sender<WriteJob>,
}

#[derive(Clone)]
enum Backend {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

impl UsageStore {
    /// Build a store from a chaos-storage provider and spawn the
    /// background writer. Returns `None` if the provider has no usable
    /// pool (should not happen once wired from boot, but we stay
    /// defensive so misconfig surfaces as an error, not a panic).
    ///
    /// Must be called from within a tokio runtime — the returned store
    /// drives its writer on the current runtime.
    pub fn from_provider(provider: &ChaosStorageProvider) -> Option<Arc<Self>> {
        let backend = if let Some(pool) = provider.sqlite_pool_cloned() {
            Backend::Sqlite(pool)
        } else if let Some(pool) = provider.postgres_pool_cloned() {
            Backend::Postgres(pool)
        } else {
            return None;
        };

        let (tx, rx) = mpsc::channel(WRITER_QUEUE_CAPACITY);
        let store = Arc::new(Self {
            backend: backend.clone(),
            writer: tx,
        });
        // Hand the writer its own clone of the backend rather than the
        // whole store: otherwise the task would hold the last reference
        // to the mpsc::Sender, the channel would never close, and the
        // loop would never terminate when the public `Arc<UsageStore>`
        // finally drops.
        tokio::spawn(async move { run_writer(backend, rx).await });
        Some(store)
    }

    /// Enqueue a batch of windows for asynchronous persistence. Returns
    /// immediately: the actual write happens on the consumer task. If
    /// the writer queue is full (sustained DB outage) the snapshot is
    /// dropped with a warning so the caller never stalls.
    pub fn enqueue(&self, provider: &str, base_url: &str, windows: Vec<UsageWindow>) {
        if windows.is_empty() {
            return;
        }
        let job = WriteJob {
            provider: provider.to_string(),
            base_url: base_url.to_string(),
            windows,
        };
        match self.writer.try_send(job) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(job)) => {
                tracing::warn!(
                    target: "ration",
                    provider = %job.provider,
                    base_url = %job.base_url,
                    "ration writer queue full; dropping usage snapshot"
                );
            }
            Err(mpsc::error::TrySendError::Closed(job)) => {
                tracing::warn!(
                    target: "ration",
                    provider = %job.provider,
                    base_url = %job.base_url,
                    "ration writer task is gone; dropping usage snapshot"
                );
            }
        }
    }

    /// Persist a batch of windows observed from a single response. Appends
    /// every window to `ration_history`, then upserts `ration_usage` so
    /// fresh reads see the latest values. `base_url` identifies the
    /// concrete endpoint the response came from so snapshots from
    /// different configs keyed under the same provider don't collide.
    pub async fn record(
        &self,
        provider: &str,
        base_url: &str,
        windows: &[UsageWindow],
    ) -> anyhow::Result<()> {
        if windows.is_empty() {
            return Ok(());
        }
        match &self.backend {
            Backend::Sqlite(pool) => record_sqlite(pool, provider, base_url, windows).await,
            Backend::Postgres(pool) => record_postgres(pool, provider, base_url, windows).await,
        }
    }

    /// Fetch the latest window for every (base_url, label) under
    /// `provider`. Each window is tagged with its freshness relative to
    /// `now` (unix seconds), so callers can distinguish live data,
    /// stale-but-valid cache, and past-reset "budget recovered" states
    /// without recomputing the rule.
    pub async fn latest_for(&self, provider: &str, now: i64) -> anyhow::Result<Vec<LatestWindow>> {
        let rows = match &self.backend {
            Backend::Sqlite(pool) => fetch_latest_sqlite(pool, Some(provider)).await?,
            Backend::Postgres(pool) => fetch_latest_postgres(pool, Some(provider)).await?,
        };
        Ok(tag_freshness(rows, now))
    }

    /// Fetch the latest window for every (provider, base_url, label) in
    /// the store.
    pub async fn latest_all(&self, now: i64) -> anyhow::Result<Vec<LatestWindow>> {
        let rows = match &self.backend {
            Backend::Sqlite(pool) => fetch_latest_sqlite(pool, None).await?,
            Backend::Postgres(pool) => fetch_latest_postgres(pool, None).await?,
        };
        Ok(tag_freshness(rows, now))
    }
}

/// Drain the mpsc channel, persisting each batch as it arrives. Runs
/// for the lifetime of the store — the loop exits when the last sender
/// is dropped (which happens when the `Arc<UsageStore>` goes away).
async fn run_writer(backend: Backend, mut rx: mpsc::Receiver<WriteJob>) {
    while let Some(job) = rx.recv().await {
        let result = match &backend {
            Backend::Sqlite(pool) => {
                record_sqlite(pool, &job.provider, &job.base_url, &job.windows).await
            }
            Backend::Postgres(pool) => {
                record_postgres(pool, &job.provider, &job.base_url, &job.windows).await
            }
        };
        if let Err(err) = result {
            tracing::warn!(
                target: "ration",
                provider = %job.provider,
                base_url = %job.base_url,
                %err,
                "failed to record usage snapshot"
            );
        }
    }
}

fn tag_freshness(rows: Vec<(String, String, UsageWindow)>, now: i64) -> Vec<LatestWindow> {
    rows.into_iter()
        .map(|(provider, base_url, window)| {
            let freshness = window.freshness(now);
            LatestWindow {
                provider,
                base_url,
                window,
                freshness,
            }
        })
        .collect()
}

async fn record_sqlite(
    pool: &SqlitePool,
    provider: &str,
    base_url: &str,
    windows: &[UsageWindow],
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    for w in windows {
        let limit = w.limit.map(|v| v as i64);
        let remaining = w.remaining.map(|v| v as i64);

        sqlx::query(
            "INSERT INTO ration_history \
             (provider, base_url, label, limit_value, remaining, utilization, resets_at, observed_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(provider)
        .bind(base_url)
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
             (provider, base_url, label, limit_value, remaining, utilization, resets_at, observed_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, UNIXEPOCH()) \
             ON CONFLICT(provider, base_url, label) DO UPDATE SET \
                limit_value = excluded.limit_value, \
                remaining = excluded.remaining, \
                utilization = excluded.utilization, \
                resets_at = excluded.resets_at, \
                observed_at = excluded.observed_at, \
                updated_at = UNIXEPOCH()",
        )
        .bind(provider)
        .bind(base_url)
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
    base_url: &str,
    windows: &[UsageWindow],
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    for w in windows {
        let limit = w.limit.map(|v| v as i64);
        let remaining = w.remaining.map(|v| v as i64);

        sqlx::query(
            "INSERT INTO ration_history \
             (provider, base_url, label, limit_value, remaining, utilization, resets_at, observed_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(provider)
        .bind(base_url)
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
             (provider, base_url, label, limit_value, remaining, utilization, resets_at, observed_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, EXTRACT(EPOCH FROM clock_timestamp())::BIGINT) \
             ON CONFLICT (provider, base_url, label) DO UPDATE SET \
                limit_value = EXCLUDED.limit_value, \
                remaining = EXCLUDED.remaining, \
                utilization = EXCLUDED.utilization, \
                resets_at = EXCLUDED.resets_at, \
                observed_at = EXCLUDED.observed_at, \
                updated_at = EXTRACT(EPOCH FROM clock_timestamp())::BIGINT",
        )
        .bind(provider)
        .bind(base_url)
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
) -> anyhow::Result<Vec<(String, String, UsageWindow)>> {
    let rows = if let Some(p) = provider {
        sqlx::query(
            "SELECT provider, base_url, label, limit_value, remaining, utilization, resets_at, observed_at \
             FROM ration_usage WHERE provider = ? ORDER BY base_url ASC, label ASC",
        )
        .bind(p)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            "SELECT provider, base_url, label, limit_value, remaining, utilization, resets_at, observed_at \
             FROM ration_usage ORDER BY provider ASC, base_url ASC, label ASC",
        )
        .fetch_all(pool)
        .await?
    };
    Ok(rows.into_iter().map(sqlite_row_to_window).collect())
}

async fn fetch_latest_postgres(
    pool: &PgPool,
    provider: Option<&str>,
) -> anyhow::Result<Vec<(String, String, UsageWindow)>> {
    let rows = if let Some(p) = provider {
        sqlx::query(
            "SELECT provider, base_url, label, limit_value, remaining, utilization, resets_at, observed_at \
             FROM ration_usage WHERE provider = $1 ORDER BY base_url ASC, label ASC",
        )
        .bind(p)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            "SELECT provider, base_url, label, limit_value, remaining, utilization, resets_at, observed_at \
             FROM ration_usage ORDER BY provider ASC, base_url ASC, label ASC",
        )
        .fetch_all(pool)
        .await?
    };
    Ok(rows.into_iter().map(postgres_row_to_window).collect())
}

fn sqlite_row_to_window(row: SqliteRow) -> (String, String, UsageWindow) {
    let provider: String = row.get("provider");
    let base_url: String = row.get("base_url");
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
    (provider, base_url, window)
}

fn postgres_row_to_window(row: PgRow) -> (String, String, UsageWindow) {
    let provider: String = row.get("provider");
    let base_url: String = row.get("base_url");
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
    (provider, base_url, window)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn open_sqlite_store() -> (tempfile::TempDir, Arc<UsageStore>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = chaos_proc::open_runtime_db(dir.path())
            .await
            .expect("open runtime db");
        let provider = ChaosStorageProvider::from_sqlite_pool(pool);
        let store = UsageStore::from_provider(&provider).expect("store");
        (dir, store)
    }

    #[tokio::test]
    async fn record_upsert_freshness_and_base_url_isolation() {
        let (_dir, store) = open_sqlite_store().await;

        // First snapshot for xai under its canonical base_url: 40k/34k at
        // t=1000 — "85% left".
        let first = UsageWindow::from_raw("tokens", 40_000, 34_000, Some(2_000), 1_000);
        store
            .record("xai", "https://api.x.ai/v1", &[first])
            .await
            .expect("record first");

        // Latest reflects the snapshot; observed within 60s of now=1030 → Live.
        let live = store.latest_for("xai", 1_030).await.expect("latest");
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].freshness, Freshness::Live);
        assert_eq!(live[0].window.remaining_percent(), 85);
        assert_eq!(live[0].base_url, "https://api.x.ai/v1");

        // Past the observation minute but before reset → Cached.
        let cached = store.latest_for("xai", 1_500).await.expect("latest");
        assert_eq!(cached[0].freshness, Freshness::Cached);

        // Past resets_at → Reset: budget should be considered recovered.
        let reset = store.latest_for("xai", 2_000).await.expect("latest");
        assert_eq!(reset[0].freshness, Freshness::Reset);

        // Second snapshot under the same (provider, base_url) upserts.
        let second = UsageWindow::from_raw("tokens", 40_000, 10_000, Some(4_000), 3_000);
        store
            .record("xai", "https://api.x.ai/v1", &[second])
            .await
            .expect("record second");

        let after = store.latest_for("xai", 3_005).await.expect("latest");
        assert_eq!(
            after.len(),
            1,
            "upsert keeps one row per (provider, base_url, label)"
        );
        assert_eq!(after[0].window.remaining_percent(), 25);
        assert_eq!(after[0].freshness, Freshness::Live);

        // A second config for the same provider under a different
        // base_url must coexist, not stomp.
        let alt = UsageWindow::from_raw("tokens", 40_000, 20_000, Some(5_000), 3_000);
        store
            .record("xai", "https://proxy.internal/xai", &[alt])
            .await
            .expect("record alt");

        let both = store.latest_for("xai", 3_005).await.expect("latest");
        assert_eq!(both.len(), 2, "distinct base_urls keep distinct rows");
        let percents: Vec<u8> = both.iter().map(|w| w.window.remaining_percent()).collect();
        assert!(percents.contains(&25));
        assert!(percents.contains(&50));
    }
}
