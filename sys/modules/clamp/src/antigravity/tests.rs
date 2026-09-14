use super::*;

fn test_bridge(dir: &Path) -> AntigravityBridgeConfig {
    AntigravityBridgeConfig {
        socket_path: dir.join("bridge.sock"),
        token: "ephemeral-test-token".to_string(),
        chaos_executable: dir.join("chaos"),
    }
}

const OBSERVED_STREAM: &[u8] = br#"{"event":"init","conversation_id":"conversation-1","init":{"model":"gemini-3.1-pro-low","permission_mode":"request-review","tools":["run_command"]}}
{"event":"step_update","step_update":{"conversation_id":"conversation-1","step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"hello\n","usage":{"input_tokens":11,"output_tokens":7,"thinking_tokens":5,"cache_read_tokens":3,"total_tokens":26}}}
{"event":"step_update","step_update":{"conversation_id":"conversation-1","step_index":4,"state":"DONE","step_type":"tool","tool_name":"call_mcp_tool","tool_info":{"parameters":{"ServerName":"chaos","ToolName":"read_file"},"output":"ok"}}}
{"event":"step_update","step_update":{"conversation_id":"conversation-1","step_index":5,"state":"DONE","step_type":"future_checkpoint_kind"}}
{"event":"result","result":{"conversation_id":"conversation-1","status":"SUCCESS","response":"hello\n","duration_seconds":1.5,"num_turns":1,"usage":{"input_tokens":11,"output_tokens":7,"thinking_tokens":5,"cache_read_tokens":3,"total_tokens":26}}}
"#;

/// One realistic stream covers init, text, tool, forward-compatible step
/// kinds, and the terminal result — and proves each is forwarded to the
/// sink before the invocation ends.
#[tokio::test]
async fn streams_observed_events_to_the_sink_as_they_parse() {
    let (tx, mut rx) = mpsc::channel(16);
    let events = read_events(OBSERVED_STREAM, Some(&tx))
        .await
        .expect("events should parse");
    drop(tx);

    let mut streamed = Vec::new();
    while let Some(event) = rx.recv().await {
        streamed.push(event);
    }
    assert_eq!(streamed, events);
    assert_eq!(events.len(), 5);

    let AntigravityEvent::StepUpdate { step_update } = &events[1] else {
        panic!("expected text step");
    };
    assert_eq!(step_update.text_delta.as_deref(), Some("hello\n"));
    let AntigravityEvent::StepUpdate { step_update } = &events[2] else {
        panic!("expected tool step");
    };
    assert_eq!(step_update.tool_name.as_deref(), Some("call_mcp_tool"));
    assert_eq!(
        step_update.tool_info.as_ref().expect("tool info")["output"],
        "ok"
    );
    let AntigravityEvent::StepUpdate { step_update } = &events[3] else {
        panic!("expected unknown step");
    };
    assert_eq!(step_update.step_type, "future_checkpoint_kind");
    let AntigravityEvent::Result { result } = &events[4] else {
        panic!("expected result event");
    };
    assert_eq!(result.response, "hello\n");
    assert_eq!(result.usage.as_ref().expect("usage").thinking_tokens, 5);
}

#[tokio::test]
async fn rejects_stdout_beyond_the_stream_limit() {
    let oversized = vec![b'x'; usize::try_from(STDOUT_LIMIT).expect("limit fits usize") + 1];
    let error = read_events(oversized.as_slice(), None)
        .await
        .expect_err("oversized stdout should be rejected");
    assert!(matches!(error, AntigravityError::Protocol(_)), "{error}");
}

#[test]
fn rejects_unknown_effort_and_unsafe_resume_ids() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let base = AntigravityConfig {
        home: Some(directory.path().join("home")),
        bridge: Some(test_bridge(directory.path())),
        ..Default::default()
    };
    assert!(matches!(
        AntigravityTransport::new(AntigravityConfig {
            effort: Some("ultra".to_string()),
            ..base.clone()
        }),
        Err(AntigravityError::Protocol(_))
    ));
    assert!(matches!(
        AntigravityTransport::with_conversation_id(base, "../other-session".to_string()),
        Err(AntigravityError::Protocol(_))
    ));
}

#[test]
fn auth_errors_are_classified_without_returning_diagnostics() {
    assert!(text_indicates_auth_failure(
        "Authentication failed: OAuth token expired"
    ));
    assert!(!text_indicates_auth_failure("model backend unavailable"));
    assert!(!text_indicates_auth_failure(
        "the sign in button was not found on the page"
    ));
}

#[cfg(unix)]
#[test]
fn non_executable_cli_paths_are_rejected() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let script = directory.path().join("agy");
    std::fs::write(&script, "#!/bin/sh\n").expect("write fake agy");
    let config = AntigravityConfig {
        cli_path: Some(script),
        home: Some(directory.path().join("home")),
        bridge: Some(test_bridge(directory.path())),
        ..Default::default()
    };
    assert!(matches!(
        find_agy_cli(&config),
        Err(AntigravityError::CliNotFound(_))
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn transport_runs_fresh_then_resumed_turn_without_api_keys() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary directory");
    let dir = directory.path();
    let script = dir.join("agy");
    std::fs::write(
        &script,
        r#"#!/bin/sh
case "$*" in
  *--dangerously-skip-permissions*) exit 90 ;;
esac
for argument in "$@"; do
  case "$argument" in
    --print|--print=*) exit 93 ;;
  esac
done
case "$*" in
  *"--input-format stream-json"*) ;;
  *) exit 93 ;;
