use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use chaos_ipc::ProcessId;
use chaos_storage::ChaosStorageProvider;
use sqlx::PgPool;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use sqlx::postgres::Postgres;

async fn open_runtime_storage(chaos_home: &Path) -> std::io::Result<Option<ChaosStorageProvider>> {
    if let Ok(provider) = ChaosStorageProvider::from_env(None).await {
        return Ok(Some(provider));
    }

    let db_path = chaos_proc::runtime_db_path(chaos_home);
    if !tokio::fs::try_exists(&db_path).await? {
        return Ok(None);
    }

    Ok(Some(
        ChaosStorageProvider::from_optional_sqlite(None, Some(chaos_home))
            .await
            .map_err(std::io::Error::other)?,
    ))
}

trait ProcessNameStore {
    async fn set_process_name(
        &self,
        process_id: ProcessId,
        process_name: Option<&str>,
    ) -> std::io::Result<bool>;

    async fn get_process_name(&self, process_id: ProcessId) -> std::io::Result<Option<String>>;

    async fn get_process_names(
        &self,
        process_ids: &HashSet<ProcessId>,
    ) -> std::io::Result<HashMap<ProcessId, String>>;

    async fn find_process_id_by_name(&self, name: &str) -> std::io::Result<Option<ProcessId>>;
}

impl ProcessNameStore for SqlitePool {
    async fn set_process_name(
        &self,
        process_id: ProcessId,
        process_name: Option<&str>,
    ) -> std::io::Result<bool> {
        let result =
            sqlx::query("UPDATE processes SET process_name = ?, updated_at = ? WHERE id = ?")
                .bind(process_name)
                .bind(jiff::Timestamp::now().as_second())
                .bind(process_id.to_string())
                .execute(self)
                .await
                .map_err(std::io::Error::other)?;
        Ok(result.rows_affected() > 0)
    }

    async fn get_process_name(&self, process_id: ProcessId) -> std::io::Result<Option<String>> {
        let row = sqlx::query(
            "SELECT process_name FROM processes WHERE id = ? AND process_name IS NOT NULL AND trim(process_name) <> ''",
        )
        .bind(process_id.to_string())
        .fetch_optional(self)
        .await
        .map_err(std::io::Error::other)?;
        Ok(row.and_then(|row| row.try_get("process_name").ok()))
    }

    async fn get_process_names(
        &self,
        process_ids: &HashSet<ProcessId>,
    ) -> std::io::Result<HashMap<ProcessId, String>> {
        if process_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, process_name FROM processes WHERE process_name IS NOT NULL AND trim(process_name) <> '' AND id IN (",
        );
        {
            let mut separated = builder.separated(", ");
            for process_id in process_ids {
                separated.push_bind(process_id.to_string());
            }
            separated.push_unseparated(")");
        }

        let rows = builder
            .build()
            .fetch_all(self)
            .await
            .map_err(std::io::Error::other)?;
        let mut out = HashMap::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get("id").map_err(std::io::Error::other)?;
            let process_name: String =
                row.try_get("process_name").map_err(std::io::Error::other)?;
            out.insert(
                ProcessId::try_from(id).map_err(std::io::Error::other)?,
                process_name,
            );
        }
        Ok(out)
    }

    async fn find_process_id_by_name(&self, name: &str) -> std::io::Result<Option<ProcessId>> {
        if name.trim().is_empty() {
            return Ok(None);
        }

        let row = sqlx::query(
            "SELECT id FROM processes WHERE process_name = ? ORDER BY updated_at DESC, created_at DESC LIMIT 1",
        )
        .bind(name)
        .fetch_optional(self)
        .await
        .map_err(std::io::Error::other)?;
        row.map(|row| row.try_get::<String, _>("id"))
            .transpose()
            .map_err(std::io::Error::other)?
            .map(ProcessId::try_from)
            .transpose()
            .map_err(std::io::Error::other)
    }
}

impl ProcessNameStore for PgPool {
    async fn set_process_name(
        &self,
        process_id: ProcessId,
        process_name: Option<&str>,
    ) -> std::io::Result<bool> {
        let result =
            sqlx::query("UPDATE processes SET process_name = $1, updated_at = $2 WHERE id = $3")
                .bind(process_name)
                .bind(jiff::Timestamp::now().as_second())
                .bind(process_id.to_string())
                .execute(self)
                .await
                .map_err(std::io::Error::other)?;
        Ok(result.rows_affected() > 0)
    }

    async fn get_process_name(&self, process_id: ProcessId) -> std::io::Result<Option<String>> {
        let row = sqlx::query(
            "SELECT process_name FROM processes WHERE id = $1 AND process_name IS NOT NULL AND btrim(process_name) <> ''",
        )
        .bind(process_id.to_string())
        .fetch_optional(self)
        .await
        .map_err(std::io::Error::other)?;
        Ok(row.and_then(|row| row.try_get("process_name").ok()))
    }

