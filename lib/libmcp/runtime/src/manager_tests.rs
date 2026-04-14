use super::*;
use chaos_ipc::protocol::GranularApprovalConfig;
use chaos_ipc::protocol::McpAuthStatus;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;

fn create_test_tool(server_name: &str, tool_name: &str) -> ToolInfo {
    ToolInfo {
        server_name: server_name.to_string(),
        tool_name: tool_name.to_string(),
        tool_namespace: server_name.to_string(),
        tool: McpToolInfo {
            name: tool_name.to_string(),
            title: None,
            description: Some(format!("Test tool: {tool_name}")),
            input_schema: serde_json::json!({}),
            output_schema: None,
            annotations: None,
            execution: None,
            icons: None,
            meta: None,
        },
        connector_id: None,
        connector_name: None,
        connector_description: None,
    }
}

#[test]
fn elicitation_granular_policy_defaults_to_prompting() {
    assert!(!elicitation_is_rejected_by_policy(
        ApprovalPolicy::Interactive
    ));
    assert!(!elicitation_is_rejected_by_policy(
        ApprovalPolicy::Supervised
    ));
    assert!(elicitation_is_rejected_by_policy(ApprovalPolicy::Granular(
        GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            request_permissions: true,
            mcp_elicitations: false,
        }
    )));
}

#[test]
fn elicitation_granular_policy_respects_headless_and_config() {
    assert!(elicitation_is_rejected_by_policy(ApprovalPolicy::Headless));
    assert!(elicitation_is_rejected_by_policy(ApprovalPolicy::Granular(
        GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            request_permissions: true,
            mcp_elicitations: false,
        }
    )));
}

#[test]
fn test_qualify_tools_short_non_duplicated_names() {
    let tools = vec![
        create_test_tool("server1", "tool1"),
        create_test_tool("server1", "tool2"),
    ];

    let qualified_tools = qualify_tools(tools);

    assert_eq!(qualified_tools.len(), 2);
    assert!(qualified_tools.contains_key("mcp__server1__tool1"));
    assert!(qualified_tools.contains_key("mcp__server1__tool2"));
}

#[test]
fn test_qualify_tools_duplicated_names_skipped() {
    let tools = vec![
        create_test_tool("server1", "duplicate_tool"),
        create_test_tool("server1", "duplicate_tool"),
    ];

    let qualified_tools = qualify_tools(tools);

    // Only the first tool should remain, the second is skipped
    assert_eq!(qualified_tools.len(), 1);
    assert!(qualified_tools.contains_key("mcp__server1__duplicate_tool"));
}

#[test]
fn test_qualify_tools_long_names_same_server() {
    let server_name = "my_server";

    let tools = vec![
        create_test_tool(
            server_name,
            "extremely_lengthy_function_name_that_absolutely_surpasses_all_reasonable_limits",
        ),
        create_test_tool(
            server_name,
            "yet_another_extremely_lengthy_function_name_that_absolutely_surpasses_all_reasonable_limits",
        ),
    ];

    let qualified_tools = qualify_tools(tools);

    assert_eq!(qualified_tools.len(), 2);

    let mut keys: Vec<_> = qualified_tools.keys().cloned().collect();
    keys.sort();

    assert_eq!(keys[0].len(), 64);
    assert_eq!(
        keys[0],
        "mcp__my_server__extremel119a2b97664e41363932dc84de21e2ff1b93b3e9"
    );

    assert_eq!(keys[1].len(), 64);
    assert_eq!(
        keys[1],
        "mcp__my_server__yet_anot419a82a89325c1b477274a41f8c65ea5f3a7f341"
    );
}

#[test]
fn test_qualify_tools_sanitizes_invalid_characters() {
    let tools = vec![create_test_tool("server.one", "tool.two-three")];

    let qualified_tools = qualify_tools(tools);

    assert_eq!(qualified_tools.len(), 1);
    let (qualified_name, tool) = qualified_tools.into_iter().next().expect("one tool");
    assert_eq!(qualified_name, "mcp__server_one__tool_two_three");

    // The key is sanitized for OpenAI, but we keep original parts for the actual MCP call.
    assert_eq!(tool.server_name, "server.one");
    assert_eq!(tool.tool_name, "tool.two-three");

    assert!(
        qualified_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
        "qualified name must be Responses API compatible: {qualified_name:?}"
    );
}

#[test]
fn mcp_client_implementation_version_is_not_placeholder() {
    assert_ne!(mcp_client_implementation_version(), "0.0.0");
}

#[test]
fn tool_filter_allows_by_default() {
    let filter = ToolFilter::default();

    assert!(filter.allows("any"));
}

#[test]
fn tool_filter_applies_enabled_list() {
    let filter = ToolFilter {
        enabled: Some(HashSet::from(["allowed".to_string()])),
        disabled: HashSet::new(),
    };

    assert!(filter.allows("allowed"));
    assert!(!filter.allows("denied"));
}