esac
if [ -n "${GEMINI_API_KEY+x}" ] || [ -n "${GOOGLE_API_KEY+x}" ]; then
  exit 91
fi
case "$CHAOS_CLAMP_MCP_SOCKET" in
  */bridge.sock) ;;
  *) exit 92 ;;
esac
if [ "$CHAOS_CLAMP_MCP_TOKEN" != "ephemeral-test-token" ]; then
  exit 92
fi
cat >> "$HOME/inputs.jsonl"
case "$*" in
  *"--conversation conversation-1"*)
    turns=2
    response=second
    ;;
  *)
    turns=1
    response=first
    ;;
esac
printf '{"event":"init","conversation_id":"conversation-1","init":{"model":"gemini-test","permission_mode":"request-review"}}\n'
printf '{"event":"step_update","step_update":{"conversation_id":"conversation-1","step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"%s"}}\n' "$response"
printf '{"event":"result","result":{"conversation_id":"conversation-1","status":"SUCCESS","response":"%s","num_turns":%s,"usage":{"input_tokens":1,"output_tokens":2,"thinking_tokens":3,"cache_read_tokens":4,"total_tokens":10}}}\n' "$response" "$turns"
"#,
    )
    .expect("write fake agy");
    let mut permissions = std::fs::metadata(&script)
        .expect("fake agy metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&script, permissions).expect("make fake agy executable");

    let config = AntigravityConfig {
        cli_path: Some(script),
        home: Some(dir.join("home")),
        cwd: Some(dir.to_path_buf()),
        model: "gemini-test".to_string(),
        print_timeout: Duration::from_secs(5),
        bridge: Some(test_bridge(dir)),
        ..Default::default()
    };
    let mut transport = AntigravityTransport::new(config).expect("create transport");

    let (tx, mut rx) = mpsc::channel(16);
    let first_prompt = "x".repeat(150_000);
    let fresh = transport
        .run_turn_streamed(&first_prompt, Some(&tx))
        .await
        .expect("fresh turn");
    let resumed = transport
        .run_turn("second prompt")
        .await
        .expect("resumed turn");
    drop(tx);

    assert_eq!(fresh.response, "first");
    assert_eq!(resumed.response, "second");
    assert_eq!(transport.conversation_id(), Some("conversation-1"));
    let AntigravityEvent::Result { result } = resumed.events.last().expect("result event") else {
        panic!("expected result event");
    };
    assert_eq!(result.num_turns, Some(2));

    let input_lines =
        std::fs::read_to_string(dir.join("home/inputs.jsonl")).expect("read captured stdin");
    let messages = input_lines
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("valid stream input"))
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["event"], "user");
    assert_eq!(messages[0]["message"]["content"], first_prompt);
    assert_eq!(messages[1]["event"], "user");
    assert_eq!(messages[1]["message"]["content"], "second prompt");

    let mut streamed = Vec::new();
    while let Some(event) = rx.recv().await {
        streamed.push(event);
    }
    assert_eq!(streamed.len(), 3, "only the streamed turn feeds the sink");
}

