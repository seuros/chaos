use super::*;
use sqlx::Row;

#[tokio::test]
async fn runtime_db_uses_new_filename() {
    assert_eq!(runtime_db_filename(), "chaos.sqlite");
}

#[tokio::test]
async fn open_runtime_db_creates_unified_runtime_schema() {
    let chaos_home = test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&chaos_home)
        .await
        .expect("create temp chaos home");

    let pool = open_runtime_db(chaos_home.as_path())
        .await
        .expect("open runtime db");

    for table_name in [
        "processes",
        "process_closure",
        "process_leases",
        "journal_entries",
        "logs",
        "message_history",
        "backfill_state",
        "jobs",
        "stage1_outputs",
        "process_dynamic_tools",
        "agent_jobs",
        "agent_job_items",
        "cron_jobs",
        "model_catalog_cache",
        "project_trust",
    ] {
        let row = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(table_name)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|_| panic!("table {table_name} should exist"));

        let discovered_name: String = row.get("name");
        assert_eq!(discovered_name, table_name);
    }

    for view_name in [
        "due_cron_jobs",
        "valid_model_cache",
        "active_processes",
        "archived_processes",
        "active_process_leases",
        "process_message_counts",
    ] {
        let row = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'view' AND name = ?")
            .bind(view_name)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|_| panic!("view {view_name} should exist"));

        let discovered_name: String = row.get("name");
        assert_eq!(discovered_name, view_name);
    }

    for trigger_name in [
        "cron_jobs_touch",
        "processes_touch",
        "processes_parent_process_id_immutable",
        "processes_fork_at_seq_immutable",
        "processes_insert_closure",
        "process_leases_touch",
        "agent_jobs_touch",
        "agent_job_items_touch",
        "journal_entries_no_update",
        "journal_entries_no_delete",
    ] {
        let row = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'trigger' AND name = ?")
            .bind(trigger_name)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|_| panic!("trigger {trigger_name} should exist"));

        let discovered_name: String = row.get("name");
        assert_eq!(discovered_name, trigger_name);
    }

    assert!(
        tokio::fs::try_exists(&runtime_db_path(chaos_home.as_path()))
            .await
            .expect("stat runtime db"),
        "runtime db file should be created on demand"
    );
}

#[tokio::test]
async fn open_runtime_db_url_creates_schema() {
    let chaos_home = test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&chaos_home)
        .await
        .expect("create temp chaos home");

    let db_path = runtime_db_path(chaos_home.as_path());
    let db_url = format!("sqlite://{}", db_path.display());
    let pool = open_runtime_db_url(&db_url)
        .await
        .expect("open runtime db from sqlite url");

    let row =
        sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'cron_jobs'")
            .fetch_one(&pool)
            .await
            .expect("cron_jobs table should exist");

    let table_name: String = row.get("name");
    assert_eq!(table_name, "cron_jobs");
    assert!(
        tokio::fs::try_exists(&db_path)
            .await
            .expect("stat runtime db"),
        "runtime db file should be created from sqlite url"
    );
}