#[test]
fn tool_filter_applies_disabled_list() {
    let filter = ToolFilter {
        enabled: None,
        disabled: HashSet::from(["blocked".to_string()]),
    };

    assert!(!filter.allows("blocked"));
    assert!(filter.allows("open"));
}

#[test]
fn tool_filter_applies_enabled_then_disabled() {
    let filter = ToolFilter {
        enabled: Some(HashSet::from(["keep".to_string(), "remove".to_string()])),
        disabled: HashSet::from(["remove".to_string()]),
    };

    assert!(filter.allows("keep"));
    assert!(!filter.allows("remove"));
    assert!(!filter.allows("unknown"));
}

#[test]
fn filter_tools_applies_per_server_filters() {
    let server1_tools = vec![
        create_test_tool("server1", "tool_a"),
        create_test_tool("server1", "tool_b"),
    ];
    let server2_tools = vec![create_test_tool("server2", "tool_a")];
    let server1_filter = ToolFilter {
        enabled: Some(HashSet::from(["tool_a".to_string(), "tool_b".to_string()])),
        disabled: HashSet::from(["tool_b".to_string()]),
    };
    let server2_filter = ToolFilter {
        enabled: None,
        disabled: HashSet::from(["tool_a".to_string()]),
    };

    let filtered: Vec<_> = filter_tools(server1_tools, &server1_filter)
        .into_iter()
        .chain(filter_tools(server2_tools, &server2_filter))
        .collect();

    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].server_name, "server1");
    assert_eq!(filtered[0].tool_name, "tool_a");
}

#[tokio::test]
async fn list_all_tools_uses_startup_snapshot_while_client_is_pending() {
    let startup_tools = vec![create_test_tool("test_server", "calendar_create_event")];
    let pending_client = futures::future::pending::<Result<ManagedClient, StartupOutcomeError>>()
        .boxed()
        .shared();
    let approval_policy = Constrained::allow_any(ApprovalPolicy::Interactive);
    let mut manager = McpConnectionManager::new_uninitialized(&approval_policy);
    manager.clients.insert(
        "test_server".to_string(),
        AsyncManagedClient {
            client: pending_client,
            startup_snapshot: Some(startup_tools),
            startup_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            cwd: Arc::new(StdRwLock::new(PathBuf::from("/tmp"))),
        },
    );

    let tools = manager.list_all_tools().await;
    let tool = tools
        .get("mcp__test_server__calendar_create_event")
        .expect("tool from startup cache");
    assert_eq!(tool.server_name, "test_server");
    assert_eq!(tool.tool_name, "calendar_create_event");
}

#[tokio::test]
async fn list_all_tools_blocks_while_client_is_pending_without_startup_snapshot() {
    let pending_client = futures::future::pending::<Result<ManagedClient, StartupOutcomeError>>()
        .boxed()
        .shared();
    let approval_policy = Constrained::allow_any(ApprovalPolicy::Interactive);
    let mut manager = McpConnectionManager::new_uninitialized(&approval_policy);
    manager.clients.insert(
        "test_server".to_string(),
        AsyncManagedClient {
            client: pending_client,
            startup_snapshot: None,
            startup_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            cwd: Arc::new(StdRwLock::new(PathBuf::from("/tmp"))),
        },
    );

    let timeout_result =
        tokio::time::timeout(Duration::from_millis(10), manager.list_all_tools()).await;
    assert!(timeout_result.is_err());
}

#[tokio::test]
async fn list_all_tools_does_not_block_when_startup_snapshot_cache_hit_is_empty() {
    let pending_client = futures::future::pending::<Result<ManagedClient, StartupOutcomeError>>()
        .boxed()
        .shared();
    let approval_policy = Constrained::allow_any(ApprovalPolicy::Interactive);
    let mut manager = McpConnectionManager::new_uninitialized(&approval_policy);
    manager.clients.insert(
        "test_server".to_string(),
        AsyncManagedClient {
            client: pending_client,
            startup_snapshot: Some(Vec::new()),
            startup_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            cwd: Arc::new(StdRwLock::new(PathBuf::from("/tmp"))),
        },
    );

    let timeout_result =
        tokio::time::timeout(Duration::from_millis(10), manager.list_all_tools()).await;
    let tools = timeout_result.expect("cache-hit startup snapshot should not block");
    assert!(tools.is_empty());
}

