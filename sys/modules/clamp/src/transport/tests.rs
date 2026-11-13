use super::*;

#[test]
fn native_command_removes_daemon_credentials_after_environment_overrides() {
    let names = [
        "CHAOS_BEARER_TOKEN",
        "CHAOS_RECOVERY_TOKEN",
        "CUSTOM_WEBHOOK_SIGNING_KEY",
    ];
    let config = ClampConfig {
        env_remove: names.iter().map(|name| (*name).into()).collect(),
        ..Default::default()
    };
    let (command, _files) = build_command(&PathBuf::from("claude"), &config).unwrap();
    for name in names {
        assert!(
            command
                .as_std()
                .get_envs()
                .any(|(key, value)| key == name && value.is_none())
        );
    }
}

#[test]
fn build_command_resumes_only_the_explicit_native_session() {
    let config = ClampConfig {
        resume_session_id: Some("2eb242f5-85ad-4407-b9d3-459c32d06d91".into()),
        ..Default::default()
    };
    let (command, _files) = build_command(&PathBuf::from("claude"), &config).unwrap();
    let args: Vec<_> = command.as_std().get_args().collect();
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--resume", "2eb242f5-85ad-4407-b9d3-459c32d06d91"])
    );
    assert!(!args.contains(&std::ffi::OsStr::new("--continue")));
    let (fresh, _files) = build_command(&PathBuf::from("claude"), &ClampConfig::default()).unwrap();
    let args: Vec<_> = fresh.as_std().get_args().collect();
    assert!(
        !args
            .iter()
            .any(|arg| *arg == "--resume" || *arg == "--bare")
    );
    // Keychain OAuth stays available, but user settings and endpoint overrides do not.
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--setting-sources" && pair[1].is_empty())
    );
    assert!(
        !fresh
            .as_std()
            .get_envs()
            .any(|(key, _)| key == "ANTHROPIC_BASE_URL")
    );
}