#[tokio::test]
async fn clamp_exchange_persists_postgres() {
    let Some(database_url) = std::env::var("TEST_DATABASE_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    else {
        eprintln!("skipping postgres clamp test; TEST_DATABASE_URL not set");
        return;
    };

    let pool = open_runtime_db_postgres_url(&database_url)
        .await
        .expect("open postgres runtime");
    let runtime = RuntimeDbHandle::from_postgres_pool(
        test_support::unique_temp_dir(),
        "test-provider".to_string(),
        pool.clone(),
    );

    // Unique session id so repeated runs against the shared container don't collide.
    let session_id = format!("sess-{}", uuid::Uuid::new_v4());
    let rec = ClampExchangeRecord {
        session_id: Some(session_id.clone()),
        turn_id: Some("turn-pg".to_string()),
        method: "POST".to_string(),
        path: "/v1/messages".to_string(),
        status: Some(200),
        headers_json: serde_json::json!({"authorization": "<redacted>"}),
        request_json: Some(serde_json::json!({"model": "claude-sonnet-4-6"})),
        response_body: Some("event: message_start\n".to_string()),
        response_truncated: true,
    };
    runtime
        .record_clamp_exchange(&rec)
        .await
        .expect("record exchange (pg)");

    let row = sqlx::query(
        "SELECT method, status, headers_json, request_json, response_body, \
             response_truncated FROM clamp_exchanges WHERE session_id = $1",
    )
    .bind(&session_id)
    .fetch_one(&pool)
    .await
    .expect("read back exchange (pg)");

    let method: String = row.try_get("method").expect("method");
    let status: i32 = row.try_get("status").expect("status");
    let headers_json: serde_json::Value = row.try_get("headers_json").expect("headers_json");
    let request_json: serde_json::Value = row.try_get("request_json").expect("request_json");
    let response_body: String = row.try_get("response_body").expect("response_body");
    let truncated: bool = row.try_get("response_truncated").expect("truncated");

    assert_eq!(method, "POST");
    assert_eq!(status, 200);
    assert_eq!(headers_json["authorization"], "<redacted>");
    assert_eq!(request_json["model"], "claude-sonnet-4-6");
    assert!(response_body.contains("message_start"));
    assert!(truncated);

    // Cleanup so the container stays tidy across runs.
    let _ = sqlx::query("DELETE FROM clamp_exchanges WHERE session_id = $1")
        .bind(&session_id)
        .execute(&pool)
        .await;
}

#[tokio::test]
async fn clamp_exchange_persists() {
    let chaos_home = test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&chaos_home)
        .await
        .expect("create temp chaos home");

    let runtime = RuntimeDbHandle::Sqlite(
        StateRuntime::init(chaos_home, "test-provider".to_string())
            .await
            .expect("open runtime"),
    );

    let rec = ClampExchangeRecord {
        session_id: Some("sess-1".to_string()),
        turn_id: Some("turn-1".to_string()),
        method: "POST".to_string(),
        path: "/v1/messages?beta=true".to_string(),
        status: Some(200),
        headers_json: serde_json::json!({"authorization": "<redacted>"}),
        request_json: Some(serde_json::json!({"model": "claude-sonnet-4-6"})),
        response_body: Some("event: message_start\n".to_string()),
        response_truncated: false,
    };
    runtime
        .record_clamp_exchange(&rec)
        .await
        .expect("record exchange");

    let pool = runtime.sqlite_pool_cloned().expect("sqlite pool");
    let row = sqlx::query(
        "SELECT session_id, method, path, status, headers_json, request_json, \
             response_body, response_truncated FROM clamp_exchanges",
    )
    .fetch_one(&pool)
    .await
    .expect("read back exchange");

    let session_id: String = row.try_get("session_id").expect("session_id");
    let method: String = row.try_get("method").expect("method");
    let status: i64 = row.try_get("status").expect("status");
    let headers_json: String = row.try_get("headers_json").expect("headers_json");
    let response_body: String = row.try_get("response_body").expect("response_body");
    let truncated: i64 = row.try_get("response_truncated").expect("truncated");

    assert_eq!(session_id, "sess-1");
    assert_eq!(method, "POST");
    assert_eq!(status, 200);
    assert!(headers_json.contains("<redacted>"));
    assert!(response_body.contains("message_start"));
    assert_eq!(truncated, 0);
}

#[tokio::test]
async fn project_trust_round_trips() {
    let chaos_home = test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&chaos_home)
        .await
        .expect("create temp chaos home");

    let runtime = RuntimeDbHandle::Sqlite(
        StateRuntime::init(chaos_home, "test-provider".to_string())
            .await
            .expect("open runtime"),
    );
    let project_path = PathBuf::from("/tmp/trusted-project");

    assert_eq!(
        runtime
            .get_project_trust(&project_path)
            .await
            .expect("query trust"),
        None
    );

    runtime
        .set_project_trust(&project_path, TrustLevel::Trusted)
        .await
        .expect("set trust");
    assert_eq!(
        runtime
            .get_project_trust(&project_path)
            .await
            .expect("query trust"),
        Some(TrustLevel::Trusted)
    );

    runtime
        .set_project_trust(&project_path, TrustLevel::Untrusted)
        .await
        .expect("update trust");
    assert_eq!(
        runtime
            .get_project_trust(&project_path)
            .await
            .expect("query trust"),
        Some(TrustLevel::Untrusted)
    );
}

