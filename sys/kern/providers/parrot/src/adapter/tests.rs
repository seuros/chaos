use super::*;
use crate::representer::ResponsesRepresenter;
use chaos_abi::FunctionToolDef;
use chaos_abi::ReasoningConfig;
use chaos_test_fixtures::TEST_MODEL;
use serde_json::json;

fn make_req(model: &str) -> TurnRequest {
    TurnRequest {
        model: model.to_string(),
        instructions: String::new(),
        input: vec![],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    }
}

#[test]
fn turn_request_converts_to_responses_api_request() {
    let req = TurnRequest {
        model: TEST_MODEL.to_string(),
        instructions: "Be helpful.".to_string(),
        input: vec![],
        tools: vec![ToolDef::Function(FunctionToolDef {
            name: "get_weather".to_string(),
            description: "Get weather for a location".to_string(),
            parameters: json!({"type": "object", "properties": {"location": {"type": "string"}}}),
            strict: true,
        })],
        parallel_tool_calls: true,
        reasoning: Some(ReasoningConfig {
            effort: Some(chaos_abi::ReasoningEffort::High),
            summary: None,
        }),
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    };

    let api_req = turn_request_to_api_request(req, &ResponsesRepresenter);

    assert_eq!(api_req.model, TEST_MODEL);
    assert_eq!(api_req.instructions, "Be helpful.");
    assert_eq!(api_req.tool_choice, "auto");
    assert!(api_req.parallel_tool_calls);
    assert!(api_req.stream);
    assert!(!api_req.store);
    assert_eq!(api_req.tools.len(), 1);
    assert_eq!(api_req.tools[0]["name"], "get_weather");
    assert!(api_req.reasoning.is_some());
    assert_eq!(
        api_req.include,
        vec!["reasoning.encrypted_content".to_string()]
    );
}

#[test]
fn ultra_effort_clamps_to_max_on_the_wire() {
    let mut req = make_req("gpt-5.6-sol");
    req.reasoning = Some(ReasoningConfig {
        effort: Some(chaos_abi::ReasoningEffort::Ultra),
        summary: None,
    });

    let api_req = turn_request_to_api_request(req, &ResponsesRepresenter);

    let reasoning = api_req.reasoning.expect("reasoning present");
    assert_eq!(reasoning.effort, Some(chaos_abi::ReasoningEffort::Max));
    assert_eq!(
        serde_json::to_value(&reasoning).unwrap()["effort"],
        serde_json::json!("max")
    );
}

#[test]
fn compatible_provider_preserves_ultra_effort_on_the_wire() {
    use crate::representer::OpenwAInnabeRepresenter;

    let mut req = make_req("grok-4");
    req.reasoning = Some(ReasoningConfig {
        effort: Some(chaos_abi::ReasoningEffort::Ultra),
        summary: None,
    });

    let api_req = turn_request_to_api_request(req, &OpenwAInnabeRepresenter);

    let reasoning = api_req.reasoning.expect("reasoning present");
    assert_eq!(reasoning.effort, Some(chaos_abi::ReasoningEffort::Ultra));
    assert_eq!(
        serde_json::to_value(&reasoning).unwrap()["effort"],
        serde_json::json!("ultra")
    );
}

#[test]
fn extensions_populate_openai_specific_fields() {
    let mut extensions = serde_json::Map::new();
    extensions.insert("store".to_string(), json!(true));
    extensions.insert("service_tier".to_string(), json!("priority"));
    extensions.insert("prompt_cache_key".to_string(), json!("conv-123"));

    let req = TurnRequest {
        model: TEST_MODEL.to_string(),
        instructions: String::new(),
        input: vec![],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions,
    };

    let api_req = turn_request_to_api_request(req, &ResponsesRepresenter);

    assert!(api_req.store);
    assert_eq!(api_req.service_tier.as_deref(), Some("priority"));
    assert_eq!(api_req.prompt_cache_key.as_deref(), Some("conv-123"));
}

