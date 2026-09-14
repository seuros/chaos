use super::*;

#[test]
fn reasoning_effort_accepts_current_catalog_levels() {
    assert_eq!(
        serde_json::from_str::<ReasoningEffort>("\"max\"").expect("deserialize max"),
        ReasoningEffort::Max
    );
    assert_eq!(
        serde_json::from_str::<ReasoningEffort>("\"ultra\"").expect("deserialize ultra"),
        ReasoningEffort::Ultra
    );
}
use pretty_assertions::assert_eq;

#[test]
fn model_family_deserialization_is_canonical_and_conservative() {
    let family: ModelFamily =
        serde_json::from_str(r#""  Anthropic  ""#).expect("valid family string");
    assert_eq!(family.as_str(), "anthropic");

    let malformed: ModelFamily =
        serde_json::from_str(r#""not a family""#).expect("string should deserialize");
    assert!(malformed.is_unknown());
}

fn test_model(spec: Option<ModelMessages>) -> ModelInfo {
    ModelInfo {
        slug: "test-model".to_string(),
        model_family: ModelFamily::default(),
        display_name: "Test Model".to_string(),
        description: None,
        default_reasoning_level: None,
        supported_reasoning_levels: vec![],
        shell_type: ConfigShellToolType::ShellCommand,
        visibility: ModelVisibility::List,
        supported_in_api: true,
        priority: 1,
        availability_nux: None,
        base_instructions: "base".to_string(),
        model_messages: spec,
        supports_reasoning_summaries: false,
        default_reasoning_summary: ReasoningSummary::Auto,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: None,
        web_search_tool_type: WebSearchToolType::Text,
        truncation_policy: TruncationPolicyConfig::bytes(10_000),
        supports_parallel_tool_calls: false,
        supports_image_detail_original: false,
        context_window: None,
        auto_compact_token_limit: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: vec![],
        input_modalities: default_input_modalities(),
        native_server_side_tools: vec![],
        used_fallback_model_metadata: false,
    }
}

fn personality_variables() -> ModelInstructionsVariables {
    ModelInstructionsVariables {
        personality_default: Some("default".to_string()),
        personality_friendly: Some("friendly".to_string()),
        personality_pragmatic: Some("pragmatic".to_string()),
    }
}

#[test]
fn get_model_instructions_uses_template_when_placeholder_present() {
    let model = test_model(Some(ModelMessages {
        instructions_template: Some("Hello {{ personality }}".to_string()),
        instructions_variables: Some(personality_variables()),
    }));

    let instructions = model.get_model_instructions(Some(Personality::Friendly));

    assert_eq!(instructions, "Hello friendly");
}

#[test]
fn get_model_instructions_always_strips_placeholder() {
    let model = test_model(Some(ModelMessages {
        instructions_template: Some("Hello\n{{ personality }}".to_string()),
        instructions_variables: Some(ModelInstructionsVariables {
            personality_default: None,
            personality_friendly: Some("friendly".to_string()),
            personality_pragmatic: None,
        }),
    }));
    assert_eq!(
        model.get_model_instructions(Some(Personality::Friendly)),
        "Hello\nfriendly"
    );
    assert_eq!(
        model.get_model_instructions(Some(Personality::Pragmatic)),
        "Hello\n"
    );
    assert_eq!(
        model.get_model_instructions(Some(Personality::None)),
        "Hello\n"
    );
    assert_eq!(model.get_model_instructions(None), "Hello\n");

    let model_no_personality = test_model(Some(ModelMessages {
        instructions_template: Some("Hello\n{{ personality }}".to_string()),
        instructions_variables: Some(ModelInstructionsVariables {
            personality_default: None,
            personality_friendly: None,
            personality_pragmatic: None,
        }),
    }));
    assert_eq!(
        model_no_personality.get_model_instructions(Some(Personality::Friendly)),
        "Hello\n"
    );
    assert_eq!(
        model_no_personality.get_model_instructions(Some(Personality::Pragmatic)),
        "Hello\n"
    );
    assert_eq!(
        model_no_personality.get_model_instructions(Some(Personality::None)),
        "Hello\n"
    );
    assert_eq!(model_no_personality.get_model_instructions(None), "Hello\n");
}

#[test]
fn get_model_instructions_falls_back_when_template_is_missing() {
    let model = test_model(Some(ModelMessages {
        instructions_template: None,
        instructions_variables: Some(ModelInstructionsVariables {
            personality_default: None,
            personality_friendly: None,
            personality_pragmatic: None,
        }),
    }));

    let instructions = model.get_model_instructions(Some(Personality::Friendly));

    assert_eq!(instructions, "base");
}

#[test]
fn get_personality_message_returns_default_when_personality_is_none() {
    let personality_template = personality_variables();
    assert_eq!(
        personality_template.get_personality_message(None),
        Some("default".to_string())
    );
}

#[test]
fn get_personality_message() {
    let personality_variables = personality_variables();
    assert_eq!(
        personality_variables.get_personality_message(Some(Personality::Friendly)),
        Some("friendly".to_string())
    );
    assert_eq!(
        personality_variables.get_personality_message(Some(Personality::Pragmatic)),
        Some("pragmatic".to_string())
    );
    assert_eq!(
        personality_variables.get_personality_message(Some(Personality::None)),
        Some(String::new())
    );
    assert_eq!(
        personality_variables.get_personality_message(None),
        Some("default".to_string())
    );

    let personality_variables = ModelInstructionsVariables {
        personality_default: Some("default".to_string()),
        personality_friendly: None,
        personality_pragmatic: None,
    };
    assert_eq!(
        personality_variables.get_personality_message(Some(Personality::Friendly)),
        None
    );
    assert_eq!(
        personality_variables.get_personality_message(Some(Personality::Pragmatic)),
        None
    );
    assert_eq!(
        personality_variables.get_personality_message(Some(Personality::None)),
        Some(String::new())
    );
    assert_eq!(
        personality_variables.get_personality_message(None),
        Some("default".to_string())
    );

    let personality_variables = ModelInstructionsVariables {
        personality_default: None,
        personality_friendly: Some("friendly".to_string()),
        personality_pragmatic: Some("pragmatic".to_string()),
    };
    assert_eq!(
        personality_variables.get_personality_message(Some(Personality::Friendly)),
        Some("friendly".to_string())
    );
    assert_eq!(
        personality_variables.get_personality_message(Some(Personality::Pragmatic)),
        Some("pragmatic".to_string())
    );
    assert_eq!(
        personality_variables.get_personality_message(Some(Personality::None)),
        Some(String::new())
    );
    assert_eq!(personality_variables.get_personality_message(None), None);
}

#[test]
fn model_info_defaults_availability_nux_to_none_when_omitted() {
    let model: ModelInfo = serde_json::from_value(serde_json::json!({
        "slug": "test-model",
        "display_name": "Test Model",
        "description": null,
        "supported_reasoning_levels": [],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "base_instructions": "base",
        "model_messages": null,
        "supports_reasoning_summaries": false,
        "default_reasoning_summary": "auto",
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {
            "mode": "bytes",
            "limit": 10000
        },
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "context_window": null,
        "auto_compact_token_limit": null,
        "effective_context_window_percent": 95,
        "experimental_supported_tools": [],
        "input_modalities": ["text", "image"]
    }))
    .expect("deserialize model info");

    assert_eq!(model.availability_nux, None);
    assert!(!model.supports_image_detail_original);
    assert_eq!(model.web_search_tool_type, WebSearchToolType::Text);
}

#[test]
fn model_preset_preserves_availability_nux() {
    let preset = ModelPreset::from(ModelInfo {
        availability_nux: Some(ModelAvailabilityNux {
            message: "Try Spark.".to_string(),
        }),
        ..test_model(None)
    });

    assert_eq!(
        preset.availability_nux,
        Some(ModelAvailabilityNux {
            message: "Try Spark.".to_string(),
        })
    );
}
