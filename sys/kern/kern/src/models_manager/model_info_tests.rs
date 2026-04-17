use super::*;
use crate::config::test_config;
use chaos_abi::AbiModelInfo;

#[test]
fn with_config_overrides_never_yields_empty_base_instructions() {
    let model = model_info_from_abi(&AbiModelInfo {
        id: "test-model".to_string(),
        display_name: "Test".to_string(),
        max_input_tokens: None,
        max_output_tokens: None,
        supports_thinking: false,
        supports_images: false,
        supports_structured_output: false,
        supports_reasoning_effort: false,
        native_server_side_tools: vec![],
    });
    // Catalog-only ModelInfo has empty base_instructions.
    assert!(model.base_instructions.is_empty());

    // After kern finalization the sentinel is replaced.
    let finalized = with_config_overrides(model, &test_config());
    assert!(!finalized.base_instructions.is_empty());
}

fn unknown_model() -> ModelInfo {
    model_info_from_abi(&AbiModelInfo {
        id: "unknown-model".to_string(),
        display_name: "unknown-model".to_string(),
        max_input_tokens: None,
        max_output_tokens: None,
        supports_thinking: false,
        supports_images: false,
        supports_structured_output: false,
        supports_reasoning_effort: false,
        native_server_side_tools: vec![],
    })
}

#[test]
fn reasoning_summaries_override_true_enables_support() {
    let model = unknown_model();
    let mut config = test_config();
    config.model_supports_reasoning_summaries = Some(true);

    let updated = with_config_overrides(model, &config);

    assert!(updated.supports_reasoning_summaries);
}

#[test]
fn reasoning_summaries_override_false_does_not_disable_support() {
    let mut model = unknown_model();
    model.supports_reasoning_summaries = true;
    let mut config = test_config();
    config.model_supports_reasoning_summaries = Some(false);

    let updated = with_config_overrides(model, &config);

    assert!(updated.supports_reasoning_summaries);
}

#[test]
fn reasoning_summaries_override_false_is_noop_when_model_is_false() {
    let model = unknown_model();
    let mut config = test_config();
    config.model_supports_reasoning_summaries = Some(false);

    let updated = with_config_overrides(model, &config);

    assert!(!updated.supports_reasoning_summaries);
}
