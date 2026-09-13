use super::*;

#[test]
fn explicit_missing_cli_path_is_distinguishable() {
    let config = ClampConfig {
        cli_path: Some(PathBuf::from("/definitely/not/a/real/claude-code-binary")),
        ..Default::default()
    };

    assert!(matches!(
        find_claude_cli(&config),
        Err(ClampError::CliNotFound(_))
    ));
}

#[test]
fn authentication_failures_are_classified_without_exposing_stderr() {
    assert!(text_indicates_auth_failure(
        "Not logged in. Please run claude auth login."
    ));
    assert!(text_indicates_auth_failure(
        "Authentication failed: invalid OAuth token"
    ));
    assert!(!text_indicates_auth_failure(
        "unexpected control protocol response"
    ));
}

#[test]
fn failed_result_detection_preserves_legacy_success_messages() {
    assert!(result_indicates_error(Some("error_during_execution"), true));
    assert!(result_indicates_error(
        Some("error_during_execution"),
        false
    ));
    assert!(!result_indicates_error(Some("success"), false));
    assert!(!result_indicates_error(None, false));
}

#[test]
fn default_tool_permission_allow_preserves_input() {
    let input = serde_json::json!({"command": "ls"});
    let value = default_tool_permission_response(true, input.clone(), None);
    assert_eq!(value["behavior"], "allow");
    assert_eq!(value["updatedInput"], input);
}

#[test]
fn default_tool_permission_deny_sets_message() {
    let value = default_tool_permission_response(false, Value::Null, Some("nope".to_string()));
    assert_eq!(value["behavior"], "deny");
    assert_eq!(value["message"], "nope");
}

#[test]
fn default_mcp_error_reuses_jsonrpc_id() {
    let request = serde_json::json!({"id": 42, "method": "tools/call"});
    let response = default_mcp_error_response("chaos", &request);
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 42);
    assert_eq!(response["error"]["code"], -32601);
}

#[test]
fn control_response_request_id_extracts_id() {
    let msg = Message::ControlResponse {
        response: serde_json::json!({
            "request_id": "req_123",
            "subtype": "success"
        }),
    };
    assert_eq!(control_response_request_id(&msg), Some("req_123"));
}

#[test]
fn build_command_no_bare_mode_by_default() {
    // bare_mode is false by default; keychain auth must remain accessible
    // for Claude Code MAX OAuth to work.
    let config = ClampConfig::default();
    let command = build_command(&PathBuf::from("claude"), &config);
    let args: Vec<_> = command
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(!args.iter().any(|arg| arg == "--bare"));
    // settings files are still blocked
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--setting-sources" && w[1].is_empty())
    );
}

#[test]
fn build_command_sets_anthropic_base_url_when_configured() {
    let config = ClampConfig {
        anthropic_base_url: Some("http://127.0.0.1:4567".to_string()),
        ..Default::default()
    };
    let command = build_command(&PathBuf::from("claude"), &config);
    let envs: Vec<_> = command
        .as_std()
        .get_envs()
        .filter_map(|(k, v)| Some((k.to_str()?, v?.to_str()?.to_owned())))
        .collect();
    assert!(
        envs.iter()
            .any(|(k, v)| *k == "ANTHROPIC_BASE_URL" && v == "http://127.0.0.1:4567"),
        "ANTHROPIC_BASE_URL not set: {envs:?}"
    );
}

#[test]
fn build_command_omits_anthropic_base_url_by_default() {
    let config = ClampConfig::default();
    let command = build_command(&PathBuf::from("claude"), &config);
    let has_base_url = command
        .as_std()
        .get_envs()
        .any(|(k, _)| k.to_str() == Some("ANTHROPIC_BASE_URL"));
    assert!(!has_base_url, "ANTHROPIC_BASE_URL must be unset by default");
}

#[test]
fn build_command_includes_disallowed_tools() {
    let config = ClampConfig {
        disallowed_tools: vec!["Bash".to_string(), "Read".to_string()],
        // disallowed_tools only takes effect when CC tools are permitted
        allow_claude_code_tools: true,
        ..Default::default()
    };
    let command = build_command(&PathBuf::from("claude"), &config);
    let args: Vec<_> = command
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        args.windows(2)
            .any(|window| { window[0] == "--disallowedTools" && window[1] == "Bash,Read" })
    );
}

#[test]
fn build_command_includes_allowed_tools_when_builtin_tools_disabled() {
    let config = ClampConfig {
        allowed_tools: vec!["mcp__chaos__*".to_string()],
        ..Default::default()
    };
    let command = build_command(&PathBuf::from("claude"), &config);
    let args: Vec<_> = command
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    assert!(
        args.windows(2)
            .any(|window| { window[0] == "--tools" && window[1].is_empty() }),
        "built-in tools must stay disabled: {args:?}"
    );
    assert!(
        args.windows(2)
            .any(|window| { window[0] == "--allowedTools" && window[1] == "mcp__chaos__*" }),
        "MCP bridge allow rule must be passed through: {args:?}"
    );
}
