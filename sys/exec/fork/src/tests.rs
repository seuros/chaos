use super::*;
use chaos_ipc::protocol::ErrorEvent;
use chaos_ipc::protocol::McpStartupStatus;
use chaos_ipc::protocol::McpStartupUpdateEvent;
use chaos_ipc::protocol::TurnCompleteEvent;
use chaos_snitch::set_parent_from_w3c_trace_context;
use pretty_assertions::assert_eq;
use rama::telemetry::opentelemetry::sdk::trace::SdkTracerProvider;
use rama::telemetry::opentelemetry::trace::TraceContextExt;
use rama::telemetry::opentelemetry::trace::TraceId;
use rama::telemetry::opentelemetry::trace::TracerProvider as _;
use serde_json::json;
use tracing_opentelemetry::OpenTelemetrySpanExt;

fn test_tracing_subscriber() -> impl tracing::Subscriber + Send + Sync {
    let provider = SdkTracerProvider::builder().build();
    let tracer = provider.tracer("chaos-exec-tests");
    tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer))
}

#[test]
fn exec_defaults_analytics_to_enabled() {
    assert_eq!(DEFAULT_ANALYTICS_ENABLED, true);
}

#[test]
fn exec_root_span_can_be_parented_from_trace_context() {
    let subscriber = test_tracing_subscriber();
    let _guard = tracing::subscriber::set_default(subscriber);

    let parent = chaos_ipc::protocol::W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000077-0000000000000088-01".into()),
        tracestate: Some("vendor=value".into()),
    };
    let exec_span = exec_root_span();
    assert!(set_parent_from_w3c_trace_context(&exec_span, &parent));

    let trace_id = exec_span.context().span().span_context().trace_id();
    assert_eq!(
        trace_id,
        TraceId::from_hex("00000000000000000000000000000077").expect("trace id")
    );
}

#[test]
fn exec_cli_preparation_covers_config_features_without_starting_a_session() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let cwd = temp.path().join("cwd");
    let extra_a = temp.path().join("extra-a");
    let extra_b = temp.path().join("extra-b");
    std::fs::create_dir_all(&cwd)?;
    std::fs::create_dir_all(&extra_a)?;
    std::fs::create_dir_all(&extra_b)?;
    let schema_path = temp.path().join("schema.json");
    let expected_schema = json!({
        "type": "object",
        "properties": {
            "answer": { "type": "string" }
        },
        "required": ["answer"]
    });
    std::fs::write(&schema_path, serde_json::to_vec(&expected_schema)?)?;

    let cli = crate::cli::parse_owned_for_test(&[
        "chaos-exec".to_string(),
        "--sandbox".to_string(),
        "workspace-write".to_string(),
        "--profile".to_string(),
        "ci".to_string(),
        "--add-dir".to_string(),
        extra_a.display().to_string(),
        "--add-dir".to_string(),
        extra_b.display().to_string(),
        "--ephemeral".to_string(),
        "--output-schema".to_string(),
        schema_path.display().to_string(),
        "--json".to_string(),
        "-C".to_string(),
        cwd.display().to_string(),
        "-m".to_string(),
        "test-model".to_string(),
        "return structured output".to_string(),
    ]);
    let Cli {
        config_profile,
        model,
        auto_exec,
        cwd,
        add_dir,
        ephemeral,
        json,
        sandbox_mode,
        prompt,
        output_schema,
        ..
    } = cli;
    let expected_cwd = cwd.clone();

    let overrides = exec_config_overrides(ExecConfigOverrideInputs {
        model,
        config_profile,
        sandbox_mode: exec_sandbox_mode(auto_exec.full_auto, auto_exec.headless, sandbox_mode),
        cwd,
        ephemeral,
        additional_writable_roots: add_dir,
        model_provider: Some("test-provider".to_string()),
        alcatraz_exe: PathBuf::from("/alcatraz"),
    });

    assert!(json);
    assert_eq!(prompt.as_deref(), Some("return structured output"));
    assert_eq!(overrides.model.as_deref(), Some("test-model"));
    assert_eq!(overrides.config_profile.as_deref(), Some("ci"));
    assert_eq!(overrides.cwd.as_deref(), expected_cwd.as_deref());
    assert_eq!(overrides.sandbox_mode, Some(SandboxMode::WorkspaceWrite));
    assert_eq!(overrides.approval_policy, Some(ApprovalPolicy::Headless));
    assert_eq!(overrides.ephemeral, Some(true));
    assert_eq!(overrides.additional_writable_roots, vec![extra_a, extra_b]);
    assert_eq!(overrides.model_provider.as_deref(), Some("test-provider"));
    assert!(overrides.provider_user_override);
    assert_eq!(load_output_schema(output_schema)?, Some(expected_schema));

    Ok(())
}

