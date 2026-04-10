//! Centralized feature flags and metadata.
//!
//! Type definitions and the feature registry live in `chaos-config::features`.
//! This module re-exports them and adds runtime functions that depend on
//! `Config`, `ConfigToml`, or telemetry.

// Re-export all pure type definitions from chaos-config.
pub use chaos_sysctl::features::*;

use crate::config::ConfigToml;
use crate::config::profile::ConfigProfile;
use chaos_syslog::SessionTelemetry;

/// Build a `Features` set from a parsed config and active profile.
pub fn features_from_config(
    cfg: &ConfigToml,
    config_profile: &ConfigProfile,
    overrides: FeatureOverrides,
) -> Features {
    let mut features = Features::with_defaults();

    if let Some(base_features) = cfg.features.as_ref() {
        features.apply_map(&base_features.entries);
    }

    if let Some(profile_features) = config_profile.features.as_ref() {
        features.apply_map(&profile_features.entries);
    }

    overrides.apply(&mut features);

    features
}

/// Emit feature flag state to telemetry.
pub fn emit_feature_metrics(features: &Features, otel: &SessionTelemetry) {
    for feature in FEATURES {
        if features.enabled(feature.id) != feature.default_enabled {
            otel.counter(
                "chaos.feature.state",
                /*inc*/ 1,
                &[
                    ("feature", feature.key),
                    ("value", &features.enabled(feature.id).to_string()),
                ],
            );
        }
    }
}

#[cfg(test)]
#[path = "features_tests.rs"]
mod tests;
