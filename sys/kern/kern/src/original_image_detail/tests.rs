use super::*;

use crate::config::test_config;
use crate::models_manager::manager::ModelsManager;

#[test]
fn original_image_detail_follows_model_capability() {
    let config = test_config();
    let mut model_info =
        ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    for supported in [false, true] {
        model_info.supports_image_detail_original = supported;
        assert_eq!(can_request_original_image_detail(&model_info), supported);
    }
}
