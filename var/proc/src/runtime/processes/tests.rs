use super::*;
use crate::runtime::test_support::test_process_metadata;
use crate::runtime::test_support::unique_temp_dir;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::GitInfo;
use chaos_ipc::protocol::SessionMeta;
use chaos_ipc::protocol::SessionMetaLine;
use chaos_ipc::protocol::SessionSource;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

#[tokio::test]
async fn list_processes_includes_rows_without_derived_user_message() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("runtime db should initialize");
    let process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000122").expect("valid process id");
    let mut metadata = test_process_metadata(&chaos_home, process_id, chaos_home.clone());
    metadata.first_user_message = None;

    runtime
        .upsert_process(&metadata)
        .await
        .expect("legacy metadata row should persist");
    let page = runtime
        .list_processes(
            10,
            None,
            SortKey::UpdatedAt,
            &["cli".to_string()],
            None,
            false,
            None,
        )
        .await
        .expect("process listing should succeed");

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, process_id);
    assert_eq!(page.items[0].first_user_message, None);
}

#[tokio::test]
async fn upsert_thread_keeps_creation_memory_mode_for_existing_rows() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("runtime db should initialize");
    let process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000123").expect("valid thread id");
    let mut metadata = test_process_metadata(&chaos_home, process_id, chaos_home.clone());

    runtime
        .upsert_process_with_creation_memory_mode(&metadata, Some("disabled"))
        .await
        .expect("initial insert should succeed");

    let memory_mode: String = sqlx::query_scalar("SELECT memory_mode FROM processes WHERE id = ?")
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

    let memory_mode: String = sqlx::query_scalar("SELECT memory_mode FROM processes WHERE id = ?")
        .bind(process_id.to_string())
        .fetch_one(runtime.pool.as_ref())
        .await
        .expect("memory mode should remain readable");
    assert_eq!(memory_mode, "disabled");
}

#[tokio::test]
async fn apply_rollout_items_restores_memory_mode_from_session_meta() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("runtime db should initialize");
    let process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000456").expect("valid thread id");
    let metadata = test_process_metadata(&chaos_home, process_id, chaos_home.clone());

    runtime
        .upsert_process(&metadata)
        .await
        .expect("initial upsert should succeed");

    let builder = ProcessMetadataBuilder::new(process_id, metadata.created_at, SessionSource::Cli);
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
async fn apply_rollout_items_projects_persisted_user_response() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("runtime db should initialize");
    let process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000458").expect("valid thread id");
    let mut metadata = test_process_metadata(&chaos_home, process_id, chaos_home.clone());
    metadata.title.clear();
    metadata.first_user_message = None;
    runtime
        .upsert_process(&metadata)
        .await
        .expect("initial upsert should succeed");

    let builder = ProcessMetadataBuilder::new(process_id, metadata.created_at, SessionSource::Cli);
    let items = vec![RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: concat!(
                "<environment_context>\n",
                "  <cwd>/tmp/project</cwd>\n",
                "</environment_context>\n\n",
                "## My request for FreeChaOS: Keep resume fast"
            )
            .to_string(),
        }],
        end_turn: None,
        phase: None,
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
    assert_eq!(
        persisted.first_user_message.as_deref(),
        Some("Keep resume fast")
    );
    assert_eq!(persisted.title, "Keep resume fast");
}

#[tokio::test]
async fn apply_rollout_items_preserves_existing_git_branch_and_fills_missing_git_fields() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("runtime db should initialize");
    let process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000457").expect("valid thread id");
    let mut metadata = test_process_metadata(&chaos_home, process_id, chaos_home.clone());
    metadata.git_branch = Some("sqlite-branch".to_string());

    runtime
        .upsert_process(&metadata)
        .await
        .expect("initial upsert should succeed");

    let created_at = metadata.created_at.to_string();
    let builder = ProcessMetadataBuilder::new(process_id, metadata.created_at, SessionSource::Cli);
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
            repository_url: Some("git@example.com:seuros/chaos.git".to_string()),
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
        Some("git@example.com:seuros/chaos.git")
    );
}

#[tokio::test]
async fn update_process_git_info_preserves_newer_non_git_metadata() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("runtime db should initialize");
    let process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000789").expect("valid thread id");
    let metadata = test_process_metadata(&chaos_home, process_id, chaos_home.clone());

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
            Some(Some("git@example.com:seuros/chaos.git")),
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
    // The processes_touch trigger advances updated_at on any UPDATE
    // where it was not explicitly changed, so it will be >= the value
    // we manually set.
    assert!(
        datetime_to_epoch_seconds(persisted.updated_at) >= updated_at,
        "updated_at should advance (trigger) or stay the same"
    );
    assert_eq!(persisted.git_sha.as_deref(), Some("abc123"));
    assert_eq!(persisted.git_branch.as_deref(), Some("feature/branch"));
    assert_eq!(
        persisted.git_origin_url.as_deref(),
        Some("git@example.com:seuros/chaos.git")
    );
}

#[tokio::test]
async fn insert_thread_if_absent_preserves_existing_metadata() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("runtime db should initialize");
    let process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000791").expect("valid thread id");

    let mut existing = test_process_metadata(&chaos_home, process_id, chaos_home.clone());
    existing.tokens_used = 123;
    existing.first_user_message = Some("newer preview".to_string());
    existing.updated_at = jiff::Timestamp::new(1_700_000_100, 0).expect("timestamp");
    runtime
        .upsert_process(&existing)
        .await
        .expect("initial upsert should succeed");

    let mut fallback = test_process_metadata(&chaos_home, process_id, chaos_home.clone());
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
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("runtime db should initialize");
    let process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000790").expect("valid thread id");
    let mut metadata = test_process_metadata(&chaos_home, process_id, chaos_home.clone());
    metadata.git_sha = Some("abc123".to_string());
    metadata.git_branch = Some("feature/branch".to_string());
    metadata.git_origin_url = Some("git@example.com:seuros/chaos.git".to_string());

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
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("runtime db should initialize");
    let process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000791").expect("valid thread id");
    let mut metadata = test_process_metadata(&chaos_home, process_id, chaos_home.clone());
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
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("runtime db should initialize");
    let process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000792").expect("valid thread id");
    let metadata = test_process_metadata(&chaos_home, process_id, chaos_home.clone());

    runtime
        .upsert_process(&metadata)
        .await
        .expect("initial upsert should succeed");

    let builder = ProcessMetadataBuilder::new(process_id, metadata.created_at, SessionSource::Cli);
    let items = vec![RolloutItem::EventMsg(EventMsg::TokenCount(
        chaos_ipc::protocol::TokenCountEvent {
            info: Some(chaos_ipc::protocol::TokenUsageInfo {
                total_token_usage: chaos_ipc::protocol::TokenUsage {
                    total_tokens: 321,
                    ..Default::default()
                },
                last_token_usage: chaos_ipc::protocol::TokenUsage::default(),
                model_context_window: None,
            }),
            rate_limits: None,
            provider_request_started: false,
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
