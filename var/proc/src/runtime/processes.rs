use super::{
    DynamicToolSpec, ProcessId, ProcessMetadata, ProcessMetadataBuilder, ProcessRow, ProcessesPage,
    QueryBuilder, RolloutItem, Row, SortKey, Sqlite, StateRuntime, Value, anchor_from_item,
    apply_rollout_item, datetime_to_epoch_seconds,
};
use tracing::warn;

fn serialize_process_source_json(source: &str) -> String {
    serde_json::to_string(source).unwrap_or_else(|err| {
        warn!("failed to serialize process source as json string: {err}");
        "\"\"".to_string()
    })
}

impl StateRuntime {
    pub async fn get_process(
        &self,
        id: ProcessId,
    ) -> anyhow::Result<Option<crate::ProcessMetadata>> {
        let row = sqlx::query(
            r#"
SELECT
    id,
    created_at,
    updated_at,
    source,
    agent_nickname,
    agent_role,
    model_provider,
    cwd,
    cli_version,
    title,
    sandbox_policy,
    approval_mode,
    tokens_used,
    first_user_message,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url
FROM processes
WHERE id = ?
            "#,
        )
        .bind(id.to_string())
        .fetch_optional(self.pool.as_ref())
        .await?;
        row.map(|row| ProcessRow::try_from_row(&row).and_then(ProcessMetadata::try_from))
            .transpose()
    }

