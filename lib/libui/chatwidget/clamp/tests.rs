use super::*;

#[test]
fn initial_clamp_model_uses_backend_defaults_for_unrelated_api_models() {
    assert_eq!(
        initial_clamp_model(ClampBackend::ClaudeCode, "gpt-5", None),
        "default"
    );
    assert_eq!(
        initial_clamp_model(ClampBackend::Antigravity, "gpt-5", None),
        chaos_clamp::AntigravityConfig::default().model
    );
    assert_eq!(
        initial_clamp_model(ClampBackend::Antigravity, "default", None),
        chaos_clamp::AntigravityConfig::default().model
    );
}

#[test]
fn initial_clamp_model_preserves_provider_models_and_honors_agy_override() {
    for model in ["claude-sonnet-4-6", "sonnet", "opus", "haiku"] {
        assert_eq!(
            initial_clamp_model(ClampBackend::ClaudeCode, model, None),
            model
        );
    }
    for model in [
        "gemini-3.1-pro-preview",
        "google/gemini-3.1-pro-preview",
        "gemini-3.1-pro-high",
        "claude-opus-4-6",
    ] {
        assert_eq!(
            initial_clamp_model(ClampBackend::Antigravity, model, None),
            model
        );
    }
    assert_eq!(
        initial_clamp_model(
            ClampBackend::Antigravity,
            "gemini-3.1-pro-low",
            Some("configured-agy-model".into()),
        ),
        "configured-agy-model"
    );
}
