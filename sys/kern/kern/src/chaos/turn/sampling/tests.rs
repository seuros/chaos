use super::*;
use std::assert_matches;

fn retry_lifecycle() -> TurnLifecycle {
    let mut lifecycle = TurnLifecycle::default();
    lifecycle.apply(TurnControlEvent::Sample);
    lifecycle.apply(TurnControlEvent::AttemptSettled);
    assert!(lifecycle.try_retry(0));
    lifecycle
}

#[tokio::test(start_paused = true)]
async fn retry_backoff_cancels_without_waiting_for_delay() {
    let mut lifecycle = retry_lifecycle();
    let token = CancellationToken::new();
    let started = tokio::time::Instant::now();
    let cancel = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        token.cancel();
    };
    let (result, ()) = tokio::join!(
        wait_for_retry(&mut lifecycle, Duration::from_secs(60), &token),
        cancel
    );
    assert_matches!(result, Err(ChaosErr::TurnAborted));
    assert_eq!(started.elapsed(), Duration::from_secs(1));
    lifecycle.close(true);
}

#[tokio::test(start_paused = true)]
async fn cancellation_wins_when_retry_timer_is_already_ready() {
    let mut lifecycle = retry_lifecycle();
    let token = CancellationToken::new();
    token.cancel();
    assert_matches!(
        wait_for_retry(&mut lifecycle, Duration::ZERO, &token).await,
        Err(ChaosErr::TurnAborted)
    );
    lifecycle.close(true);
}

#[tokio::test(start_paused = true)]
async fn elapsed_retry_reenters_preparation_before_sampling() {
    let mut lifecycle = retry_lifecycle();
    wait_for_retry(
        &mut lifecycle,
        Duration::from_secs(1),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    lifecycle.apply(TurnControlEvent::Sample);
}

#[test]
fn appends_labeled_mcp_server_instructions() {
    let mut base = chaos_ipc::models::BaseInstructions {
        text: "Base instructions.".to_string(),
    };

    append_mcp_server_instructions(
        &mut base,
        &[
            McpServerInstructions {
                server_name: "alpha & \"tools\"".to_string(),
                server_version: "1.2.3 & \"preview\"".to_string(),
                capabilities: serde_json::json!({
                    "resources": {"subscribe": true},
                    "tools": {"listChanged": false}
                }),
                instructions: "Use Vec<T> & preserve\n  ]]> literally.".to_string(),
            },
            McpServerInstructions {
                server_name: "beta".to_string(),
                server_version: "2.0.0".to_string(),
                capabilities: serde_json::json!({}),
                instructions: "Prefer beta resources.".to_string(),
            },
        ],
    )
    .unwrap();

    assert!(base.text.starts_with("Base instructions."));
    let xml = base.text.split_once("<mcp_server_instructions>").unwrap().1;
    assert_eq!(
        xml,
        concat!(
            r#"
  <server name="alpha &amp; &quot;tools&quot;" "#,
            r#"version="1.2.3 &amp; &quot;preview&quot;" capabilities="resources, tools">
    <instructions><![CDATA[Use Vec<T> & preserve
  ]]]]><![CDATA[> literally.]]></instructions>
  </server>
  <server name="beta" version="2.0.0" capabilities="">
    <instructions><![CDATA[Prefer beta resources.]]></instructions>
  </server>
</mcp_server_instructions>"#
        )
    );
}