    pub async fn find_process_ids_by_parent_and_role(
        &self,
        parent_process_id: ProcessId,
        agent_role: &str,
    ) -> anyhow::Result<Vec<ProcessId>> {
        let rows = sqlx::query(
            r#"
SELECT id
FROM processes
WHERE parent_process_id = ?
  AND agent_role = ?
  AND archived_at IS NULL
ORDER BY created_at ASC, id ASC
            "#,
        )
        .bind(parent_process_id.to_string())
        .bind(agent_role)
        .fetch_all(self.pool.as_ref())
        .await?;
        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id")?;
                Ok(ProcessId::try_from(id)?)
            })
            .collect()
    }

    pub async fn get_process_memory_mode(&self, id: ProcessId) -> anyhow::Result<Option<String>> {
        let row = sqlx::query("SELECT memory_mode FROM processes WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(self.pool.as_ref())
            .await?;
        Ok(row.and_then(|row| row.try_get("memory_mode").ok()))
    }

    pub async fn get_process_name(&self, id: ProcessId) -> anyhow::Result<Option<String>> {
        let row = sqlx::query(
            "SELECT process_name FROM processes WHERE id = ? AND process_name IS NOT NULL AND trim(process_name) <> ''",
        )
        .bind(id.to_string())
        .fetch_optional(self.pool.as_ref())
        .await?;
        Ok(row.and_then(|row| row.try_get("process_name").ok()))
    }

    pub async fn get_process_names(
        &self,
        ids: &std::collections::HashSet<ProcessId>,
    ) -> anyhow::Result<std::collections::HashMap<ProcessId, String>> {
        if ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, process_name FROM processes WHERE process_name IS NOT NULL AND trim(process_name) <> '' AND id IN (",
        );
        {
            let mut separated = builder.separated(", ");
            for id in ids {
                separated.push_bind(id.to_string());
            }
        }
        builder.push(")");

        let rows = builder.build().fetch_all(self.pool.as_ref()).await?;
        let mut out = std::collections::HashMap::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get("id")?;
            let process_name: String = row.try_get("process_name")?;
            out.insert(ProcessId::try_from(id)?, process_name);
        }
        Ok(out)
    }

    pub async fn find_process_id_by_name(&self, name: &str) -> anyhow::Result<Option<ProcessId>> {
        if name.trim().is_empty() {
            return Ok(None);
        }
        let row = sqlx::query(
            r#"
SELECT id
FROM processes
WHERE process_name = ?
ORDER BY updated_at DESC, created_at DESC
LIMIT 1
            "#,
        )
        .bind(name)
        .fetch_optional(self.pool.as_ref())
        .await?;
        row.map(|row| row.try_get::<String, _>("id"))
            .transpose()?
            .map(ProcessId::try_from)
            .transpose()
            .map_err(Into::into)
    }

    pub async fn set_process_name(
        &self,
        process_id: ProcessId,
        process_name: Option<&str>,
    ) -> anyhow::Result<bool> {
        let result =
            sqlx::query("UPDATE processes SET process_name = ?, updated_at = ? WHERE id = ?")
                .bind(process_name)
                .bind(datetime_to_epoch_seconds(jiff::Timestamp::now()))
                .bind(process_id.to_string())
                .execute(self.pool.as_ref())
                .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Get dynamic tools for a thread, if present.
    pub async fn get_dynamic_tools(
        &self,
        process_id: ProcessId,
    ) -> anyhow::Result<Option<Vec<DynamicToolSpec>>> {
        let rows = sqlx::query(
            r#"
SELECT name, description, input_schema, defer_loading
FROM process_dynamic_tools
WHERE process_id = ?
ORDER BY position ASC
            "#,
        )
        .bind(process_id.to_string())
        .fetch_all(self.pool.as_ref())
        .await?;
        if rows.is_empty() {
            return Ok(None);
        }
        let mut tools = Vec::with_capacity(rows.len());
        for row in rows {
            let input_schema: String = row.try_get("input_schema")?;
            let input_schema = serde_json::from_str::<Value>(input_schema.as_str())?;
            tools.push(DynamicToolSpec {
                name: row.try_get("name")?,
                description: row.try_get("description")?,
                input_schema,
                defer_loading: row.try_get("defer_loading")?,
            });
        }
        Ok(Some(tools))
    }

    /// List processes using the underlying database.
    #[allow(clippy::too_many_arguments)]
    pub async fn list_processes(
        &self,
        page_size: usize,
        anchor: Option<&crate::Anchor>,
        sort_key: crate::SortKey,
        allowed_sources: &[String],
        model_providers: Option<&[String]>,
        archived_only: bool,
        search_term: Option<&str>,
    ) -> anyhow::Result<crate::ProcessesPage> {
        let limit = page_size.saturating_add(1);

        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"
SELECT
    id,
    created_at,
    updated_at,
    source,
    agent_nickname,
    agent_role,
    model_provider,
    cwd,
    cli_version,
    title,
    sandbox_policy,
    approval_mode,
    tokens_used,
    first_user_message,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url
FROM processes
            "#,
        );
        push_process_filters(
            &mut builder,
            archived_only,
            allowed_sources,
            model_providers,
            anchor,
            sort_key,
            search_term,
        );
        push_process_order_and_limit(&mut builder, sort_key, limit);

        let rows = builder.build().fetch_all(self.pool.as_ref()).await?;
        let mut items = rows
            .into_iter()
            .map(|row| ProcessRow::try_from_row(&row).and_then(ProcessMetadata::try_from))
            .collect::<Result<Vec<_>, _>>()?;
        let num_scanned_rows = items.len();
        let next_anchor = if items.len() > page_size {
            items.pop();
            items
                .last()
                .and_then(|item| anchor_from_item(item, sort_key))
        } else {
            None
        };
        Ok(ProcessesPage {
            items,
            next_anchor,
            num_scanned_rows,
        })
    }

    /// List thread ids using the underlying database (no rollout scanning).
    pub async fn list_process_ids(
        &self,
        limit: usize,
        anchor: Option<&crate::Anchor>,
        sort_key: crate::SortKey,
        allowed_sources: &[String],
        model_providers: Option<&[String]>,
        archived_only: bool,
    ) -> anyhow::Result<Vec<ProcessId>> {
        let mut builder = QueryBuilder::<Sqlite>::new("SELECT id FROM processes");
        push_process_filters(
            &mut builder,
            archived_only,
            allowed_sources,
            model_providers,
            anchor,
            sort_key,
            /*search_term*/ None,
        );
        push_process_order_and_limit(&mut builder, sort_key, limit);

        let rows = builder.build().fetch_all(self.pool.as_ref()).await?;
        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id")?;
                Ok(ProcessId::try_from(id)?)
            })
            .collect()
    }

    /// Insert or replace thread metadata directly.
    pub async fn upsert_process(&self, metadata: &crate::ProcessMetadata) -> anyhow::Result<()> {
        self.upsert_process_with_creation_memory_mode(metadata, /*creation_memory_mode*/ None)
            .await
    }

    pub async fn insert_process_if_absent(
        &self,
        metadata: &crate::ProcessMetadata,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query(
            r#"
INSERT INTO processes (
    id,
    source_json,
    created_at,
    updated_at,
    source,
    agent_nickname,
    agent_role,
    model_provider,
    cwd,
    cli_version,
    title,
    sandbox_policy,
    approval_mode,
    tokens_used,
    first_user_message,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url,
    memory_mode
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(id) DO NOTHING
            "#,
        )
        .bind(metadata.id.to_string())
        .bind(serialize_process_source_json(metadata.source.as_str()))
        .bind(datetime_to_epoch_seconds(metadata.created_at))
        .bind(datetime_to_epoch_seconds(metadata.updated_at))
        .bind(metadata.source.as_str())
        .bind(metadata.agent_nickname.as_deref())
        .bind(metadata.agent_role.as_deref())
        .bind(metadata.model_provider.as_str())
        .bind(metadata.cwd.display().to_string())
        .bind(metadata.cli_version.as_str())
        .bind(metadata.title.as_str())
        .bind(metadata.sandbox_policy.as_str())
        .bind(metadata.approval_mode.as_str())
        .bind(metadata.tokens_used)
        .bind(metadata.first_user_message.as_deref().unwrap_or_default())
        .bind(metadata.archived_at.map(datetime_to_epoch_seconds))
        .bind(metadata.git_sha.as_deref())
        .bind(metadata.git_branch.as_deref())
        .bind(metadata.git_origin_url.as_deref())
        .bind("enabled")
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn set_process_memory_mode(
        &self,
        process_id: ProcessId,
        memory_mode: &str,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query("UPDATE processes SET memory_mode = ? WHERE id = ?")
            .bind(memory_mode)
            .bind(process_id.to_string())
            .execute(self.pool.as_ref())
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn touch_process_updated_at(
        &self,
        process_id: ProcessId,
        updated_at: jiff::Timestamp,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query("UPDATE processes SET updated_at = ? WHERE id = ?")
            .bind(datetime_to_epoch_seconds(updated_at))
            .bind(process_id.to_string())
            .execute(self.pool.as_ref())
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn update_process_git_info(
        &self,
        process_id: ProcessId,
        git_sha: Option<Option<&str>>,
        git_branch: Option<Option<&str>>,
        git_origin_url: Option<Option<&str>>,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query(
            r#"
UPDATE processes
SET
    git_sha = CASE WHEN ? THEN ? ELSE git_sha END,
    git_branch = CASE WHEN ? THEN ? ELSE git_branch END,
    git_origin_url = CASE WHEN ? THEN ? ELSE git_origin_url END
WHERE id = ?
            "#,
        )
        .bind(git_sha.is_some())
        .bind(git_sha.flatten())
        .bind(git_branch.is_some())
        .bind(git_branch.flatten())
        .bind(git_origin_url.is_some())
        .bind(git_origin_url.flatten())
        .bind(process_id.to_string())
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn upsert_process_with_creation_memory_mode(
        &self,
        metadata: &crate::ProcessMetadata,
        creation_memory_mode: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO processes (
    id,
    source_json,
    created_at,
    updated_at,
    source,
    agent_nickname,
    agent_role,
    model_provider,
    cwd,
    cli_version,
    title,
    sandbox_policy,
    approval_mode,
    tokens_used,
    first_user_message,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url,
    memory_mode
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(id) DO UPDATE SET
    source_json = excluded.source_json,
    created_at = excluded.created_at,
    updated_at = excluded.updated_at,
    source = excluded.source,
    agent_nickname = excluded.agent_nickname,
    agent_role = excluded.agent_role,
    model_provider = excluded.model_provider,
    cwd = excluded.cwd,
    cli_version = excluded.cli_version,
    title = excluded.title,
    sandbox_policy = excluded.sandbox_policy,
    approval_mode = excluded.approval_mode,
    tokens_used = excluded.tokens_used,
    first_user_message = excluded.first_user_message,
    archived_at = excluded.archived_at,
    git_sha = excluded.git_sha,
    git_branch = excluded.git_branch,
    git_origin_url = excluded.git_origin_url
            "#,
        )
        .bind(metadata.id.to_string())
        .bind(serialize_process_source_json(metadata.source.as_str()))
        .bind(datetime_to_epoch_seconds(metadata.created_at))
        .bind(datetime_to_epoch_seconds(metadata.updated_at))
        .bind(metadata.source.as_str())
        .bind(metadata.agent_nickname.as_deref())
        .bind(metadata.agent_role.as_deref())
        .bind(metadata.model_provider.as_str())
        .bind(metadata.cwd.display().to_string())
        .bind(metadata.cli_version.as_str())
        .bind(metadata.title.as_str())
        .bind(metadata.sandbox_policy.as_str())
        .bind(metadata.approval_mode.as_str())
        .bind(metadata.tokens_used)
        .bind(metadata.first_user_message.as_deref().unwrap_or_default())
        .bind(metadata.archived_at.map(datetime_to_epoch_seconds))
        .bind(metadata.git_sha.as_deref())
        .bind(metadata.git_branch.as_deref())
        .bind(metadata.git_origin_url.as_deref())
        .bind(creation_memory_mode.unwrap_or("enabled"))
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    /// Persist dynamic tools for a thread if none have been stored yet.
    ///
    /// Dynamic tools are defined at thread start and should not change afterward.
    /// This only writes the first time we see tools for a given thread.
    pub async fn persist_dynamic_tools(
        &self,
        process_id: ProcessId,
        tools: Option<&[DynamicToolSpec]>,
    ) -> anyhow::Result<()> {
        let Some(tools) = tools else {
            return Ok(());
        };
        if tools.is_empty() {
            return Ok(());
        }
        let process_id = process_id.to_string();
        let mut tx = self.pool.begin().await?;
        for (idx, tool) in tools.iter().enumerate() {
            let position = i64::try_from(idx).unwrap_or(i64::MAX);
            let input_schema = serde_json::to_string(&tool.input_schema)?;
            sqlx::query(
                r#"
INSERT INTO process_dynamic_tools (
    process_id,
    position,
    name,
    description,
    input_schema,
    defer_loading
) VALUES (?, ?, ?, ?, ?, ?)
ON CONFLICT(process_id, position) DO NOTHING
                "#,
            )
            .bind(process_id.as_str())
            .bind(position)
            .bind(tool.name.as_str())
            .bind(tool.description.as_str())
            .bind(input_schema)
            .bind(tool.defer_loading)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Apply rollout items incrementally using the underlying database.
    pub async fn apply_rollout_items(
        &self,
        builder: &ProcessMetadataBuilder,
        items: &[RolloutItem],
        new_process_memory_mode: Option<&str>,
        updated_at_override: Option<jiff::Timestamp>,
    ) -> anyhow::Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let existing_metadata = self.get_process(builder.id).await?;
        let mut metadata = existing_metadata
            .clone()
            .unwrap_or_else(|| builder.build(&self.default_provider));
        for item in items {
            apply_rollout_item(&mut metadata, item, &self.default_provider);
        }
        if let Some(existing_metadata) = existing_metadata.as_ref() {
            metadata.prefer_existing_git_info(existing_metadata);
        }
        let updated_at = match updated_at_override {
            Some(updated_at) => Some(updated_at),
            None => Some(jiff::Timestamp::now()),
        };
        if let Some(updated_at) = updated_at {
            metadata.updated_at = updated_at;
        }
        // Keep the thread upsert before dynamic tools to satisfy the foreign key constraint:
        // process_dynamic_tools.process_id -> processes.id.
        let upsert_result = if existing_metadata.is_none() {
            self.upsert_process_with_creation_memory_mode(&metadata, new_process_memory_mode)
                .await
        } else {
            self.upsert_process(&metadata).await
        };
        upsert_result?;
        if let Some(memory_mode) = extract_memory_mode(items)
            && let Err(err) = self
                .set_process_memory_mode(builder.id, memory_mode.as_str())
                .await
        {
            return Err(err);
        }
        let dynamic_tools = extract_dynamic_tools(items);
        if let Some(dynamic_tools) = dynamic_tools
            && let Err(err) = self
                .persist_dynamic_tools(builder.id, dynamic_tools.as_deref())
                .await
        {
            return Err(err);
        }
        Ok(())
    }

    /// Mark a thread as archived using the underlying database.
    pub async fn mark_archived(
        &self,
        process_id: ProcessId,
        archived_at: jiff::Timestamp,
    ) -> anyhow::Result<()> {
        let Some(mut metadata) = self.get_process(process_id).await? else {
            return Ok(());
        };
        metadata.archived_at = Some(archived_at);
        metadata.updated_at = archived_at;
        if metadata.id != process_id {
            warn!(
                "thread id mismatch during archive: expected {process_id}, got {}",
                metadata.id
            );
        }
        self.upsert_process(&metadata).await
    }

    /// Mark a thread as unarchived using the underlying database.
    pub async fn mark_unarchived(&self, process_id: ProcessId) -> anyhow::Result<()> {
        let Some(mut metadata) = self.get_process(process_id).await? else {
            return Ok(());
        };
        metadata.archived_at = None;
        metadata.updated_at = jiff::Timestamp::now();
        if metadata.id != process_id {
            warn!(
                "thread id mismatch during unarchive: expected {process_id}, got {}",
                metadata.id
            );
        }
        self.upsert_process(&metadata).await
    }

    /// Delete a thread metadata row by id.
    pub async fn delete_process(&self, process_id: ProcessId) -> anyhow::Result<u64> {
        let result = sqlx::query("DELETE FROM processes WHERE id = ?")
            .bind(process_id.to_string())
            .execute(self.pool.as_ref())
            .await?;
        Ok(result.rows_affected())
    }
}

pub(super) fn extract_dynamic_tools(items: &[RolloutItem]) -> Option<Option<Vec<DynamicToolSpec>>> {
    items.iter().find_map(|item| match item {
        RolloutItem::SessionMeta(meta_line) => Some(meta_line.meta.dynamic_tools.clone()),
        RolloutItem::ResponseItem(_)
        | RolloutItem::Compacted(_)
        | RolloutItem::BackgroundTask(_)
        | RolloutItem::CompactionControl(_)
        | RolloutItem::TurnContext(_)
        | RolloutItem::EventMsg(_) => None,
    })
}

pub(super) fn extract_memory_mode(items: &[RolloutItem]) -> Option<String> {
    items.iter().rev().find_map(|item| match item {
        RolloutItem::SessionMeta(meta_line) => meta_line.meta.memory_mode.clone(),
        RolloutItem::ResponseItem(_)
        | RolloutItem::Compacted(_)
        | RolloutItem::BackgroundTask(_)
        | RolloutItem::CompactionControl(_)
        | RolloutItem::TurnContext(_)
        | RolloutItem::EventMsg(_) => None,
    })
}

pub(super) fn push_process_filters<'a>(
    builder: &mut QueryBuilder<Sqlite>,
    archived_only: bool,
    allowed_sources: &'a [String],
    model_providers: Option<&'a [String]>,
    anchor: Option<&crate::Anchor>,
    sort_key: SortKey,
    search_term: Option<&'a str>,
) {
    builder.push(" WHERE 1 = 1");
    if archived_only {
        builder.push(" AND archived_at IS NOT NULL");
    } else {
        builder.push(" AND archived_at IS NULL");
    }
    if !allowed_sources.is_empty() {
        builder.push(" AND source IN (");
        let mut separated = builder.separated(", ");
        for source in allowed_sources {
            separated.push_bind(source);
        }
        separated.push_unseparated(")");
    }
    if let Some(model_providers) = model_providers
        && !model_providers.is_empty()
    {
        builder.push(" AND model_provider IN (");
        let mut separated = builder.separated(", ");
        for provider in model_providers {
            separated.push_bind(provider);
        }
        separated.push_unseparated(")");
    }
    if let Some(search_term) = search_term {
        builder.push(" AND instr(title, ");
        builder.push_bind(search_term);
        builder.push(") > 0");
    }
    if let Some(anchor) = anchor {
        let anchor_ts = datetime_to_epoch_seconds(anchor.ts);
        let column = match sort_key {
            SortKey::CreatedAt => "created_at",
            SortKey::UpdatedAt => "updated_at",
        };
        builder.push(" AND (");
        builder.push(column);
        builder.push(" < ");
        builder.push_bind(anchor_ts);
        builder.push(" OR (");
        builder.push(column);
        builder.push(" = ");
        builder.push_bind(anchor_ts);
        builder.push(" AND id < ");
        builder.push_bind(anchor.id.to_string());
        builder.push("))");
    }
}

pub(super) fn push_process_order_and_limit(
    builder: &mut QueryBuilder<Sqlite>,
    sort_key: SortKey,
    limit: usize,
) {
    let order_column = match sort_key {
        SortKey::CreatedAt => "created_at",
        SortKey::UpdatedAt => "updated_at",
    };
    builder.push(" ORDER BY ");
    builder.push(order_column);
    builder.push(" DESC, id DESC");
    builder.push(" LIMIT ");
    builder.push_bind(limit as i64);
}

#[cfg(test)]
mod tests;