#[tokio::test]
async fn global_mcp_servers_round_trip() {
    let chaos_home = test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&chaos_home)
        .await
        .expect("create temp chaos home");

    let runtime = RuntimeDbHandle::Sqlite(
        StateRuntime::init(chaos_home, "test-provider".to_string())
            .await
            .expect("open runtime"),
    );
    let mut servers = std::collections::BTreeMap::new();
    servers.insert(
        "docs".to_string(),
        McpServerConfig {
            transport: chaos_sysctl::types::McpServerTransportConfig::Stdio {
                command: "echo".to_string(),
                args: vec!["hello".to_string()],
                env: None,
                env_vars: Vec::new(),
                cwd: None,
            },
            enabled: true,
            required: false,
            disabled_reason: None,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            enabled_tools: None,
            disabled_tools: None,
            scopes: None,
            oauth_resource: None,
            r#type: None,
            oauth: None,
        },
    );

    assert!(
        runtime
            .list_global_mcp_servers()
            .await
            .expect("list servers")
            .is_empty()
    );

    runtime
        .replace_global_mcp_servers(&servers)
        .await
        .expect("replace servers");
    assert_eq!(
        runtime
            .list_global_mcp_servers()
            .await
            .expect("list servers"),
        servers
    );

    servers.clear();
    runtime
        .replace_global_mcp_servers(&servers)
        .await
        .expect("clear servers");
    assert!(
        runtime
            .list_global_mcp_servers()
            .await
            .expect("list servers")
            .is_empty()
    );
}

#[tokio::test]
async fn global_mcp_server_upsert_and_delete_are_per_server() {
    let chaos_home = test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&chaos_home)
        .await
        .expect("create temp chaos home");

    let runtime = StateRuntime::init(chaos_home.clone(), "openai".to_string())
        .await
        .expect("init runtime");

    let docs = McpServerConfig {
        transport: chaos_sysctl::types::McpServerTransportConfig::Stdio {
            command: "docs-server".to_string(),
            args: vec!["--port".to_string(), "4000".to_string()],
            env: None,
            env_vars: Vec::new(),
            cwd: None,
        },
        enabled: true,
        required: false,
        disabled_reason: None,
        startup_timeout_sec: None,
        tool_timeout_sec: None,
        enabled_tools: None,
        disabled_tools: None,
        scopes: None,
        oauth_resource: None,
        r#type: None,
        oauth: None,
    };
    let issues = McpServerConfig {
        transport: chaos_sysctl::types::McpServerTransportConfig::StreamableHttp {
            url: "https://example.com/issues".to_string(),
            bearer_token: Some("secret-token".to_string()),
            bearer_token_env_var: None,
            http_headers: None,
            env_http_headers: None,
        },
        enabled: true,
        required: false,
        disabled_reason: None,
        startup_timeout_sec: None,
        tool_timeout_sec: None,
        enabled_tools: None,
        disabled_tools: None,
        scopes: None,
        oauth_resource: None,
        r#type: None,
        oauth: None,
    };

    runtime
        .upsert_global_mcp_server("docs", &docs)
        .await
        .expect("upsert docs");
    runtime
        .upsert_global_mcp_server("issues", &issues)
        .await
        .expect("upsert issues");

    assert_eq!(
        runtime
            .get_global_mcp_server("docs")
            .await
            .expect("get docs"),
        Some(docs.clone())
    );
    assert_eq!(
        runtime
            .get_global_mcp_server("issues")
            .await
            .expect("get issues"),
        Some(issues.clone())
    );

    let removed = runtime
        .delete_global_mcp_server("docs")
        .await
        .expect("delete docs");
    assert!(removed);
    assert_eq!(
        runtime
            .get_global_mcp_server("docs")
            .await
            .expect("get docs after delete"),
        None
    );
    assert_eq!(
        runtime
            .get_global_mcp_server("issues")
            .await
            .expect("get issues after delete"),
        Some(issues)
    );

    let removed = runtime
        .delete_global_mcp_server("docs")
        .await
        .expect("delete docs again");
    assert!(!removed);
}