#[test]
fn event_policy_covers_success_and_fatal_boundaries() {
    let task_id = "turn-1";
    let required_mcp_servers = HashSet::from(["required".to_string()]);
    let cases = [
        (
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: "other-turn".to_string(),
                last_agent_message: None,
            }),
            ExecEventDecision::Ignore,
        ),
        (
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: task_id.to_string(),
                last_agent_message: Some("done".to_string()),
            }),
            ExecEventDecision::Dispatch { fatal: false },
        ),
        (
            EventMsg::Error(ErrorEvent {
                message: "server failed".to_string(),
                chaos_error_info: None,
            }),
            ExecEventDecision::Dispatch { fatal: true },
        ),
        (
            EventMsg::McpStartupUpdate(McpStartupUpdateEvent {
                server: "required".to_string(),
                status: McpStartupStatus::Failed {
                    error: "not found".to_string(),
                },
            }),
            ExecEventDecision::Stop {
                fatal: true,
                interrupt: true,
                message: Some(
                    "Required MCP server 'required' failed to initialize: not found".to_string(),
                ),
            },
        ),
    ];

    for (event, expected) in cases {
        assert_eq!(
            exec_event_decision(&event, task_id, &required_mcp_servers),
            expected
        );
    }
}

#[test]
fn builds_uncommitted_review_request() {
    let args = ReviewArgs {
        uncommitted: true,
        base: None,
        commit: None,
        commit_title: None,
        prompt: None,
    };
    let request = build_review_request(&args).expect("builds uncommitted review request");

    let expected = ReviewRequest {
        target: ReviewTarget::UncommittedChanges,
        user_facing_hint: None,
        reviewer: None,
    };

    assert_eq!(request, expected);
}

#[test]
fn builds_commit_review_request_with_title() {
    let args = ReviewArgs {
        uncommitted: false,
        base: None,
        commit: Some("123456789".to_string()),
        commit_title: Some("Add review command".to_string()),
        prompt: None,
    };
    let request = build_review_request(&args).expect("builds commit review request");

    let expected = ReviewRequest {
        target: ReviewTarget::Commit {
            sha: "123456789".to_string(),
            title: Some("Add review command".to_string()),
        },
        user_facing_hint: None,
        reviewer: None,
    };

    assert_eq!(request, expected);
}

#[test]
fn builds_custom_review_request_trims_prompt() {
    let args = ReviewArgs {
        uncommitted: false,
        base: None,
        commit: None,
        commit_title: None,
        prompt: Some("  custom review instructions  ".to_string()),
    };
    let request = build_review_request(&args).expect("builds custom review request");

    let expected = ReviewRequest {
        target: ReviewTarget::Custom {
            instructions: "custom review instructions".to_string(),
        },
        user_facing_hint: None,
        reviewer: None,
    };

    assert_eq!(request, expected);
}

#[test]
fn decode_prompt_bytes_strips_utf8_bom() {
    let input = [0xEF, 0xBB, 0xBF, b'h', b'i', b'\n'];

    let out = decode_prompt_bytes(&input).expect("decode utf-8 with BOM");

    assert_eq!(out, "hi\n");
}

