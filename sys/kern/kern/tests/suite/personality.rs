use chaos_kern::config::types::Personality;
use core_test_support::load_default_config_for_test;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn personality_does_not_mutate_base_instructions_without_template() {
    let chaos_home = TempDir::new().expect("create temp dir");
    let mut config = load_default_config_for_test(&chaos_home).await;
    config.personality = Some(Personality::Friendly);

    let model_info = chaos_kern::test_support::construct_model_info_offline("gpt-5.1", &config);
    assert_eq!(
        model_info.get_model_instructions(config.personality),
        model_info.base_instructions
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn base_instructions_override_disables_personality_template() {
    let chaos_home = TempDir::new().expect("create temp dir");
    let mut config = load_default_config_for_test(&chaos_home).await;
    config.personality = Some(Personality::Friendly);
    config.base_instructions = Some("override instructions".to_string());

    let model_info =
        chaos_kern::test_support::construct_model_info_offline("gpt-5.4-codex", &config);

    assert_eq!(model_info.base_instructions, "override instructions");
    assert_eq!(
        model_info.get_model_instructions(config.personality),
        "override instructions"
    );
}