#[tokio::test]
async fn journal_entries_are_append_only() {
    let chaos_home = test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&chaos_home)
        .await
        .expect("create temp chaos home");

    let pool = open_runtime_db(chaos_home.as_path())
        .await
        .expect("open runtime db");

    sqlx::query(
            "INSERT INTO processes (
                id, parent_process_id, fork_at_seq, source, source_json, model_provider, cwd,
                created_at, updated_at, archived_at, title, sandbox_policy, approval_mode,
                tokens_used, first_user_message, cli_version, agent_nickname, agent_role,
                git_sha, git_branch, git_origin_url, memory_mode, model, reasoning_effort,
                agent_path, process_name
            ) VALUES (?, NULL, NULL, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, ?, NULL, NULL, NULL, NULL)",
        )
        .bind("process-1")
        .bind("cli")
        .bind("\"cli\"")
        .bind("openai")
        .bind("/tmp")
        .bind(1_i64)
        .bind(1_i64)
        .bind("")
        .bind("")
        .bind("")
        .bind(0_i64)
        .bind("")
        .bind("")
        .bind("enabled")
        .execute(&pool)
        .await
        .expect("insert process");

    sqlx::query(
        "INSERT INTO journal_entries (process_id, seq, recorded_at, item_type, payload_json)
             VALUES (?, ?, ?, ?, ?)",
    )
    .bind("process-1")
    .bind(0_i64)
    .bind(1_775_606_400_i64)
    .bind("response_item")
    .bind("{\"ok\":true}")
    .execute(&pool)
    .await
    .expect("insert journal entry");

    let update_err =
        sqlx::query("UPDATE journal_entries SET payload_json = ? WHERE process_id = ? AND seq = ?")
            .bind("{\"ok\":false}")
            .bind("process-1")
            .bind(0_i64)
            .execute(&pool)
            .await
            .expect_err("journal entry update should fail");
    assert!(
        update_err.to_string().contains("append-only"),
        "unexpected update error: {update_err}"
    );

    let delete_err = sqlx::query("DELETE FROM journal_entries WHERE process_id = ? AND seq = ?")
        .bind("process-1")
        .bind(0_i64)
        .execute(&pool)
        .await
        .expect_err("journal entry delete should fail");
    assert!(
        delete_err.to_string().contains("append-only"),
        "unexpected delete error: {delete_err}"
    );
}

#[tokio::test]
async fn processes_touch_trigger_updates_updated_at() {
    let chaos_home = test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&chaos_home)
        .await
        .expect("create temp chaos home");

    let pool = open_runtime_db(chaos_home.as_path())
        .await
        .expect("open runtime db");

    sqlx::query(
            "INSERT INTO processes (
                id, parent_process_id, fork_at_seq, source, source_json, model_provider, cwd,
                created_at, updated_at, archived_at, title, sandbox_policy, approval_mode,
                tokens_used, first_user_message, cli_version, agent_nickname, agent_role,
                git_sha, git_branch, git_origin_url, memory_mode, model, reasoning_effort,
                agent_path, process_name
            ) VALUES (?, NULL, NULL, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, ?, NULL, NULL, NULL, NULL)",
        )
        .bind("process-1")
        .bind("cli")
        .bind("\"cli\"")
        .bind("openai")
        .bind("/tmp")
        .bind(1_i64)
        .bind(1_i64)
        .bind("")
        .bind("")
        .bind("")
        .bind(0_i64)
        .bind("")
        .bind("")
        .bind("enabled")
        .execute(&pool)
        .await
        .expect("insert process");

    sqlx::query("UPDATE processes SET title = ? WHERE id = ?")
        .bind("hello")
        .bind("process-1")
        .execute(&pool)
        .await
        .expect("update process title");

    let updated_at: i64 = sqlx::query_scalar("SELECT updated_at FROM processes WHERE id = ?")
        .bind("process-1")
        .fetch_one(&pool)
        .await
        .expect("fetch updated_at");
    assert!(updated_at > 1, "touch trigger should advance updated_at");
}