#[test]
fn openai_tools_extension_overrides_neutral_tool_conversion() {
    let mut extensions = serde_json::Map::new();
    extensions.insert(
        "openai_tools".to_string(),
        json!([{
            "type": "local_shell"
        }]),
    );

    let req = TurnRequest {
        model: TEST_MODEL.to_string(),
        instructions: String::new(),
        input: vec![],
        tools: vec![ToolDef::Function(FunctionToolDef {
            name: "should_not_be_used".to_string(),
            description: "ignored".to_string(),
            parameters: json!({"type": "object"}),
            strict: false,
        })],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions,
    };

    let api_req = turn_request_to_api_request(req, &ResponsesRepresenter);
    assert_eq!(api_req.tools, vec![json!({"type": "local_shell"})]);
}

#[test]
fn wannabe_representer_used_via_turn_request_conversion() {
    use crate::representer::OpenwAInnabeRepresenter;
    use chaos_abi::ResponseItem;
    let req = TurnRequest {
        input: vec![
            ResponseItem::Message {
                id: None,
                role: "system".into(),
                content: vec![],
                end_turn: None,
                phase: None,
            },
            ResponseItem::Reasoning {
                id: "rs".into(),
                summary: vec![],
                content: None,
                encrypted_content: None,
            },
        ],
        ..make_req("grok-4")
    };
    let api_req = turn_request_to_api_request(req, &OpenwAInnabeRepresenter);
    assert_eq!(api_req.input.len(), 1, "Reasoning item must be dropped");
    assert!(
        matches!(&api_req.input[0], ResponseItem::Message { role, .. } if role == "system"),
        "system role must pass through unchanged for wannabe"
    );
}

#[test]
fn response_event_roundtrips_through_turn_event() {
    let event = ResponseEvent::OutputTextDelta("hello".to_string());
    let turn: TurnEvent = event.into();
    let back: ResponseEvent = turn.into();
    assert!(matches!(back, ResponseEvent::OutputTextDelta(ref s) if s == "hello"));
}

#[test]
fn api_error_semantics_are_preserved_in_abi_error() {
    let err: crate::error::ApiError = crate::TransportError::Http {
        status: rama::http::StatusCode::UNAUTHORIZED,
        url: Some("https://api.openai.com/v1/responses".to_string()),
        headers: None,
        body: Some("unauthorized".to_string()),
    }
    .into();

    let abi: AbiError = err.into();
    assert!(matches!(
        abi,
        AbiError::Transport { status: 401, message } if message == "unauthorized"
    ));

    let outage: crate::error::ApiError = crate::TransportError::Http {
        status: rama::http::StatusCode::NOT_FOUND,
        url: Some(format!(
            "{}?client_version=47.2.0",
            chaos_services::openai::CHATGPT_MODELS_URL
        )),
        headers: None,
        body: None,
    }
    .into();
    assert!(matches!(outage, crate::error::ApiError::ServiceUnavailable));
    let abi: AbiError = outage.into();
    assert!(matches!(abi, AbiError::ServiceUnavailable));

    for transport in [
        crate::TransportError::Http {
            status: rama::http::StatusCode::NOT_FOUND,
            url: Some("https://example.com/v1/models".to_string()),
            headers: None,
            body: None,
        },
        crate::TransportError::Http {
            status: rama::http::StatusCode::NOT_FOUND,
            url: Some(chaos_services::openai::CHATGPT_RESPONSES_URL.to_string()),
            headers: None,
            body: Some(r#"{"error":{"message":"not found"}}"#.to_string()),
        },
    ] {
        assert!(matches!(
            crate::error::ApiError::from(transport),
            crate::error::ApiError::Transport(_)
        ));
    }
}

#[test]
fn freeform_tool_converts_to_custom_json() {
    let tool = ToolDef::Freeform(chaos_abi::FreeformToolDef {
        name: "apply_patch".to_string(),
        description: "Apply a patch".to_string(),
        format_type: "xml".to_string(),
        syntax: "xml-patch".to_string(),
        definition: "<patch>...</patch>".to_string(),
    });

    let json = tool_def_to_openai(tool);
    assert_eq!(json["type"], "custom");
    assert_eq!(json["name"], "apply_patch");
    assert_eq!(json["format"]["type"], "xml");
}
