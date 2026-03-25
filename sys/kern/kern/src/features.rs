//! Centralized feature flags and metadata.
//!
//! Type definitions and the feature registry live in `codex-config::features`.
//! This module re-exports them and adds runtime functions that depend on
//! `Config`, `ConfigToml`, or telemetry.

// Re-export all pure type definitions from codex-config.
pub use chaos_sysctl::features::*;

use crate::config::Config;
use crate::config::ConfigToml;
use crate::config::profile::ConfigProfile;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::WarningEvent;
use chaos_sysctl::CONFIG_TOML_FILE;
use chaos_syslog::SessionTelemetry;
use toml::Value as TomlValue;

/// Build a `Features` set from a parsed config and active profile.
pub fn features_from_config(
    cfg: &ConfigToml,
    config_profile: &ConfigProfile,
    overrides: FeatureOverrides,
) -> Features {
    let mut features = Features::with_defaults();

    let base_legacy = LegacyFeatureToggles {
        experimental_use_freeform_apply_patch: cfg.experimental_use_freeform_apply_patch,
        experimental_use_unified_exec_tool: cfg.experimental_use_unified_exec_tool,
        ..Default::default()
    };
    base_legacy.apply(&mut features);

    if let Some(base_features) = cfg.features.as_ref() {
        features.apply_map(&base_features.entries);
    }

    let profile_legacy = LegacyFeatureToggles {
        include_apply_patch_tool: config_profile.include_apply_patch_tool,
        experimental_use_freeform_apply_patch: config_profile.experimental_use_freeform_apply_patch,
        experimental_use_unified_exec_tool: config_profile.experimental_use_unified_exec_tool,
    };
    profile_legacy.apply(&mut features);
    if let Some(profile_features) = config_profile.features.as_ref() {
        features.apply_map(&profile_features.entries);
    }

    overrides.apply(&mut features);
    features.normalize_dependencies();

    features
}

/// Emit feature flag state to telemetry.
pub fn emit_feature_metrics(features: &Features, otel: &SessionTelemetry) {
    for feature in FEATURES {
        if matches!(feature.stage, Stage::Removed) {
            continue;
        }
        if features.enabled(feature.id) != feature.default_enabled {
            otel.counter(
                "codex.feature.state",
                /*inc*/ 1,
                &[
                    ("feature", feature.key),
                    ("value", &features.enabled(feature.id).to_string()),
                ],
            );
        }
    }
}

/// Push a warning event if any under-development features are enabled.
pub fn maybe_push_unstable_features_warning(
    config: &Config,
    post_session_configured_events: &mut Vec<Event>,
) {
    if config.suppress_unstable_features_warning {
        return;
    }

    let mut under_development_feature_keys = Vec::new();
    if let Some(table) = config
        .config_layer_stack
        .effective_config()
        .get("features")
        .and_then(TomlValue::as_table)
    {
        for (key, value) in table {
            if value.as_bool() != Some(true) {
                continue;
            }
            let Some(spec) = FEATURES.iter().find(|spec| spec.key == key.as_str()) else {
                continue;
            };
            if !config.features.enabled(spec.id) {
                continue;
            }
            if matches!(spec.stage, Stage::UnderDevelopment) {
                under_development_feature_keys.push(spec.key.to_string());
            }
        }
    }

    if under_development_feature_keys.is_empty() {
        return;
    }

    let under_development_feature_keys = under_development_feature_keys.join(", ");
    let config_path = config
        .codex_home
        .join(CONFIG_TOML_FILE)
        .display()
        .to_string();
    let message = format!(
        "Under-development features enabled: {under_development_feature_keys}. Under-development features are incomplete and may behave unpredictably. To suppress this warning, set `suppress_unstable_features_warning = true` in {config_path}."
    );
    post_session_configured_events.push(Event {
        id: "".to_owned(),
        msg: EventMsg::Warning(WarningEvent { message }),
    });
}

#[cfg(test)]
#[path = "features_tests.rs"]
mod tests;