async fn insert_test_process(
    pool: &SqlitePool,
    id: &str,
    parent_process_id: Option<&str>,
    fork_at_seq: Option<i64>,
) {
    sqlx::query(
            "INSERT INTO processes (
                id, parent_process_id, fork_at_seq, source, source_json, model_provider, cwd,
                created_at, updated_at, archived_at, title, sandbox_policy, approval_mode,
                tokens_used, first_user_message, cli_version, agent_nickname, agent_role,
                git_sha, git_branch, git_origin_url, memory_mode, model, reasoning_effort,
                agent_path, process_name
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, ?, NULL, NULL, NULL, NULL)",
        )
        .bind(id)
        .bind(parent_process_id)
        .bind(fork_at_seq)
        .bind("cli")
        .bind("\"cli\"")
        .bind("openai")
        .bind("/tmp")
        .bind(1_i64)
        .bind(1_i64)
        .bind("")
        .bind("")
        .bind("")
        .bind(0_i64)
        .bind("")
        .bind("")
        .bind("enabled")
        .execute(pool)
        .await
        .unwrap_or_else(|_| panic!("insert process {id}"));
}

#[tokio::test]
async fn process_closure_rows_are_materialized_from_parent_links() {
    let chaos_home = test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&chaos_home)
        .await
        .expect("create temp chaos home");

    let pool = open_runtime_db(chaos_home.as_path())
        .await
        .expect("open runtime db");

    insert_test_process(&pool, "root", None, None).await;
    insert_test_process(&pool, "child", Some("root"), Some(7)).await;
    insert_test_process(&pool, "grandchild", Some("child"), Some(3)).await;

    let closure_rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT ancestor_process_id, descendant_process_id, depth
             FROM process_closure
             ORDER BY ancestor_process_id, descendant_process_id",
    )
    .fetch_all(&pool)
    .await
    .expect("fetch process closure rows");

    assert_eq!(
        closure_rows,
        vec![
            ("child".to_string(), "child".to_string(), 0),
            ("child".to_string(), "grandchild".to_string(), 1),
            ("grandchild".to_string(), "grandchild".to_string(), 0),
            ("root".to_string(), "child".to_string(), 1),
            ("root".to_string(), "grandchild".to_string(), 2),
            ("root".to_string(), "root".to_string(), 0),
        ]
    );
}

#[tokio::test]
async fn process_lineage_columns_are_immutable_after_insert() {
    let chaos_home = test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&chaos_home)
        .await
        .expect("create temp chaos home");

    let pool = open_runtime_db(chaos_home.as_path())
        .await
        .expect("open runtime db");

    insert_test_process(&pool, "root", None, None).await;
    insert_test_process(&pool, "child", Some("root"), Some(7)).await;

    let parent_err = sqlx::query("UPDATE processes SET parent_process_id = ? WHERE id = ?")
        .bind::<Option<&str>>(None)
        .bind("child")
        .execute(&pool)
        .await
        .expect_err("updating parent_process_id should fail");
    assert!(
        parent_err.to_string().contains("immutable"),
        "unexpected parent immutability error: {parent_err}"
    );

    let fork_err = sqlx::query("UPDATE processes SET fork_at_seq = ? WHERE id = ?")
        .bind(8_i64)
        .bind("child")
        .execute(&pool)
        .await
        .expect_err("updating fork_at_seq should fail");
    assert!(
        fork_err.to_string().contains("immutable"),
        "unexpected fork_at_seq immutability error: {fork_err}"
    );
}