#[test]
fn command_removes_metered_api_keys_and_never_auto_approves() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let dir = directory.path();
    let config = AntigravityConfig {
        home: Some(dir.join("home")),
        bridge: Some(test_bridge(dir)),
        sandbox: Some(AntigravitySandbox {
            program: PathBuf::from("/usr/lib/chaos/alcatraz"),
            arg0: Some("alcatraz".to_string()),
            args: vec!["--allow-network-for-proxy".to_string(), "--".to_string()],
        }),
        egress: Some(AntigravityEgress {
            proxy_url: "http://127.0.0.1:41234".to_string(),
            ca_bundle_path: Some(dir.join("egress-ca.pem")),
        }),
        ..Default::default()
    };
    let command = build_command(&PathBuf::from("/tmp/agy"), &config, None);
    let std_command = command.as_std();

    // The sandbox helper is what runs; `agy` is an argument to it.
    assert_eq!(
        std_command.get_program().to_string_lossy(),
        "/usr/lib/chaos/alcatraz"
    );
    // A multicall helper dispatches on argv[0]; the standard library has no
    // getter for it, but its `Debug` output brackets the program path and
    // renders an overridden argv[0] as the first argument.
    assert!(
        format!("{std_command:?}").contains("[\"/usr/lib/chaos/alcatraz\"] \"alcatraz\""),
        "helper must be launched under its multicall name: {std_command:?}"
    );
    let args = std_command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(args.iter().any(|arg| arg == "--sandbox"));
    assert!(!args.iter().any(|arg| arg.contains("dangerously")));
    let separator = args
        .iter()
        .position(|arg| arg == "--")
        .expect("sandbox argument separator");
    assert_eq!(args[separator + 1], "/tmp/agy");

    let removed = std_command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(removed.iter().any(|name| name == "GEMINI_API_KEY"));
    assert!(removed.iter().any(|name| name == "GOOGLE_API_KEY"));
    // An inherited NO_PROXY would exempt hosts from the only permitted route.
    assert!(removed.iter().any(|name| name == "NO_PROXY"));
    assert!(removed.iter().any(|name| name == "no_proxy"));
    let envs = std_command
        .get_envs()
        .filter_map(|(name, value)| {
            Some((
                name.to_string_lossy().into_owned(),
                value?.to_string_lossy().into_owned(),
            ))
        })
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        envs.get(BRIDGE_TOKEN_ENV).map(String::as_str),
        Some("ephemeral-test-token")
    );
    assert_eq!(
        envs.get(BRIDGE_SOCKET_ENV).map(String::as_str),
        Some(dir.join("bridge.sock").to_string_lossy().as_ref())
    );
    assert_eq!(
        envs.get("HTTPS_PROXY").map(String::as_str),
        Some("http://127.0.0.1:41234")
    );
    assert_eq!(
        envs.get("SSL_CERT_FILE").map(String::as_str),
        Some(dir.join("egress-ca.pem").to_string_lossy().as_ref())
    );
}

#[test]
fn managed_config_exposes_only_chaos_mcp_without_persisting_capability() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let dir = directory.path();
    let settings_path = dir.join(".gemini/antigravity-cli/settings.json");
    std::fs::create_dir_all(settings_path.parent().expect("settings parent"))
        .expect("create settings parent");
    std::fs::write(
        &settings_path,
        r#"{"theme":"dark","permissions":{"allow":["command(*)"]}}"#,
    )
    .expect("seed settings");

    let config = AntigravityConfig {
        home: Some(dir.to_path_buf()),
        bridge: Some(test_bridge(dir)),
        ..Default::default()
    };
    prepare_managed_home(&config).expect("prepare managed home");

    let mcp_bytes =
        std::fs::read(dir.join(".gemini/config/mcp_config.json")).expect("read MCP config");
    let mcp: Value = serde_json::from_slice(&mcp_bytes).expect("parse MCP config");
    assert_eq!(
        mcp["mcpServers"]["chaos"]["args"],
        serde_json::json!(["clamp-session-bridge"])
    );
    assert_eq!(
        mcp["mcpServers"]["chaos"]["command"],
        dir.join("chaos").to_string_lossy().as_ref()
    );
    assert!(!String::from_utf8_lossy(&mcp_bytes).contains("ephemeral-test-token"));
    assert!(!String::from_utf8_lossy(&mcp_bytes).contains("bridge.sock"));

    let settings_bytes = std::fs::read(&settings_path).expect("read settings");
    let settings: Value = serde_json::from_slice(&settings_bytes).expect("parse settings");
    assert_eq!(settings["theme"], "dark");
    assert_eq!(
        settings["permissions"]["allow"],
        serde_json::json!([CHAOS_MCP_ALLOW_RULE])
    );
    assert_eq!(
        settings["permissions"]["deny"],
        serde_json::json!(NATIVE_TOOL_DENY_RULES)
    );
    assert!(!String::from_utf8_lossy(&settings_bytes).contains("ephemeral-test-token"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [dir.join(".gemini/config/mcp_config.json"), settings_path] {
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("managed config metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}

#[test]
fn conversation_store_round_trips_only_for_matching_model_and_safe_ids() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = AntigravityConversationStore::new(directory.path().join("state/conversation.json"));

    assert_eq!(store.load("gemini-3.1-pro-low"), None);
    store
        .save("gemini-3.1-pro-low", "conversation-123")
        .expect("save conversation state");
    assert_eq!(
        store.load("gemini-3.1-pro-low"),
        Some("conversation-123".to_string())
    );
    assert_eq!(store.load("gemini-3.1-pro-high"), None);

    store
        .save("gemini-3.1-pro-low", "../other-session")
        .expect("save unsafe conversation state");
    assert_eq!(store.load("gemini-3.1-pro-low"), None);

    store.clear();
    assert_eq!(store.load("gemini-3.1-pro-low"), None);
    store.clear();
}
