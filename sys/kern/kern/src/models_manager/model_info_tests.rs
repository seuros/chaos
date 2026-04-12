use super::*;
use crate::config::test_config;
use chaos_abi::AbiModelInfo;
use pretty_assertions::assert_eq;

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
    })
}

#[test]
fn reasoning_summaries_override_true_enables_support() {
    let model = unknown_model();
    let mut config = test_config();
    config.model_supports_reasoning_summaries = Some(true);

    let updated = with_config_overrides(model.clone(), &config);
    let mut expected = model;
    expected.supports_reasoning_summaries = true;

    assert_eq!(updated, expected);
}

#[test]
fn reasoning_summaries_override_false_does_not_disable_support() {
    let mut model = unknown_model();
    model.supports_reasoning_summaries = true;
    let mut config = test_config();
    config.model_supports_reasoning_summaries = Some(false);

    let updated = with_config_overrides(model.clone(), &config);

    assert_eq!(updated, model);
}

#[test]
fn reasoning_summaries_override_false_is_noop_when_model_is_false() {
    let model = unknown_model();
    let mut config = test_config();
    config.model_supports_reasoning_summaries = Some(false);

    let updated = with_config_overrides(model.clone(), &config);

    assert_eq!(updated, model);
}
