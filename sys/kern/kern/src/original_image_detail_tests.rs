use super::*;

use crate::config::test_config;
use crate::models_manager::manager::ModelsManager;
use pretty_assertions::assert_eq;

#[test]
fn image_detail_original_enabled_when_model_supports_it() {
    let config = test_config();
    let mut model_info =
        ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    model_info.supports_image_detail_original = true;

    assert!(can_request_original_image_detail(&model_info));
    assert_eq!(
        normalize_output_image_detail(&model_info, Some(ImageDetail::Original)),
        Some(ImageDetail::Original)
    );
    assert_eq!(normalize_output_image_detail(&model_info, None), None);
}

#[test]
fn explicit_original_is_dropped_without_model_support() {
    let config = test_config();
    let mut model_info =
        ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    model_info.supports_image_detail_original = false;
    assert_eq!(
        normalize_output_image_detail(&model_info, Some(ImageDetail::Original)),
        None
    );
}

#[test]
fn unsupported_non_original_detail_is_dropped() {
    let config = test_config();
    let mut model_info =
        ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    model_info.supports_image_detail_original = true;

    assert_eq!(
        normalize_output_image_detail(&model_info, Some(ImageDetail::Low)),
        None
    );
}