#[test]
fn decode_prompt_bytes_decodes_utf16le_bom() {
    // UTF-16LE BOM + "hi\n"
    let input = [0xFF, 0xFE, b'h', 0x00, b'i', 0x00, b'\n', 0x00];

    let out = decode_prompt_bytes(&input).expect("decode utf-16le with BOM");

    assert_eq!(out, "hi\n");
}

#[test]
fn decode_prompt_bytes_decodes_utf16be_bom() {
    // UTF-16BE BOM + "hi\n"
    let input = [0xFE, 0xFF, 0x00, b'h', 0x00, b'i', 0x00, b'\n'];

    let out = decode_prompt_bytes(&input).expect("decode utf-16be with BOM");

    assert_eq!(out, "hi\n");
}

#[test]
fn decode_prompt_bytes_rejects_utf32le_bom() {
    // UTF-32LE BOM + "hi\n"
    let input = [
        0xFF, 0xFE, 0x00, 0x00, b'h', 0x00, 0x00, 0x00, b'i', 0x00, 0x00, 0x00, b'\n', 0x00, 0x00,
        0x00,
    ];

    let err = decode_prompt_bytes(&input).expect_err("utf-32le should be rejected");

    assert_eq!(
        err,
        PromptDecodeError::UnsupportedBom {
            encoding: "UTF-32LE"
        }
    );
}

#[test]
fn decode_prompt_bytes_rejects_utf32be_bom() {
    // UTF-32BE BOM + "hi\n"
    let input = [
        0x00, 0x00, 0xFE, 0xFF, 0x00, 0x00, 0x00, b'h', 0x00, 0x00, 0x00, b'i', 0x00, 0x00, 0x00,
        b'\n',
    ];

    let err = decode_prompt_bytes(&input).expect_err("utf-32be should be rejected");

    assert_eq!(
        err,
        PromptDecodeError::UnsupportedBom {
            encoding: "UTF-32BE"
        }
    );
}

#[test]
fn decode_prompt_bytes_rejects_malformed_utf16() {
    // UTF-16LE BOM + odd trailing byte.
    let odd_le = [0xFF, 0xFE, b'h', 0x00, 0x00];
    assert_eq!(
        decode_prompt_bytes(&odd_le).unwrap_err(),
        PromptDecodeError::InvalidUtf16 {
            encoding: "UTF-16LE"
        }
    );

    // UTF-16BE BOM + odd trailing byte.
    let odd_be = [0xFE, 0xFF, 0x00, b'h', 0x00];
    assert_eq!(
        decode_prompt_bytes(&odd_be).unwrap_err(),
        PromptDecodeError::InvalidUtf16 {
            encoding: "UTF-16BE"
        }
    );

    // UTF-16LE BOM + lone surrogate.
    let lone_surrogate_le = [0xFF, 0xFE, 0x00, 0xD8, b'x', 0x00];
    assert_eq!(
        decode_prompt_bytes(&lone_surrogate_le).unwrap_err(),
        PromptDecodeError::InvalidUtf16 {
            encoding: "UTF-16LE"
        }
    );

    // UTF-16BE BOM + lone surrogate.
    let lone_surrogate_be = [0xFE, 0xFF, 0xD8, 0x00, 0x00, b'x'];
    assert_eq!(
        decode_prompt_bytes(&lone_surrogate_be).unwrap_err(),
        PromptDecodeError::InvalidUtf16 {
            encoding: "UTF-16BE"
        }
    );
}

#[test]
fn decode_prompt_bytes_rejects_invalid_utf8() {
    // Invalid UTF-8 sequence: 0xC3 0x28
    let input = [0xC3, 0x28];

    let err = decode_prompt_bytes(&input).expect_err("invalid utf-8 should fail");

    assert_eq!(err, PromptDecodeError::InvalidUtf8 { valid_up_to: 0 });
}