#[test]
fn explicit_missing_cli_path_is_distinguishable() {
    let config = ClampConfig {
        cli_path: Some(PathBuf::from("/definitely/not/a/real/claude-code-binary")),
        ..Default::default()
    };

    std::assert_matches!(find_claude_cli(&config), Err(ClampError::CliNotFound(_)));
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
fn failed_results_carry_what_claude_code_said() {
    let detail = failed_result_detail(
        &serde_json::json!(
            "Failed to authenticate. API Error: 403 Attention Required! | Cloudflare"
        ),
        &[],
    );
    assert_eq!(
        detail.as_deref(),
        Some("Failed to authenticate. API Error: 403 Attention Required! | Cloudflare")
    );
    // A Cloudflare 403 is an upstream challenge, not a missing login: it
    // must stay a failed turn that says so, not become auth-unavailable.
    assert!(!text_indicates_auth_failure(
        detail.as_deref().unwrap_or_default()
    ));

    let error = ClampError::TurnFailed {
        subtype: Some("error_during_execution".into()),
        detail: Some("overloaded_error".into()),
    };
    assert_eq!(
        error.to_string(),
        "Claude Code reported a failed result (error_during_execution): overloaded_error"
    );
    let bare = ClampError::TurnFailed {
        subtype: None,
        detail: None,
    };
    assert_eq!(bare.to_string(), "Claude Code reported a failed result");
}

#[test]
fn failed_result_detail_falls_back_to_errors_and_strips_pages() {
    assert_eq!(
        failed_result_detail(
            &serde_json::Value::Null,
            &[
                serde_json::json!("API Error: 529 overloaded"),
                serde_json::json!("retry later")
            ]
        )
        .as_deref(),
        Some("API Error: 529 overloaded; retry later")
    );
    let page = "API Error: 403 <!DOCTYPE html><html><head><title>Attention Required! | Cloudflare</title>\
        <style>body{color:red}</style><script>var x=1;</script></head>\
        <body><h1>Sorry, you have been blocked</h1></body></html>";
    assert_eq!(
        failed_result_detail(&serde_json::json!(page), &[]).as_deref(),
        Some("API Error: 403 Attention Required! | Cloudflare Sorry, you have been blocked")
    );
    assert_eq!(failed_result_detail(&serde_json::json!("   "), &[]), None);
}

#[test]
fn failed_result_detail_redacts_and_bounds() {
    // The adapter's own redactor (antigravity::sanitise_diagnostic): a
    // secret-named key loses its whole value, to the end of the line.
    let detail = failed_result_detail(
        &serde_json::json!(
            "Authorization: Bearer abcdefgh12345678\nkey sk-ant-api03-SECRETSECRET\n\
             at https://api.example.com/v1?key=hunter2"
        ),
        &[],
    )
    .expect("detail");
    assert!(!detail.contains("abcdefgh12345678"), "{detail}");
    assert!(!detail.contains("SECRETSECRET"), "{detail}");
    assert!(!detail.contains("hunter2"), "{detail}");
    assert!(
        detail.contains("https://api.example.com/v1?[redacted]"),
        "{detail}"
    );

    let long = "word ".repeat(400);
    let detail = failed_result_detail(&serde_json::json!(long), &[]).expect("detail");
    assert_eq!(detail.chars().count(), FAILED_RESULT_DETAIL_LIMIT);
    assert!(detail.ends_with('…'));
}

#[test]
fn default_tool_permission_responses_preserve_input_or_explain_denial() {
    let input = serde_json::json!({"command": "ls"});
    for (allow, message, expected) in [
        (
            true,
            None,
            serde_json::json!({"behavior": "allow", "updatedInput": input}),
        ),
        (
            false,
            Some("nope".to_string()),
            serde_json::json!({"behavior": "deny", "message": "nope"}),
        ),
        (
            false,
            None,
            serde_json::json!({"behavior": "deny", "message": "tool use denied"}),
        ),
    ] {
        assert_eq!(
            default_tool_permission_response(allow, input.clone(), message),
            expected
        );
    }
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
fn build_command_passes_endpoint_and_tool_restrictions() {
    let config = ClampConfig {
        anthropic_base_url: Some("http://127.0.0.1:4567".to_string()),
        disallowed_tools: vec!["Bash".to_string(), "Read".to_string()],
        // disallowed_tools only takes effect when CC tools are permitted.
        allow_claude_code_tools: true,
        ..Default::default()
    };
    let (command, _config_files) =
        build_command(&PathBuf::from("claude"), &config).expect("build command");
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
    let (command, _config_files) =
        build_command(&PathBuf::from("claude"), &config).expect("build command");
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

#[test]
fn config_files_keep_private_contents_out_of_argv() {
    let prompt = "private-prompt: '\" $HOME 🦀\n".repeat(16_384);
    let mcp = serde_json::json!({
        "mcpServers": {"chaos": {"env": {"CHAOS_CLAMP_MCP_TOKEN": "private-token"}}}
    });
    let config = ClampConfig {
        system_prompt: Some(prompt.clone()),
        mcp_config: Some(mcp.clone()),
        ..Default::default()
    };
    let (command, files) = build_command(&PathBuf::from("claude"), &config).expect("build command");
    let args: Vec<_> = command.as_std().get_args().collect();
    assert!(!args.iter().any(|arg| *arg == "--system-prompt"));
    assert!(
        args.iter()
            .all(|arg| !arg.to_string_lossy().contains("private-"))
    );

    let paths = [
        ("--system-prompt-file", files.system_prompt.as_ref(), prompt),
        ("--mcp-config", files.mcp.as_ref(), mcp.to_string()),
    ]
    .map(|(flag, file, contents)| {
        let path = file.expect("config file").to_path_buf();
        assert!(path.is_absolute());
        assert!(
            args.windows(2)
                .any(|window| window[0] == flag && window[1] == path.as_os_str())
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read config"),
            contents
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::metadata(&path).expect("metadata").permissions();
            assert_eq!(permissions.mode() & 0o777, 0o600);
        }
        path
    });

    drop(files);
    assert!(paths.iter().all(|path| !path.exists()));
}

#[test]
fn empty_configuration_preserves_blank_prompt_without_files() {
    for system_prompt in [None, Some(String::new())] {
        let config = ClampConfig {
            system_prompt,
            ..Default::default()
        };
        let (command, files) =
            build_command(&PathBuf::from("claude"), &config).expect("build command");
        let args: Vec<_> = command.as_std().get_args().collect();

        assert!(files.system_prompt.is_none() && files.mcp.is_none());
        assert!(
            args.windows(2)
                .any(|window| window[0] == "--system-prompt" && window[1].is_empty())
        );
        assert!(
            !args
                .iter()
                .any(|arg| *arg == "--system-prompt-file" || *arg == "--mcp-config")
        );
    }
}
