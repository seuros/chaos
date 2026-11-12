use super::*;
use crate::test_support::test_remote_model;
use chaos_ipc::openai_models::ModelVisibility;

fn presets(slugs: &[&str]) -> Vec<ModelPreset> {
    slugs
        .iter()
        .map(|slug| test_remote_model(slug, ModelVisibility::List, 1).into())
        .collect()
}

#[test]
fn model_lookup_normalizes_ids_and_preserves_catalog_slugs() -> Result<(), FunctionCallError> {
    let models = presets(&["vendor/Model-v1.2", "other-model"]);
    for requested in [
        "vendor/Model-v1.2",
        "vendor/model-v1.2",
        "VENDOR/MODEL-V1.2",
        " \tvendor/Model-v1.2\r\n",
        " \tVENDOR/MODEL-V1.2\r\n",
    ] {
        assert_eq!(find_model(&models, requested, "spawn_agent")?, &models[0]);
    }

    let padded = presets(&[" \tvendor/Model-v1.2\r\n"]);
    assert_eq!(
        find_model(&padded, "VENDOR/MODEL-V1.2", "spawn_agent")?,
        &padded[0]
    );
    Ok(())
}

#[test]
fn model_lookup_prefers_exact_ids_and_rejects_ambiguous_normalization()
-> Result<(), FunctionCallError> {
    let models = presets(&["vendor/Model", "vendor/model"]);
    for (requested, index) in [
        ("vendor/Model", 0),
        ("vendor/model", 1),
        (" \tvendor/model\n", 1),
    ] {
        assert_eq!(
            find_model(&models, requested, "spawn_agent")?,
            &models[index]
        );
    }
    assert!(find_model(&models, "VENDOR/MODEL", "spawn_agent").is_err());
    Ok(())
}

#[test]
fn model_lookup_rejects_unknown_ids_without_folding_punctuation_or_namespaces() {
    let models = presets(&["vendor/Model-v1.2"]);
    for requested in [
        "",
        " \t\r\n",
        "unknown-model",
        "Model-v1.2",
        "vendor/Model-v1-2",
        "vendor/Model_v1.2",
        "vendor/Model-v1.2-extra",
        "vendor/Model-v1.2\u{200b}",
    ] {
        let error = find_model(&models, requested, "spawn_agent").expect_err("unknown model");
        assert_eq!(
            error,
            FunctionCallError::RespondToModel(format!(
                "Unknown model `{requested}` for spawn_agent. Available models: vendor/Model-v1.2"
            ))
        );
    }
    assert!(find_model(&[], "vendor/Model-v1.2", "spawn_agent").is_err());
}

#[test]
fn effort_selection_validates_overrides_and_preserves_defaults() -> Result<(), FunctionCallError> {
    use ReasoningEffort::{High, Low};

    let supported = [ReasoningEffortPreset {
        effort: Low,
        description: String::new(),
    }];
    assert_eq!(
        resolve_reasoning_effort("model", &supported, None, None)?,
        None
    );
    assert_eq!(
        resolve_reasoning_effort("model", &supported, None, Some(High))?,
        Some(High)
    );
    assert_eq!(
        resolve_reasoning_effort("model", &supported, Some(Low), Some(High))?,
        Some(Low)
    );
    assert!(resolve_reasoning_effort("model", &supported, Some(High), Some(Low)).is_err());
    Ok(())
}
