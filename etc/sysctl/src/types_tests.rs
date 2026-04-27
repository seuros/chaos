use super::*;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::path::PathBuf;

fn deserialize_server_config(input: &str) -> Result<McpServerConfig, toml::de::Error> {
    toml::from_str(input)
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn string_map(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

fn stdio_transport(
    args: &[&str],
    env: Option<&[(&str, &str)]>,
    env_vars: &[&str],
    cwd: Option<&str>,
) -> McpServerTransportConfig {
    McpServerTransportConfig::Stdio {
        command: "echo".to_string(),
        args: strings(args),
        env: env.map(string_map),
        env_vars: strings(env_vars),
        cwd: cwd.map(PathBuf::from),
    }
}

fn streamable_http_transport(
    bearer_token: Option<&str>,
    bearer_token_env_var: Option<&str>,
    http_headers: Option<&[(&str, &str)]>,
    env_http_headers: Option<&[(&str, &str)]>,
) -> McpServerTransportConfig {
    McpServerTransportConfig::StreamableHttp {
        url: "https://example.com/mcp".to_string(),
        bearer_token: bearer_token.map(str::to_string),
        bearer_token_env_var: bearer_token_env_var.map(str::to_string),
        http_headers: http_headers.map(string_map),
        env_http_headers: env_http_headers.map(string_map),
    }
}

struct SuccessfulServerCase {
    name: &'static str,
    input: &'static str,
    expected_transport: McpServerTransportConfig,
    expected_enabled: bool,
    expected_required: bool,
    expected_enabled_tools: Option<Vec<String>>,
    expected_disabled_tools: Option<Vec<String>>,
    expected_oauth_resource: Option<&'static str>,
}

fn assert_successful_server_case(case: SuccessfulServerCase) {
    let cfg = deserialize_server_config(case.input)
        .unwrap_or_else(|err| panic!("{} should deserialize: {err}", case.name));

    assert_eq!(
        cfg.transport, case.expected_transport,
        "case: {}",
        case.name
    );
    assert_eq!(cfg.enabled, case.expected_enabled, "case: {}", case.name);
    assert_eq!(cfg.required, case.expected_required, "case: {}", case.name);
    assert_eq!(
        cfg.enabled_tools, case.expected_enabled_tools,
        "case: {}",
        case.name
    );
    assert_eq!(
        cfg.disabled_tools, case.expected_disabled_tools,
        "case: {}",
        case.name
    );
    assert_eq!(
        cfg.oauth_resource.as_deref(),
        case.expected_oauth_resource,
        "case: {}",
        case.name
    );
}

struct RejectedServerCase {
    name: &'static str,
    input: &'static str,
    expected_message: Option<&'static str>,
}

fn assert_rejected_server_case(case: RejectedServerCase) {
    let err = deserialize_server_config(case.input)
        .expect_err("server config should reject invalid transport fields");

    if let Some(expected_message) = case.expected_message {
        assert!(
            err.to_string().contains(expected_message),
            "unexpected error for {}: {err}",
            case.name
        );
    }
}

#[test]
fn deserialize_stdio_command_server_config_variants() {
    for case in [
        SuccessfulServerCase {
            name: "default stdio",
            input: r#"
                command = "echo"
            "#,
            expected_transport: stdio_transport(&[], None, &[], None),
            expected_enabled: true,
            expected_required: false,
            expected_enabled_tools: None,
            expected_disabled_tools: None,
            expected_oauth_resource: None,
        },
        SuccessfulServerCase {
            name: "stdio with args",
            input: r#"
                command = "echo"
                args = ["hello", "world"]
            "#,
            expected_transport: stdio_transport(&["hello", "world"], None, &[], None),
            expected_enabled: true,
            expected_required: false,
            expected_enabled_tools: None,
            expected_disabled_tools: None,
            expected_oauth_resource: None,
        },
        SuccessfulServerCase {
            name: "stdio with args and env",
            input: r#"
                command = "echo"
                args = ["hello", "world"]
                env = { "FOO" = "BAR" }
            "#,
            expected_transport: stdio_transport(
                &["hello", "world"],
                Some(&[("FOO", "BAR")]),
                &[],
                None,
            ),
            expected_enabled: true,
            expected_required: false,
            expected_enabled_tools: None,
            expected_disabled_tools: None,
            expected_oauth_resource: None,
        },
        SuccessfulServerCase {
            name: "stdio with env vars",
            input: r#"
                command = "echo"
                env_vars = ["FOO", "BAR"]
            "#,
            expected_transport: stdio_transport(&[], None, &["FOO", "BAR"], None),
            expected_enabled: true,
            expected_required: false,
            expected_enabled_tools: None,
            expected_disabled_tools: None,
            expected_oauth_resource: None,
        },
        SuccessfulServerCase {
            name: "stdio with cwd",
            input: r#"
                command = "echo"
                cwd = "/tmp"
            "#,
            expected_transport: stdio_transport(&[], None, &[], Some("/tmp")),
            expected_enabled: true,
            expected_required: false,
            expected_enabled_tools: None,
            expected_disabled_tools: None,
            expected_oauth_resource: None,
        },
    ] {
        assert_successful_server_case(case);
    }
}

#[test]
fn deserialize_disabled_server_config() {
    let cfg: McpServerConfig = deserialize_server_config(
        r#"
            command = "echo"
            enabled = false
        "#,
    )
    .expect("should deserialize disabled server config");

    assert!(!cfg.enabled);
    assert!(!cfg.required);
}

#[test]
fn deserialize_required_server_config() {
    let cfg: McpServerConfig = deserialize_server_config(
        r#"
            command = "echo"
            required = true
        "#,
    )
    .expect("should deserialize required server config");

    assert!(cfg.required);
}

#[test]
fn deserialize_streamable_http_server_config_variants() {
    for case in [
        SuccessfulServerCase {
            name: "default streamable http",
            input: r#"
                url = "https://example.com/mcp"
            "#,
            expected_transport: streamable_http_transport(None, None, None, None),
            expected_enabled: true,
            expected_required: false,
            expected_enabled_tools: None,
            expected_disabled_tools: None,
            expected_oauth_resource: None,
        },
        SuccessfulServerCase {
            name: "streamable http with bearer token",
            input: r#"
                url = "https://example.com/mcp"
                bearer_token = "super-secret"
            "#,
            expected_transport: streamable_http_transport(Some("super-secret"), None, None, None),
            expected_enabled: true,
            expected_required: false,
            expected_enabled_tools: None,
            expected_disabled_tools: None,
            expected_oauth_resource: None,
        },
        SuccessfulServerCase {
            name: "streamable http with bearer token env var",
            input: r#"
                url = "https://example.com/mcp"
                bearer_token_env_var = "GITHUB_TOKEN"
            "#,
            expected_transport: streamable_http_transport(None, Some("GITHUB_TOKEN"), None, None),
            expected_enabled: true,
            expected_required: false,
            expected_enabled_tools: None,
            expected_disabled_tools: None,
            expected_oauth_resource: None,
        },
        SuccessfulServerCase {
            name: "streamable http with headers",
            input: r#"
                url = "https://example.com/mcp"
                http_headers = { "X-Foo" = "bar" }
                env_http_headers = { "X-Token" = "TOKEN_ENV" }
            "#,
            expected_transport: streamable_http_transport(
                None,
                None,
                Some(&[("X-Foo", "bar")]),
                Some(&[("X-Token", "TOKEN_ENV")]),
            ),
            expected_enabled: true,
            expected_required: false,
            expected_enabled_tools: None,
            expected_disabled_tools: None,
            expected_oauth_resource: None,
        },
        SuccessfulServerCase {
            name: "streamable http with oauth resource",
            input: r#"
                url = "https://example.com/mcp"
                oauth_resource = "https://api.example.com"
            "#,
            expected_transport: streamable_http_transport(None, None, None, None),
            expected_enabled: true,
            expected_required: false,
            expected_enabled_tools: None,
            expected_disabled_tools: None,
            expected_oauth_resource: Some("https://api.example.com"),
        },
    ] {
        assert_successful_server_case(case);
    }
}

#[test]
fn deserialize_server_config_with_tool_filters() {
    let cfg: McpServerConfig = deserialize_server_config(
        r#"
            command = "echo"
            enabled_tools = ["allowed"]
            disabled_tools = ["blocked"]
        "#,
    )
    .expect("should deserialize tool filters");

    assert_eq!(cfg.enabled_tools, Some(vec!["allowed".to_string()]));
    assert_eq!(cfg.disabled_tools, Some(vec!["blocked".to_string()]));
}

#[test]
fn deserialize_rejects_invalid_transport_fields() {
    for case in [
        RejectedServerCase {
            name: "command and url",
            input: r#"
                command = "echo"
                url = "https://example.com"
            "#,
            expected_message: None,
        },
        RejectedServerCase {
            name: "http transport with env",
            input: r#"
                url = "https://example.com"
                env = { "FOO" = "BAR" }
            "#,
            expected_message: None,
        },
        RejectedServerCase {
            name: "stdio with http headers",
            input: r#"
                command = "echo"
                http_headers = { "X-Foo" = "bar" }
            "#,
            expected_message: None,
        },
        RejectedServerCase {
            name: "stdio with env http headers",
            input: r#"
                command = "echo"
                env_http_headers = { "X-Foo" = "BAR_ENV" }
            "#,
            expected_message: None,
        },
        RejectedServerCase {
            name: "stdio with oauth resource",
            input: r#"
                command = "echo"
                oauth_resource = "https://api.example.com"
            "#,
            expected_message: Some("oauth_resource is not supported for stdio"),
        },
        RejectedServerCase {
            name: "http transport with both bearer token sources",
            input: r#"
                url = "https://example.com"
                bearer_token = "secret"
                bearer_token_env_var = "TOKEN_ENV"
            "#,
            expected_message: Some("bearer_token and bearer_token_env_var cannot both be set"),
        },
    ] {
        assert_rejected_server_case(case);
    }
}

#[test]
fn json_round_trip_preserves_shared_client_fields() {
    let cfg: McpServerConfig = serde_json::from_str(
        r#"
        {
            "url": "https://example.com/mcp",
            "type": "streamable_http",
            "oauth": {
                "client_id": "shared-client"
            }
        }
        "#,
    )
    .expect("should deserialize shared-client fields from json");

    assert_eq!(cfg.r#type.as_deref(), Some("streamable_http"));
    assert_eq!(
        cfg.oauth,
        Some(serde_json::json!({
            "client_id": "shared-client"
        }))
    );

    let rendered = serde_json::to_value(&cfg).expect("should serialize shared-client fields");
    assert_eq!(
        rendered.get("type"),
        Some(&serde_json::json!("streamable_http"))
    );
    assert_eq!(
        rendered.get("oauth"),
        Some(&serde_json::json!({
            "client_id": "shared-client"
        }))
    );
}