#[test]
fn server_capability_xml_summarizes_model_facing_features() {
    for (capabilities, expected) in [
        (
            serde_json::json!({
                "tools": {"listChanged": false},
                "resources": {"subscribe": true},
                "prompts": {},
            }),
            "prompts, resources, tools",
        ),
        (
            serde_json::json!({
                "vendor/extra": {"z": 2, "a": 1},
                "tools": {"listChanged": false},
                "resources": {"subscribe": true},
                "prompts": {},
                "experimental": {
                    "zeta": {},
                    "vendor/feature": {"enabled": true},
                },
                "logging": {},
                "tasks": {
                    "cancel": {},
                    "list": {},
                    "requests": {"tools": {"call": {}}},
                },
            }),
            "prompts, resources, tools, other*",
        ),
        (
            serde_json::json!({
                "tasks": {
                    "cancel": {},
                    "list": {},
                    "requests": {"tools": {"call": {}}},
                },
            }),
            "other*",
        ),
        (
            serde_json::json!({"experimental": {"vendor/<&\"'>]]>": {}}}),
            "other*",
        ),
        (serde_json::json!({"vendor/<&\"'>]]>": {}}), "other*"),
        (
            serde_json::json!({"experimental": {}, "tools": {}}),
            "tools, other*",
        ),
        (
            serde_json::json!({"experimental": null, "tools": {}}),
            "tools, other*",
        ),
        (serde_json::json!({}), ""),
        (serde_json::Value::Null, ""),
    ] {
        let servers = [McpServerInstructions {
            server_name: "test".to_string(),
            server_version: "1.0.0".to_string(),
            capabilities,
            instructions: "Use this server.".to_string(),
        }];
        let xml = McpInstructionsDocument::new(&servers).to_xml().unwrap();
        assert!(!xml.contains("<capabilities>"));
        assert!(!xml.contains("listChanged"));
        assert!(!xml.contains("enabled"));
        assert!(!xml.contains("subscribe"));
        assert!(!xml.contains("tasks"));
        assert!(!xml.contains("requests"));
        assert!(!xml.contains("experimental"));
        assert!(!xml.contains("vendor/"));
        let mut reader = quick_xml::Reader::from_str(&xml);
        let mut server_count = 0;
        loop {
            match reader.read_event().unwrap() {
                quick_xml::events::Event::Start(element) if element.name().as_ref() == "server" => {
                    server_count += 1;
                    let attribute = element.try_get_attribute("capabilities").unwrap().unwrap();
                    let names = attribute
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .unwrap();
                    assert_eq!(names, expected);
                }
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        assert_eq!(server_count, 1);
    }
}

#[test]
fn leaves_base_instructions_unchanged_without_mcp_instructions() {
    let mut base = chaos_ipc::models::BaseInstructions {
        text: "Base instructions.".to_string(),
    };

    append_mcp_server_instructions(&mut base, &[]).unwrap();

    assert_eq!(base.text, "Base instructions.");
}

#[test]
fn runtime_guidance_is_request_local_and_preserves_literal_base() {
    let original = vec![DeveloperInstructions::new("saved context").into()];
    let mut prompt = Prompt {
        input: original.clone(),
        tools: vec![crate::client_common::tools::ToolSpec::LocalShell {}],
        base_instructions: chaos_ipc::models::BaseInstructions {
            text: "literal {{ not_a_template }}".to_owned(),
        },
        ..Prompt::default()
    };
    let catalog = crate::tools::groups::build_catalog().unwrap();
    let state = catalog.new_state();
    append_runtime_instructions(
        &mut prompt,
        ToolGroupFilter {
            catalog: &catalog,
            state: &state,
        },
        ModeCapabilities::default(),
        &SessionSource::Api,
        false,
        ApprovalPolicy::Interactive,
    );
    assert_eq!(&prompt.input[..original.len()], original.as_slice());
    assert_eq!(prompt.input.len(), original.len() + 1);
    assert_eq!(
        prompt.base_instructions.text,
        "literal {{ not_a_template }}"
    );
    let message = serde_json::to_value(prompt.input.last().unwrap()).unwrap();
    assert_eq!(message["role"], "system");
}

#[cfg(feature = "tui")]
#[test]
fn headless_policy_suppresses_console_guidance() {
    let catalog = crate::tools::groups::build_catalog().unwrap();
    let state = catalog.new_state();
    for (policy, expected_messages) in [
        (ApprovalPolicy::Interactive, 1),
        (ApprovalPolicy::Headless, 0),
    ] {
        let mut prompt = Prompt::default();
        append_runtime_instructions(
            &mut prompt,
            ToolGroupFilter {
                catalog: &catalog,
                state: &state,
            },
            ModeCapabilities::default(),
            &SessionSource::Cli,
            true,
            policy,
        );
        assert_eq!(prompt.input.len(), expected_messages);
    }
}
