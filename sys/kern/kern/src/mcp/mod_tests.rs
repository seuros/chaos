use super::*;

use chaos_ipc::mcp::Tool;
use pretty_assertions::assert_eq;

fn make_tool(name: &str) -> Tool {
    Tool {
        name: name.to_string(),
        title: None,
        description: None,
        input_schema: serde_json::json!({"type": "object", "properties": {}}),
        output_schema: None,
        annotations: None,
        icons: None,
        meta: None,
    }
}

#[test]
fn split_qualified_tool_name_returns_server_and_tool() {
    assert_eq!(
        split_qualified_tool_name("mcp__alpha__do_thing"),
        Some(("alpha".to_string(), "do_thing".to_string()))
    );
}

#[test]
fn split_qualified_tool_name_rejects_invalid_names() {
    assert_eq!(split_qualified_tool_name("other__alpha__do_thing"), None);
    assert_eq!(split_qualified_tool_name("mcp__alpha__"), None);
}

#[test]
fn group_tools_by_server_strips_prefix_and_groups() {
    let mut tools = HashMap::new();
    tools.insert("mcp__alpha__do_thing".to_string(), make_tool("do_thing"));
    tools.insert(
        "mcp__alpha__nested__op".to_string(),
        make_tool("nested__op"),
    );
    tools.insert("mcp__beta__do_other".to_string(), make_tool("do_other"));

    let mut expected_alpha = HashMap::new();
    expected_alpha.insert("do_thing".to_string(), make_tool("do_thing"));
    expected_alpha.insert("nested__op".to_string(), make_tool("nested__op"));

    let mut expected_beta = HashMap::new();
    expected_beta.insert("do_other".to_string(), make_tool("do_other"));

    let mut expected = HashMap::new();
    expected.insert("alpha".to_string(), expected_alpha);
    expected.insert("beta".to_string(), expected_beta);

    assert_eq!(group_tools_by_server(&tools), expected);
}

#[test]
fn codex_apps_mcp_url_for_base_url_keeps_existing_paths() {
    assert_eq!(
        codex_apps_mcp_url_for_base_url("https://chatgpt.com/backend-api"),
        "https://chatgpt.com/backend-api/wham/apps"
    );
    assert_eq!(
        codex_apps_mcp_url_for_base_url("https://chat.openai.com"),
        "https://chat.openai.com/backend-api/wham/apps"
    );
    assert_eq!(
        codex_apps_mcp_url_for_base_url("http://localhost:8080/api/chaos"),
        "http://localhost:8080/api/chaos/apps"
    );
    assert_eq!(
        codex_apps_mcp_url_for_base_url("http://localhost:8080"),
        "http://localhost:8080/api/chaos/apps"
    );
}

#[test]
fn codex_apps_mcp_url_uses_legacy_codex_apps_path() {
    let mut config = crate::config::test_config();
    config.chatgpt_base_url = "https://chatgpt.com".to_string();

    assert_eq!(
        codex_apps_mcp_url(&config),
        "https://chatgpt.com/backend-api/wham/apps"
    );
}

/// Test-local stand-in after the global constant was removed.
const CODEX_APPS_MCP_SERVER_NAME: &str = "test-apps-server";

#[test]
fn codex_apps_server_config_uses_legacy_codex_apps_path() {
    let mut config = crate::config::test_config();
    config.chatgpt_base_url = "https://chatgpt.com".to_string();

    let mut servers = with_codex_apps_mcp(HashMap::new(), false, None, &config);
    assert!(!servers.contains_key(CODEX_APPS_MCP_SERVER_NAME));

    servers = with_codex_apps_mcp(servers, true, None, &config);
    let server = servers
        .get(CODEX_APPS_MCP_SERVER_NAME)
        .expect("chaos apps should be present when apps is enabled");
    let url = match &server.transport {
        McpServerTransportConfig::StreamableHttp { url, .. } => url,
        _ => panic!("expected streamable http transport for chaos apps"),
    };

    assert_eq!(url, "https://chatgpt.com/backend-api/wham/apps");
}

#[tokio::test]
async fn effective_mcp_servers_returns_configured_servers() {
    let mut config = crate::config::test_config();
    let mut configured_servers = config.mcp_servers.get().clone();
    configured_servers.insert(
        "sample".to_string(),
        McpServerConfig {
            transport: McpServerTransportConfig::StreamableHttp {
                url: "https://user.example/mcp".to_string(),
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
    );
    config
        .mcp_servers
        .set(configured_servers)
        .expect("test config should accept MCP servers");

    let mcp_manager = McpManager::new();
    let effective = mcp_manager.effective_servers(&config);

    let sample = effective.get("sample").expect("user server should exist");

    match &sample.transport {
        McpServerTransportConfig::StreamableHttp { url, .. } => {
            assert_eq!(url, "https://user.example/mcp");
        }
        other => panic!("expected streamable http transport, got {other:?}"),
    }
}