#[tokio::test]
async fn list_all_tools_uses_startup_snapshot_when_client_startup_fails() {
    let startup_tools = vec![create_test_tool("test_server", "calendar_create_event")];
    let failed_client = futures::future::ready::<Result<ManagedClient, StartupOutcomeError>>(Err(
        StartupOutcomeError::Failed {
            error: "startup failed".to_string(),
        },
    ))
    .boxed()
    .shared();
    let approval_policy = Constrained::allow_any(ApprovalPolicy::Interactive);
    let mut manager = McpConnectionManager::new_uninitialized(&approval_policy);
    let startup_complete = Arc::new(std::sync::atomic::AtomicBool::new(true));
    manager.clients.insert(
        "test_server".to_string(),
        AsyncManagedClient {
            client: failed_client,
            startup_snapshot: Some(startup_tools),
            startup_complete,
            cwd: Arc::new(StdRwLock::new(PathBuf::from("/tmp"))),
        },
    );

    let tools = manager.list_all_tools().await;
    let tool = tools
        .get("mcp__test_server__calendar_create_event")
        .expect("tool from startup cache");
    assert_eq!(tool.server_name, "test_server");
    assert_eq!(tool.tool_name, "calendar_create_event");
}

#[test]
fn mcp_init_error_display_prompts_for_github_pat() {
    let server_name = "github";
    let entry = McpAuthStatusEntry {
        config: McpServerConfig {
            transport: McpServerTransportConfig::StreamableHttp {
                url: "https://api.githubcopilot.com/mcp/".to_string(),
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
        },
        auth_status: McpAuthStatus::Unsupported,
    };
    let err: StartupOutcomeError = anyhow::anyhow!("OAuth is unsupported").into();

    let display = mcp_init_error_display(server_name, Some(&entry), &err);

    let expected = format!(
        "GitHub MCP does not support OAuth. Log in by adding a personal access token (https://github.com/settings/personal-access-tokens) to your environment and config.toml:\n[mcp_servers.{server_name}]\nbearer_token_env_var = CHAOS_GITHUB_PERSONAL_ACCESS_TOKEN"
    );

    assert_eq!(expected, display);
}

#[test]
fn mcp_init_error_display_prompts_for_login_when_auth_required() {
    let server_name = "example";
    let err: StartupOutcomeError = anyhow::anyhow!("Auth required for server").into();

    let display = mcp_init_error_display(server_name, None, &err);

    let expected = format!(
        "The {server_name} MCP server is not logged in. Run `chaos mcp login {server_name}`."
    );

    assert_eq!(expected, display);
}

#[test]
fn mcp_init_error_display_reports_generic_errors() {
    let server_name = "custom";
    let entry = McpAuthStatusEntry {
        config: McpServerConfig {
            transport: McpServerTransportConfig::StreamableHttp {
                url: "https://example.com".to_string(),
                bearer_token_env_var: Some("TOKEN".to_string()),
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
        },
        auth_status: McpAuthStatus::Unsupported,
    };
    let err: StartupOutcomeError = anyhow::anyhow!("boom").into();

    let display = mcp_init_error_display(server_name, Some(&entry), &err);

    let expected = format!("MCP client for `{server_name}` failed to start: {err:#}");

    assert_eq!(expected, display);
}

#[test]
fn mcp_init_error_display_includes_startup_timeout_hint() {
    let server_name = "slow";
    let err: StartupOutcomeError = anyhow::anyhow!("request timed out").into();

    let display = mcp_init_error_display(server_name, None, &err);

    assert_eq!(
        "MCP client for `slow` timed out after 10 seconds. Add or adjust `startup_timeout_sec` in your config.toml:\n[mcp_servers.slow]\nstartup_timeout_sec = XX",
        display
    );
}

#[test]
fn transport_origin_extracts_http_origin() {
    let transport = McpServerTransportConfig::StreamableHttp {
        url: "https://example.com:8443/path?query=1".to_string(),
        bearer_token_env_var: None,
        http_headers: None,
        env_http_headers: None,
    };

    assert_eq!(
        transport_origin(&transport),
        Some("https://example.com:8443".to_string())
    );
}

#[test]
fn transport_origin_is_stdio_for_stdio_transport() {
    let transport = McpServerTransportConfig::Stdio {
        command: "server".to_string(),
        args: Vec::new(),
        env: None,
        env_vars: Vec::new(),
        cwd: None,
    };

    assert_eq!(transport_origin(&transport), Some("stdio".to_string()));
}

#[test]
fn root_uri_from_cwd_escapes_spaces_and_unicode() {
    let uri = root_uri_from_cwd(Path::new("/tmp/bsd café"));
    assert_eq!(uri, "file:///tmp/bsd%20caf%C3%A9/");
}

#[tokio::test]
async fn notify_roots_changed_does_not_block_while_client_is_starting() {
    let pending_client = futures::future::pending::<Result<ManagedClient, StartupOutcomeError>>()
        .boxed()
        .shared();
    let cwd = Arc::new(StdRwLock::new(PathBuf::from("/before")));
    let async_client = AsyncManagedClient {
        client: pending_client,
        startup_snapshot: None,
        startup_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        cwd: Arc::clone(&cwd),
    };

    tokio::time::timeout(
        Duration::from_millis(10),
        async_client.notify_roots_changed(Path::new("/after")),
    )
    .await
    .expect("notify_roots_changed should not block during startup")
    .expect("updating cwd should succeed");

    let current_cwd = cwd
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(current_cwd, PathBuf::from("/after"));
}
