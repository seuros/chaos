use std::collections::HashMap;
use std::collections::HashSet;

use chaos_ipc::ProcessId;
use chaos_vfs::Vfs;
use sqlx::FromRow;
use sqlx::PgPool;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use sqlx::postgres::Postgres;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcessNameSource {
    User,
    Agent,
}

impl ProcessNameSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }

    fn from_db(value: Option<&str>) -> Self {
        match value {
            Some("agent") => Self::Agent,
            _ => Self::User,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProcessNameRecord {
    pub name: String,
    pub source: ProcessNameSource,
}

#[derive(FromRow)]
struct ProcessNameRow {
    process_name: String,
    process_name_source: Option<String>,
}

#[derive(FromRow)]
struct NamedProcessRow {
    id: String,
    process_name: String,
}

#[derive(FromRow)]
struct ProcessIdRow {
    id: String,
}

fn decode_process_name<R>(row: Option<R>) -> std::io::Result<Option<ProcessNameRecord>>
where
    R: Row,
    for<'r> ProcessNameRow: FromRow<'r, R>,
{
    row.map(|row| {
        let row = ProcessNameRow::from_row(&row).map_err(std::io::Error::other)?;
        Ok(ProcessNameRecord {
            name: row.process_name,
            source: ProcessNameSource::from_db(row.process_name_source.as_deref()),
        })
    })
    .transpose()
}

fn decode_process_id(id: String) -> std::io::Result<ProcessId> {
    ProcessId::try_from(id).map_err(std::io::Error::other)
}

fn decode_process_names<R>(rows: Vec<R>) -> std::io::Result<HashMap<ProcessId, String>>
where
    R: Row,
    for<'r> NamedProcessRow: FromRow<'r, R>,
{
    let mut out = HashMap::with_capacity(rows.len());
    // Decode and parse each row before proceeding, preserving error precedence.
    for row in rows {
        let row = NamedProcessRow::from_row(&row).map_err(std::io::Error::other)?;
        out.insert(decode_process_id(row.id)?, row.process_name);
    }
    Ok(out)
}

fn decode_optional_process_id<R>(row: Option<R>) -> std::io::Result<Option<ProcessId>>
where
    R: Row,
    for<'r> ProcessIdRow: FromRow<'r, R>,
{
    row.map(|row| {
        let row = ProcessIdRow::from_row(&row).map_err(std::io::Error::other)?;
        decode_process_id(row.id)
    })
    .transpose()
}

trait ProcessNameStore {
    async fn set_process_name(
        &self,
        process_id: ProcessId,
        process_name: Option<&str>,
        source: ProcessNameSource,
    ) -> std::io::Result<bool>;

    async fn get_process_name(
        &self,
        process_id: ProcessId,
    ) -> std::io::Result<Option<ProcessNameRecord>>;

    async fn get_process_names(
        &self,
        process_ids: &HashSet<ProcessId>,
    ) -> std::io::Result<HashMap<ProcessId, String>>;

    async fn find_process_id_by_name(&self, name: &str) -> std::io::Result<Option<ProcessId>>;

    async fn find_unarchived_process_id_by_name(
        &self,
        name: &str,
    ) -> std::io::Result<Option<ProcessId>>;
}

impl ProcessNameStore for SqlitePool {
    async fn set_process_name(
        &self,
        process_id: ProcessId,
        process_name: Option<&str>,
        source: ProcessNameSource,
    ) -> std::io::Result<bool> {
        let result = sqlx::query(
            "UPDATE processes SET process_name = ?, process_name_source = ?, \
             updated_at = ? WHERE id = ?",
        )
        .bind(process_name)
        .bind(source.as_str())
        .bind(jiff::Timestamp::now().as_second())
        .bind(process_id.to_string())
        .execute(self)
        .await
        .map_err(std::io::Error::other)?;
        Ok(result.rows_affected() > 0)
    }

    async fn get_process_name(
        &self,
        process_id: ProcessId,
    ) -> std::io::Result<Option<ProcessNameRecord>> {
        let row = sqlx::query(
            "SELECT process_name, process_name_source FROM processes \
             WHERE id = ? AND process_name IS NOT NULL AND trim(process_name) <> ''",
        )
        .bind(process_id.to_string())
        .fetch_optional(self)
        .await
        .map_err(std::io::Error::other)?;
        decode_process_name(row)
    }

    async fn get_process_names(
        &self,
        process_ids: &HashSet<ProcessId>,
    ) -> std::io::Result<HashMap<ProcessId, String>> {
        if process_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, process_name FROM processes WHERE process_name IS NOT NULL \
             AND trim(process_name) <> '' AND id IN (",
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
        decode_process_names(rows)
    }

    async fn find_process_id_by_name(&self, name: &str) -> std::io::Result<Option<ProcessId>> {
        if name.trim().is_empty() {
            return Ok(None);
        }

        let row = sqlx::query(
            "SELECT id FROM processes WHERE process_name = ? \
             ORDER BY updated_at DESC, created_at DESC LIMIT 1",
        )
        .bind(name)
        .fetch_optional(self)
        .await
        .map_err(std::io::Error::other)?;
        decode_optional_process_id(row)
    }

    async fn find_unarchived_process_id_by_name(
        &self,
        name: &str,
    ) -> std::io::Result<Option<ProcessId>> {
        let row = sqlx::query(
            "SELECT id FROM processes WHERE process_name = ? AND archived_at IS NULL \
             ORDER BY updated_at DESC, created_at DESC LIMIT 1",
        )
        .bind(name)
        .fetch_optional(self)
        .await
        .map_err(std::io::Error::other)?;
        decode_optional_process_id(row)
    }
}

impl ProcessNameStore for PgPool {
    async fn set_process_name(
        &self,
        process_id: ProcessId,
        process_name: Option<&str>,
        source: ProcessNameSource,
    ) -> std::io::Result<bool> {
        let result = sqlx::query(
            "UPDATE processes SET process_name = $1, process_name_source = $2, \
             updated_at = $3 WHERE id = $4",
        )
        .bind(process_name)
        .bind(source.as_str())
        .bind(jiff::Timestamp::now().as_second())
        .bind(process_id.to_string())
        .execute(self)
        .await
        .map_err(std::io::Error::other)?;
        Ok(result.rows_affected() > 0)
    }

    async fn get_process_name(
        &self,
        process_id: ProcessId,
    ) -> std::io::Result<Option<ProcessNameRecord>> {
        let row = sqlx::query(
            "SELECT process_name, process_name_source FROM processes \
             WHERE id = $1 AND process_name IS NOT NULL AND btrim(process_name) <> ''",
        )
        .bind(process_id.to_string())
        .fetch_optional(self)
        .await
        .map_err(std::io::Error::other)?;
        decode_process_name(row)
    }

    async fn get_process_names(
        &self,
        process_ids: &HashSet<ProcessId>,
    ) -> std::io::Result<HashMap<ProcessId, String>> {
        if process_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id, process_name FROM processes WHERE process_name IS NOT NULL \
             AND btrim(process_name) <> '' AND id IN (",
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
        decode_process_names(rows)
    }

    async fn find_process_id_by_name(&self, name: &str) -> std::io::Result<Option<ProcessId>> {
        if name.trim().is_empty() {
            return Ok(None);
        }

        let row = sqlx::query(
            "SELECT id FROM processes WHERE process_name = $1 \
             ORDER BY updated_at DESC, created_at DESC LIMIT 1",
        )
        .bind(name)
        .fetch_optional(self)
        .await
        .map_err(std::io::Error::other)?;
        decode_optional_process_id(row)
    }

    async fn find_unarchived_process_id_by_name(
        &self,
        name: &str,
    ) -> std::io::Result<Option<ProcessId>> {
        let row = sqlx::query(
            "SELECT id FROM processes WHERE process_name = $1 AND archived_at IS NULL \
             ORDER BY updated_at DESC, created_at DESC LIMIT 1",
        )
        .bind(name)
        .fetch_optional(self)
        .await
        .map_err(std::io::Error::other)?;
        decode_optional_process_id(row)
    }
}

/// Calls `op` with the pool of the mounted backend.
async fn with_store<T, F, Fut>(op: F) -> std::io::Result<T>
where
    F: FnOnce(Vfs) -> Fut,
    Fut: Future<Output = std::io::Result<T>>,
{
    let pool = chaos_vfs::pool().map_err(std::io::Error::other)?;
    op(pool).await
}

/// Persist the explicit process name in the active runtime store.
pub async fn append_process_name(process_id: ProcessId, name: &str) -> std::io::Result<()> {
    append_process_name_with_source(process_id, name, ProcessNameSource::User).await
}

pub(crate) async fn append_process_name_with_source(
    process_id: ProcessId,
    name: &str,
    source: ProcessNameSource,
) -> std::io::Result<()> {
    append_name_to_pool(
        chaos_vfs::pool().map_err(std::io::Error::other)?,
        process_id,
        name,
        source,
    )
    .await
}

pub(crate) async fn append_process_name_for_home(
    home: &std::path::Path,
    process_id: ProcessId,
    name: &str,
    source: ProcessNameSource,
) -> std::io::Result<()> {
    append_name_to_pool(
        chaos_vfs::pool_for(home).map_err(std::io::Error::other)?,
        process_id,
        name,
        source,
    )
    .await
}

async fn append_name_to_pool(
    pool: Vfs,
    process_id: ProcessId,
    name: &str,
    source: ProcessNameSource,
) -> std::io::Result<()> {
    let updated = match pool {
        Vfs::Sqlite(p) => p.set_process_name(process_id, Some(name), source).await,
        Vfs::Postgres(p) => p.set_process_name(process_id, Some(name), source).await,
    }?;
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
pub async fn find_process_name_by_id(process_id: &ProcessId) -> std::io::Result<Option<String>> {
    Ok(find_process_name_record_by_id(process_id)
        .await?
        .map(|record| record.name))
}

pub(crate) async fn find_process_name_record_by_id(
    process_id: &ProcessId,
) -> std::io::Result<Option<ProcessNameRecord>> {
    with_store(|pool| async move {
        match pool {
            Vfs::Sqlite(p) => p.get_process_name(*process_id).await,
            Vfs::Postgres(p) => p.get_process_name(*process_id).await,
        }
    })
    .await
}

/// Find explicit process names for a batch of process ids.
pub async fn find_process_names_by_ids(
    process_ids: &HashSet<ProcessId>,
) -> std::io::Result<HashMap<ProcessId, String>> {
    with_store(|pool| async move {
        match pool {
            Vfs::Sqlite(p) => p.get_process_names(process_ids).await,
            Vfs::Postgres(p) => p.get_process_names(process_ids).await,
        }
    })
    .await
}

/// Find the most recently updated process id for a process name, if any.
pub async fn find_process_id_by_name(name: &str) -> std::io::Result<Option<ProcessId>> {
    with_store(|pool| async move {
        match pool {
            Vfs::Sqlite(p) => p.find_process_id_by_name(name).await,
            Vfs::Postgres(p) => p.find_process_id_by_name(name).await,
        }
    })
    .await
}

pub(crate) async fn find_unarchived_process_id_by_name(
    name: &str,
) -> std::io::Result<Option<ProcessId>> {
    with_store(|pool| async move {
        match pool {
            Vfs::Sqlite(p) => p.find_unarchived_process_id_by_name(name).await,
            Vfs::Postgres(p) => p.find_unarchived_process_id_by_name(name).await,
        }
    })
    .await
}

#[cfg(test)]
mod tests;
