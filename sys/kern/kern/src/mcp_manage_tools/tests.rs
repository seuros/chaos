use tempfile::tempdir;

use super::*;

#[test]
fn add_server_creates_dot_mcp_json() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join(".mcp.json");
    let result = execute_add_server(
        &path,
        McpAddServerParams {
            name: "docs".to_string(),
            command: Some("node".to_string()),
            args: Some(vec!["server.js".to_string()]),
            env: None,
            url: None,
            bearer_token_env_var: None,
            http_headers: None,
            enabled: Some(true),
            required: Some(false),
        },
    )
    .expect("add server");

    assert_eq!(result["status"], "added");
    assert_eq!(result["server"], "docs");
    let doc = load_dot_mcp_json(&path).expect("reload file");
    assert!(doc.mcp_servers.contains_key("docs"));
}

#[test]
fn server_action_updates_enabled_flag() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join(".mcp.json");
    execute_add_server(
        &path,
        McpAddServerParams {
            name: "docs".to_string(),
            command: Some("node".to_string()),
            args: None,
            env: None,
            url: None,
            bearer_token_env_var: None,
            http_headers: None,
            enabled: Some(true),
            required: Some(false),
        },
    )
    .expect("seed server");

    execute_server_action(
        &path,
        McpServerActionParams {
            name: "docs".to_string(),
            action: "disable".to_string(),
        },
    )
    .expect("disable");

    let doc = load_dot_mcp_json(&path).expect("reload file");
    assert!(!doc.mcp_servers["docs"].enabled);
}

#[test]
fn add_server_rejects_invalid_transport_instead_of_panicking() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join(".mcp.json");
    let err = execute_add_server(
        &path,
        McpAddServerParams {
            name: "docs".to_string(),
            command: Some("node".to_string()),
            args: None,
            env: None,
            url: Some("https://example.com/mcp".to_string()),
            bearer_token_env_var: None,
            http_headers: None,
            enabled: Some(true),
            required: Some(false),
        },
    )
    .expect_err("mixed transports should fail");

    assert!(err.contains("provide either `command` or `url`, not both"));
}

#[test]
fn add_http_server_persists_http_headers() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join(".mcp.json");
    execute_add_server(
        &path,
        McpAddServerParams {
            name: "docs".to_string(),
            command: None,
            args: None,
            env: None,
            url: Some("https://example.com/mcp".to_string()),
            bearer_token_env_var: None,
            http_headers: Some(BTreeMap::from([(
                "Authorization".to_string(),
                "Bearer token".to_string(),
            )])),
            enabled: Some(true),
            required: Some(false),
        },
    )
    .expect("add http server");

    let doc = load_dot_mcp_json(&path).expect("reload file");
    let server = &doc.mcp_servers["docs"];
    match &server.transport {
        McpServerTransportConfig::StreamableHttp { http_headers, .. } => {
            assert_eq!(
                http_headers
                    .as_ref()
                    .and_then(|headers| headers.get("Authorization")),
                Some(&"Bearer token".to_string())
            );
        }
        other => panic!("expected streamable http server, got {other:?}"),
    }
}
