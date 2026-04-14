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

async fn set_process_name_sqlite(
    pool: &SqlitePool,
    process_id: ProcessId,
    process_name: Option<&str>,
) -> std::io::Result<bool> {
    let result = sqlx::query("UPDATE processes SET process_name = ?, updated_at = ? WHERE id = ?")
        .bind(process_name)
        .bind(jiff::Timestamp::now().as_second())
        .bind(process_id.to_string())
        .execute(pool)
        .await
        .map_err(std::io::Error::other)?;
    Ok(result.rows_affected() > 0)
}

async fn set_process_name_postgres(
    pool: &PgPool,
    process_id: ProcessId,
    process_name: Option<&str>,
) -> std::io::Result<bool> {
    let result =
        sqlx::query("UPDATE processes SET process_name = $1, updated_at = $2 WHERE id = $3")
            .bind(process_name)
            .bind(jiff::Timestamp::now().as_second())
            .bind(process_id.to_string())
            .execute(pool)
            .await
            .map_err(std::io::Error::other)?;
    Ok(result.rows_affected() > 0)
}

async fn get_process_name_sqlite(
    pool: &SqlitePool,
    process_id: ProcessId,
) -> std::io::Result<Option<String>> {
    let row = sqlx::query(
        "SELECT process_name FROM processes WHERE id = ? AND process_name IS NOT NULL AND trim(process_name) <> ''",
    )
    .bind(process_id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(std::io::Error::other)?;
    Ok(row.and_then(|row| row.try_get("process_name").ok()))
}

async fn get_process_name_postgres(
    pool: &PgPool,
    process_id: ProcessId,
) -> std::io::Result<Option<String>> {
    let row = sqlx::query(
        "SELECT process_name FROM processes WHERE id = $1 AND process_name IS NOT NULL AND btrim(process_name) <> ''",
    )
    .bind(process_id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(std::io::Error::other)?;
    Ok(row.and_then(|row| row.try_get("process_name").ok()))
}

async fn get_process_names_sqlite(
    pool: &SqlitePool,
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
        .fetch_all(pool)
        .await
        .map_err(std::io::Error::other)?;
    let mut out = HashMap::with_capacity(rows.len());
    for row in rows {
        let id: String = row.try_get("id").map_err(std::io::Error::other)?;
        let process_name: String = row.try_get("process_name").map_err(std::io::Error::other)?;
        out.insert(
            ProcessId::try_from(id).map_err(std::io::Error::other)?,
            process_name,
        );
    }
    Ok(out)
}

async fn get_process_names_postgres(
    pool: &PgPool,
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
        .fetch_all(pool)
        .await
        .map_err(std::io::Error::other)?;
    let mut out = HashMap::with_capacity(rows.len());
    for row in rows {
        let id: String = row.try_get("id").map_err(std::io::Error::other)?;
        let process_name: String = row.try_get("process_name").map_err(std::io::Error::other)?;
        out.insert(
            ProcessId::try_from(id).map_err(std::io::Error::other)?,
            process_name,
        );
    }
    Ok(out)
}

async fn find_process_id_by_name_sqlite(
    pool: &SqlitePool,
    name: &str,
) -> std::io::Result<Option<ProcessId>> {
    if name.trim().is_empty() {
        return Ok(None);
    }

    let row = sqlx::query(
        "SELECT id FROM processes WHERE process_name = ? ORDER BY updated_at DESC, created_at DESC LIMIT 1",
    )
    .bind(name)
    .fetch_optional(pool)
    .await
    .map_err(std::io::Error::other)?;
    row.map(|row| row.try_get::<String, _>("id"))
        .transpose()
        .map_err(std::io::Error::other)?
        .map(ProcessId::try_from)
        .transpose()
        .map_err(std::io::Error::other)
}

async fn find_process_id_by_name_postgres(
    pool: &PgPool,
    name: &str,
) -> std::io::Result<Option<ProcessId>> {
    if name.trim().is_empty() {
        return Ok(None);
    }

    let row = sqlx::query(
        "SELECT id FROM processes WHERE process_name = $1 ORDER BY updated_at DESC, created_at DESC LIMIT 1",
    )
    .bind(name)
    .fetch_optional(pool)
    .await
    .map_err(std::io::Error::other)?;
    row.map(|row| row.try_get::<String, _>("id"))
        .transpose()
        .map_err(std::io::Error::other)?
        .map(ProcessId::try_from)
        .transpose()
        .map_err(std::io::Error::other)
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
    let updated = if let Some(pool) = provider.sqlite_pool_cloned() {
        set_process_name_sqlite(&pool, process_id, Some(name)).await?
    } else if let Some(pool) = provider.postgres_pool_cloned() {
        set_process_name_postgres(&pool, process_id, Some(name)).await?
    } else {
        false
    };
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
    if let Some(pool) = provider.sqlite_pool_cloned() {
        get_process_name_sqlite(&pool, *process_id).await
    } else if let Some(pool) = provider.postgres_pool_cloned() {
        get_process_name_postgres(&pool, *process_id).await
    } else {
        Ok(None)
    }
}

/// Find explicit process names for a batch of process ids.
pub async fn find_process_names_by_ids(
    chaos_home: &Path,
    process_ids: &HashSet<ProcessId>,
) -> std::io::Result<HashMap<ProcessId, String>> {
    let Some(provider) = open_runtime_storage(chaos_home).await? else {
        return Ok(HashMap::new());
    };
    if let Some(pool) = provider.sqlite_pool_cloned() {
        get_process_names_sqlite(&pool, process_ids).await
    } else if let Some(pool) = provider.postgres_pool_cloned() {
        get_process_names_postgres(&pool, process_ids).await
    } else {
        Ok(HashMap::new())
    }
}

/// Find the most recently updated process id for a process name, if any.
pub async fn find_process_id_by_name(
    chaos_home: &Path,
    name: &str,
) -> std::io::Result<Option<ProcessId>> {
    let Some(provider) = open_runtime_storage(chaos_home).await? else {
        return Ok(None);
    };
    if let Some(pool) = provider.sqlite_pool_cloned() {
        find_process_id_by_name_sqlite(&pool, name).await
    } else if let Some(pool) = provider.postgres_pool_cloned() {
        find_process_id_by_name_postgres(&pool, name).await
    } else {
        Ok(None)
    }
}
