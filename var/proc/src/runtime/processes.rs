use super::*;

impl StateRuntime {
    pub async fn get_process(
        &self,
        id: ProcessId,
    ) -> anyhow::Result<Option<crate::ProcessMetadata>> {
        let row = sqlx::query(
            r#"
SELECT
    id,
    rollout_path,
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

    pub async fn get_process_memory_mode(&self, id: ProcessId) -> anyhow::Result<Option<String>> {
        let row = sqlx::query("SELECT memory_mode FROM processes WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(self.pool.as_ref())
            .await?;
        Ok(row.and_then(|row| row.try_get("memory_mode").ok()))
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

    /// Find a rollout path by thread id using the underlying database.
    pub async fn find_rollout_path_by_id(
        &self,
        id: ProcessId,
        archived_only: Option<bool>,
    ) -> anyhow::Result<Option<PathBuf>> {
        let mut builder =
            QueryBuilder::<Sqlite>::new("SELECT rollout_path FROM processes WHERE id = ");
        builder.push_bind(id.to_string());
        match archived_only {
            Some(true) => {
                builder.push(" AND archived = 1");
            }
            Some(false) => {
                builder.push(" AND archived = 0");
            }
            None => {}
        }
        let row = builder.build().fetch_optional(self.pool.as_ref()).await?;
        Ok(row
            .and_then(|r| r.try_get::<String, _>("rollout_path").ok())
            .map(PathBuf::from))
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
    rollout_path,
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
    rollout_path,
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
    archived,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url,
    memory_mode
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(id) DO NOTHING
            "#,
        )
        .bind(metadata.id.to_string())
        .bind(metadata.rollout_path.display().to_string())
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
        .bind(metadata.archived_at.is_some())
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
    rollout_path,
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
    archived,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url,
    memory_mode
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(id) DO UPDATE SET
    rollout_path = excluded.rollout_path,
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
    archived = excluded.archived,
    archived_at = excluded.archived_at,
    git_sha = excluded.git_sha,
    git_branch = excluded.git_branch,
    git_origin_url = excluded.git_origin_url
            "#,
        )
        .bind(metadata.id.to_string())
        .bind(metadata.rollout_path.display().to_string())
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
        .bind(metadata.archived_at.is_some())
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
        metadata.rollout_path = builder.rollout_path.clone();
        for item in items {
            apply_rollout_item(&mut metadata, item, &self.default_provider);
        }
        if let Some(existing_metadata) = existing_metadata.as_ref() {
            metadata.prefer_existing_git_info(existing_metadata);
        }
        let updated_at = match updated_at_override {
            Some(updated_at) => Some(updated_at),
            None => file_modified_time_utc(builder.rollout_path.as_path()).await,
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
        rollout_path: &Path,
        archived_at: jiff::Timestamp,
    ) -> anyhow::Result<()> {
        let Some(mut metadata) = self.get_process(process_id).await? else {
            return Ok(());
        };
        metadata.archived_at = Some(archived_at);
        metadata.rollout_path = rollout_path.to_path_buf();
        if let Some(updated_at) = file_modified_time_utc(rollout_path).await {
            metadata.updated_at = updated_at;
        }
        if metadata.id != process_id {
            warn!(
                "thread id mismatch during archive: expected {process_id}, got {}",
                metadata.id
            );
        }
        self.upsert_process(&metadata).await
    }

    /// Mark a thread as unarchived using the underlying database.
    pub async fn mark_unarchived(
        &self,
        process_id: ProcessId,
        rollout_path: &Path,
    ) -> anyhow::Result<()> {
        let Some(mut metadata) = self.get_process(process_id).await? else {
            return Ok(());
        };
        metadata.archived_at = None;
        metadata.rollout_path = rollout_path.to_path_buf();
        if let Some(updated_at) = file_modified_time_utc(rollout_path).await {
            metadata.updated_at = updated_at;
        }
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
        | RolloutItem::TurnContext(_)
        | RolloutItem::EventMsg(_) => None,
    })
}

pub(super) fn extract_memory_mode(items: &[RolloutItem]) -> Option<String> {
    items.iter().rev().find_map(|item| match item {
        RolloutItem::SessionMeta(meta_line) => meta_line.meta.memory_mode.clone(),
        RolloutItem::ResponseItem(_)
        | RolloutItem::Compacted(_)
        | RolloutItem::TurnContext(_)
        | RolloutItem::EventMsg(_) => None,
    })
}

pub(super) fn push_process_filters<'a>(
    builder: &mut QueryBuilder<'a, Sqlite>,
    archived_only: bool,
    allowed_sources: &'a [String],
    model_providers: Option<&'a [String]>,
    anchor: Option<&crate::Anchor>,
    sort_key: SortKey,
    search_term: Option<&'a str>,
) {
    builder.push(" WHERE 1 = 1");
    if archived_only {
        builder.push(" AND archived = 1");
    } else {
        builder.push(" AND archived = 0");
    }
    builder.push(" AND first_user_message <> ''");
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
    builder: &mut QueryBuilder<'_, Sqlite>,
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
mod tests {
    use super::*;
    use crate::runtime::test_support::test_process_metadata;
    use crate::runtime::test_support::unique_temp_dir;
    use chaos_ipc::protocol::EventMsg;
    use chaos_ipc::protocol::GitInfo;
    use chaos_ipc::protocol::SessionMeta;
    use chaos_ipc::protocol::SessionMetaLine;
    use chaos_ipc::protocol::SessionSource;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    #[tokio::test]
    async fn upsert_thread_keeps_creation_memory_mode_for_existing_rows() {
        let codex_home = unique_temp_dir();
        let runtime = StateRuntime::init(codex_home.clone(), "test-provider".to_string())
            .await
            .expect("state db should initialize");
        let process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000123")
            .expect("valid thread id");
        let mut metadata = test_process_metadata(&codex_home, process_id, codex_home.clone());

        runtime
            .upsert_process_with_creation_memory_mode(&metadata, Some("disabled"))
            .await
            .expect("initial insert should succeed");

        let memory_mode: String =
            sqlx::query_scalar("SELECT memory_mode FROM processes WHERE id = ?")
                .bind(process_id.to_string())
                .fetch_one(runtime.pool.as_ref())
                .await
                .expect("memory mode should be readable");
        assert_eq!(memory_mode, "disabled");

        metadata.title = "updated title".to_string();
        runtime
            .upsert_process(&metadata)
            .await
            .expect("upsert should succeed");

        let memory_mode: String =
            sqlx::query_scalar("SELECT memory_mode FROM processes WHERE id = ?")
                .bind(process_id.to_string())
                .fetch_one(runtime.pool.as_ref())
                .await
                .expect("memory mode should remain readable");
        assert_eq!(memory_mode, "disabled");
    }

    #[tokio::test]
    async fn apply_rollout_items_restores_memory_mode_from_session_meta() {
        let codex_home = unique_temp_dir();
        let runtime = StateRuntime::init(codex_home.clone(), "test-provider".to_string())
            .await
            .expect("state db should initialize");
        let process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000456")
            .expect("valid thread id");
        let metadata = test_process_metadata(&codex_home, process_id, codex_home.clone());

        runtime
            .upsert_process(&metadata)
            .await
            .expect("initial upsert should succeed");

        let builder = ProcessMetadataBuilder::new(
            process_id,
            metadata.rollout_path.clone(),
            metadata.created_at,
            SessionSource::Cli,
        );
        let items = vec![RolloutItem::SessionMeta(SessionMetaLine {
            meta: SessionMeta {
                id: process_id,
                forked_from_id: None,
                timestamp: metadata.created_at.to_string(),
                cwd: PathBuf::new(),
                originator: String::new(),
                cli_version: String::new(),
                source: SessionSource::Cli,
                agent_nickname: None,
                agent_role: None,
                model_provider: None,
                base_instructions: None,
                dynamic_tools: None,
                memory_mode: Some("polluted".to_string()),
            },
            git: None,
        })];

        runtime
            .apply_rollout_items(&builder, &items, None, None)
            .await
            .expect("apply_rollout_items should succeed");

        let memory_mode = runtime
            .get_process_memory_mode(process_id)
            .await
            .expect("memory mode should load");
        assert_eq!(memory_mode.as_deref(), Some("polluted"));
    }

    #[tokio::test]
    async fn apply_rollout_items_preserves_existing_git_branch_and_fills_missing_git_fields() {
        let codex_home = unique_temp_dir();
        let runtime = StateRuntime::init(codex_home.clone(), "test-provider".to_string())
            .await
            .expect("state db should initialize");
        let process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000457")
            .expect("valid thread id");
        let mut metadata = test_process_metadata(&codex_home, process_id, codex_home.clone());
        metadata.git_branch = Some("sqlite-branch".to_string());

        runtime
            .upsert_process(&metadata)
            .await
            .expect("initial upsert should succeed");

        let created_at = metadata.created_at.to_string();
        let builder = ProcessMetadataBuilder::new(
            process_id,
            metadata.rollout_path.clone(),
            metadata.created_at,
            SessionSource::Cli,
        );
        let items = vec![RolloutItem::SessionMeta(SessionMetaLine {
            meta: SessionMeta {
                id: process_id,
                forked_from_id: None,
                timestamp: created_at,
                cwd: PathBuf::new(),
                originator: String::new(),
                cli_version: String::new(),
                source: SessionSource::Cli,
                agent_nickname: None,
                agent_role: None,
                model_provider: None,
                base_instructions: None,
                dynamic_tools: None,
                memory_mode: None,
            },
            git: Some(GitInfo {
                commit_hash: Some("rollout-sha".to_string()),
                branch: Some("rollout-branch".to_string()),
                repository_url: Some("git@example.com:openai/codex.git".to_string()),
            }),
        })];

        runtime
            .apply_rollout_items(&builder, &items, None, None)
            .await
            .expect("apply_rollout_items should succeed");

        let persisted = runtime
            .get_process(process_id)
            .await
            .expect("thread should load")
            .expect("thread should exist");
        assert_eq!(persisted.git_sha.as_deref(), Some("rollout-sha"));
        assert_eq!(persisted.git_branch.as_deref(), Some("sqlite-branch"));
        assert_eq!(
            persisted.git_origin_url.as_deref(),
            Some("git@example.com:openai/codex.git")
        );
    }

    #[tokio::test]
    async fn update_process_git_info_preserves_newer_non_git_metadata() {
        let codex_home = unique_temp_dir();
        let runtime = StateRuntime::init(codex_home.clone(), "test-provider".to_string())
            .await
            .expect("state db should initialize");
        let process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000789")
            .expect("valid thread id");
        let metadata = test_process_metadata(&codex_home, process_id, codex_home.clone());

        runtime
            .upsert_process(&metadata)
            .await
            .expect("initial upsert should succeed");

        let updated_at =
            datetime_to_epoch_seconds(jiff::Timestamp::new(1_700_000_100, 0).expect("timestamp"));
        sqlx::query(
            "UPDATE processes SET updated_at = ?, tokens_used = ?, first_user_message = ? WHERE id = ?",
        )
        .bind(updated_at)
        .bind(123_i64)
        .bind("newer preview")
        .bind(process_id.to_string())
        .execute(runtime.pool.as_ref())
        .await
        .expect("concurrent metadata write should succeed");

        let updated = runtime
            .update_process_git_info(
                process_id,
                Some(Some("abc123")),
                Some(Some("feature/branch")),
                Some(Some("git@example.com:openai/codex.git")),
            )
            .await
            .expect("git info update should succeed");
        assert!(updated, "git info update should touch the thread row");

        let persisted = runtime
            .get_process(process_id)
            .await
            .expect("thread should load")
            .expect("thread should exist");
        assert_eq!(persisted.tokens_used, 123);
        assert_eq!(
            persisted.first_user_message.as_deref(),
            Some("newer preview")
        );
        assert_eq!(datetime_to_epoch_seconds(persisted.updated_at), updated_at);
        assert_eq!(persisted.git_sha.as_deref(), Some("abc123"));
        assert_eq!(persisted.git_branch.as_deref(), Some("feature/branch"));
        assert_eq!(
            persisted.git_origin_url.as_deref(),
            Some("git@example.com:openai/codex.git")
        );
    }

    #[tokio::test]
    async fn insert_thread_if_absent_preserves_existing_metadata() {
        let codex_home = unique_temp_dir();
        let runtime = StateRuntime::init(codex_home.clone(), "test-provider".to_string())
            .await
            .expect("state db should initialize");
        let process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000791")
            .expect("valid thread id");

        let mut existing = test_process_metadata(&codex_home, process_id, codex_home.clone());
        existing.tokens_used = 123;
        existing.first_user_message = Some("newer preview".to_string());
        existing.updated_at = jiff::Timestamp::new(1_700_000_100, 0).expect("timestamp");
        runtime
            .upsert_process(&existing)
            .await
            .expect("initial upsert should succeed");

        let mut fallback = test_process_metadata(&codex_home, process_id, codex_home.clone());
        fallback.tokens_used = 0;
        fallback.first_user_message = None;
        fallback.updated_at = jiff::Timestamp::new(1_700_000_000, 0).expect("timestamp");

        let inserted = runtime
            .insert_process_if_absent(&fallback)
            .await
            .expect("insert should succeed");
        assert!(!inserted, "existing rows should not be overwritten");

        let persisted = runtime
            .get_process(process_id)
            .await
            .expect("thread should load")
            .expect("thread should exist");
        assert_eq!(persisted.tokens_used, 123);
        assert_eq!(
            persisted.first_user_message.as_deref(),
            Some("newer preview")
        );
        assert_eq!(
            datetime_to_epoch_seconds(persisted.updated_at),
            datetime_to_epoch_seconds(existing.updated_at)
        );
    }

    #[tokio::test]
    async fn update_process_git_info_can_clear_fields() {
        let codex_home = unique_temp_dir();
        let runtime = StateRuntime::init(codex_home.clone(), "test-provider".to_string())
            .await
            .expect("state db should initialize");
        let process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000790")
            .expect("valid thread id");
        let mut metadata = test_process_metadata(&codex_home, process_id, codex_home.clone());
        metadata.git_sha = Some("abc123".to_string());
        metadata.git_branch = Some("feature/branch".to_string());
        metadata.git_origin_url = Some("git@example.com:openai/codex.git".to_string());

        runtime
            .upsert_process(&metadata)
            .await
            .expect("initial upsert should succeed");

        let updated = runtime
            .update_process_git_info(process_id, Some(None), Some(None), Some(None))
            .await
            .expect("git info clear should succeed");
        assert!(updated, "git info clear should touch the thread row");

        let persisted = runtime
            .get_process(process_id)
            .await
            .expect("thread should load")
            .expect("thread should exist");
        assert_eq!(persisted.git_sha, None);
        assert_eq!(persisted.git_branch, None);
        assert_eq!(persisted.git_origin_url, None);
    }

    #[tokio::test]
    async fn touch_process_updated_at_updates_only_updated_at() {
        let codex_home = unique_temp_dir();
        let runtime = StateRuntime::init(codex_home.clone(), "test-provider".to_string())
            .await
            .expect("state db should initialize");
        let process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000791")
            .expect("valid thread id");
        let mut metadata = test_process_metadata(&codex_home, process_id, codex_home.clone());
        metadata.title = "original title".to_string();
        metadata.first_user_message = Some("first-user-message".to_string());

        runtime
            .upsert_process(&metadata)
            .await
            .expect("initial upsert should succeed");

        let touched_at = jiff::Timestamp::new(1_700_001_111, 0).expect("timestamp");
        let touched = runtime
            .touch_process_updated_at(process_id, touched_at)
            .await
            .expect("touch should succeed");
        assert!(touched);

        let persisted = runtime
            .get_process(process_id)
            .await
            .expect("thread should load")
            .expect("thread should exist");
        assert_eq!(persisted.updated_at, touched_at);
        assert_eq!(persisted.title, "original title");
        assert_eq!(
            persisted.first_user_message.as_deref(),
            Some("first-user-message")
        );
    }

    #[tokio::test]
    async fn apply_rollout_items_uses_override_updated_at_when_provided() {
        let codex_home = unique_temp_dir();
        let runtime = StateRuntime::init(codex_home.clone(), "test-provider".to_string())
            .await
            .expect("state db should initialize");
        let process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000792")
            .expect("valid thread id");
        let metadata = test_process_metadata(&codex_home, process_id, codex_home.clone());

        runtime
            .upsert_process(&metadata)
            .await
            .expect("initial upsert should succeed");

        let builder = ProcessMetadataBuilder::new(
            process_id,
            metadata.rollout_path.clone(),
            metadata.created_at,
            SessionSource::Cli,
        );
        let items = vec![RolloutItem::EventMsg(EventMsg::TokenCount(
            chaos_ipc::protocol::TokenCountEvent {
                info: Some(chaos_ipc::protocol::TokenUsageInfo {
                    total_token_usage: chaos_ipc::protocol::TokenUsage {
                        input_tokens: 0,
                        cached_input_tokens: 0,
                        output_tokens: 0,
                        reasoning_output_tokens: 0,
                        total_tokens: 321,
                    },
                    last_token_usage: chaos_ipc::protocol::TokenUsage::default(),
                    model_context_window: None,
                }),
                rate_limits: None,
            },
        ))];
        let override_updated_at = jiff::Timestamp::new(1_700_001_234, 0).expect("timestamp");

        runtime
            .apply_rollout_items(&builder, &items, None, Some(override_updated_at))
            .await
            .expect("apply_rollout_items should succeed");

        let persisted = runtime
            .get_process(process_id)
            .await
            .expect("thread should load")
            .expect("thread should exist");
        assert_eq!(persisted.tokens_used, 321);
        assert_eq!(persisted.updated_at, override_updated_at);
    }
}