    async fn get_process_names(
        &self,
        process_ids: &HashSet<ProcessId>,
    ) -> std::io::Result<HashMap<ProcessId, String>> {
        if process_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id, process_name FROM processes WHERE process_name IS NOT NULL AND btrim(process_name) <> '' AND id IN (",
        );
        {
            let mut separated = builder.separated(", ");
            for process_id in process_ids {
                separated.push_bind(process_id.to_string());
            }
            separated.push_unseparated(")");
        }

        let rows = builder
            .build()
            .fetch_all(self)
            .await
            .map_err(std::io::Error::other)?;
        let mut out = HashMap::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get("id").map_err(std::io::Error::other)?;
            let process_name: String =
                row.try_get("process_name").map_err(std::io::Error::other)?;
            out.insert(
                ProcessId::try_from(id).map_err(std::io::Error::other)?,
                process_name,
            );
        }
        Ok(out)
    }

    async fn find_process_id_by_name(&self, name: &str) -> std::io::Result<Option<ProcessId>> {
        if name.trim().is_empty() {
            return Ok(None);
        }

        let row = sqlx::query(
            "SELECT id FROM processes WHERE process_name = $1 ORDER BY updated_at DESC, created_at DESC LIMIT 1",
        )
        .bind(name)
        .fetch_optional(self)
        .await
        .map_err(std::io::Error::other)?;
        row.map(|row| row.try_get::<String, _>("id"))
            .transpose()
            .map_err(std::io::Error::other)?
            .map(ProcessId::try_from)
            .transpose()
            .map_err(std::io::Error::other)
    }
}

/// Calls `op` with the pool selected from `provider`, or returns `default` if
/// no pool is available.
async fn with_store<T, F, Fut>(
    provider: &ChaosStorageProvider,
    default: T,
    op: F,
) -> std::io::Result<T>
where
    F: FnOnce(PoolRef) -> Fut,
    Fut: Future<Output = std::io::Result<T>>,
{
    if let Some(pool) = provider.sqlite_pool_cloned() {
        op(PoolRef::Sqlite(pool)).await
    } else if let Some(pool) = provider.postgres_pool_cloned() {
        op(PoolRef::Postgres(pool)).await
    } else {
        Ok(default)
    }
}

enum PoolRef {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

/// Persist the explicit process name in the active runtime store.
pub async fn append_process_name(
    chaos_home: &Path,
    process_id: ProcessId,
    name: &str,
) -> std::io::Result<()> {
    let Some(provider) = open_runtime_storage(chaos_home).await? else {
        return Err(std::io::Error::other(
            "runtime db is unavailable; cannot persist process name",
        ));
    };
    let updated = with_store(&provider, false, |pool| async move {
        match pool {
            PoolRef::Sqlite(p) => p.set_process_name(process_id, Some(name)).await,
            PoolRef::Postgres(p) => p.set_process_name(process_id, Some(name)).await,
        }
    })
    .await?;
    if updated {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "process not found while persisting process name",
        ))
    }
}

/// Find the explicit process name for a process id, if any.
pub async fn find_process_name_by_id(
    chaos_home: &Path,
    process_id: &ProcessId,
) -> std::io::Result<Option<String>> {
    let Some(provider) = open_runtime_storage(chaos_home).await? else {
        return Ok(None);
    };
    with_store(&provider, None, |pool| async move {
        match pool {
            PoolRef::Sqlite(p) => p.get_process_name(*process_id).await,
            PoolRef::Postgres(p) => p.get_process_name(*process_id).await,
        }
    })
    .await
}

/// Find explicit process names for a batch of process ids.
pub async fn find_process_names_by_ids(
    chaos_home: &Path,
    process_ids: &HashSet<ProcessId>,
) -> std::io::Result<HashMap<ProcessId, String>> {
    let Some(provider) = open_runtime_storage(chaos_home).await? else {
        return Ok(HashMap::new());
    };
    with_store(&provider, HashMap::new(), |pool| async move {
        match pool {
            PoolRef::Sqlite(p) => p.get_process_names(process_ids).await,
            PoolRef::Postgres(p) => p.get_process_names(process_ids).await,
        }
    })
    .await
}

/// Find the most recently updated process id for a process name, if any.
pub async fn find_process_id_by_name(
    chaos_home: &Path,
    name: &str,
) -> std::io::Result<Option<ProcessId>> {
    let Some(provider) = open_runtime_storage(chaos_home).await? else {
        return Ok(None);
    };
    with_store(&provider, None, |pool| async move {
        match pool {
            PoolRef::Sqlite(p) => p.find_process_id_by_name(name).await,
            PoolRef::Postgres(p) => p.find_process_id_by_name(name).await,
        }
    })
    .await
}
