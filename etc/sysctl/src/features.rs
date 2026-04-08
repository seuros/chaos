//! Feature flag type definitions and registry.
//!
//! Pure data definitions with no runtime dependencies on `Config`.
//! Runtime evaluation functions (`from_config`, `emit_metrics`) stay in
//! `chaos-kern::features`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// Unique features toggled via configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    ExecPermissionApprovals,
    RequestPermissionsTool,
    SpawnCsv,
}

impl Feature {
    pub fn key(self) -> &'static str {
        self.info().key
    }

    pub fn default_enabled(self) -> bool {
        self.info().default_enabled
    }

    fn info(self) -> &'static FeatureSpec {
        FEATURES
            .iter()
            .find(|spec| spec.id == self)
            .unwrap_or_else(|| unreachable!("missing FeatureSpec for {:?}", self))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LegacyFeatureUsage {
    pub alias: String,
    pub feature: Feature,
    pub summary: String,
    pub details: Option<String>,
}

/// Holds the effective set of enabled features.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Features {
    enabled: BTreeSet<Feature>,
    legacy_usages: BTreeSet<LegacyFeatureUsage>,
}

#[derive(Debug, Clone, Default)]
pub struct FeatureOverrides {}

impl FeatureOverrides {
    pub fn apply(self, _features: &mut Features) {}
}

impl Features {
    /// Starts with built-in defaults.
    pub fn with_defaults() -> Self {
        let mut set = BTreeSet::new();
        for spec in FEATURES {
            if spec.default_enabled {
                set.insert(spec.id);
            }
        }
        Self {
            enabled: set,
            legacy_usages: BTreeSet::new(),
        }
    }

    pub fn enabled(&self, f: Feature) -> bool {
        self.enabled.contains(&f)
    }

    pub fn enable(&mut self, f: Feature) -> &mut Self {
        self.enabled.insert(f);
        self
    }

    pub fn disable(&mut self, f: Feature) -> &mut Self {
        self.enabled.remove(&f);
        self
    }

    pub fn set_enabled(&mut self, f: Feature, enabled: bool) -> &mut Self {
        if enabled {
            self.enable(f)
        } else {
            self.disable(f)
        }
    }

    pub fn record_legacy_usage_force(&mut self, alias: &str, feature: Feature) {
        let (summary, details) = legacy_usage_notice(alias, feature);
        self.legacy_usages.insert(LegacyFeatureUsage {
            alias: alias.to_string(),
            feature,
            summary,
            details,
        });
    }

    pub fn record_legacy_usage(&mut self, alias: &str, feature: Feature) {
        if alias == feature.key() {
            return;
        }
        self.record_legacy_usage_force(alias, feature);
    }

    pub fn legacy_feature_usages(&self) -> impl Iterator<Item = &LegacyFeatureUsage> + '_ {
        self.legacy_usages.iter()
    }

    /// Apply a table of key -> bool toggles (e.g. from TOML).
    pub fn apply_map(&mut self, m: &BTreeMap<String, bool>) {
        for (k, v) in m {
            match feature_for_key(k) {
                Some(feat) => {
                    if k != feat.key() {
                        self.record_legacy_usage(k.as_str(), feat);
                    }
                    if *v {
                        self.enable(feat);
                    } else {
                        self.disable(feat);
                    }
                }
                None => {
                    tracing::warn!("unknown feature key in config: {k}");
                }
            }
        }
    }

    pub fn enabled_features(&self) -> Vec<Feature> {
        self.enabled.iter().copied().collect()
    }
}

fn legacy_usage_notice(alias: &str, feature: Feature) -> (String, Option<String>) {
    let canonical = feature.key();
    let label = if alias.contains('.') || alias.starts_with('[') {
        alias.to_string()
    } else {
        format!("[features].{alias}")
    };
    let summary = format!("`{label}` is deprecated. Use `[features].{canonical}` instead.");
    let details = if alias == canonical {
        None
    } else {
        Some(format!(
            "Enable it with `--enable {canonical}` or `[features].{canonical}` in config.toml."
        ))
    };
    (summary, details)
}

/// Keys accepted in `[features]` tables.
pub fn feature_for_key(key: &str) -> Option<Feature> {
    FEATURES.iter().find(|spec| spec.key == key).map(|s| s.id)
}

/// Returns `true` if the provided string matches a known feature toggle key.
pub fn is_known_feature_key(key: &str) -> bool {
    feature_for_key(key).is_some()
}

/// Returns `true` if the canonical feature for `key` is marked as under development.
pub fn is_under_development_feature_key(key: &str) -> bool {
    FEATURES
        .iter()
        .find(|spec| spec.key == key)
        .is_some_and(|spec| spec.under_development)
}

/// Deserializable features table for TOML.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
pub struct FeaturesToml {
    #[serde(flatten)]
    pub entries: BTreeMap<String, bool>,
}

/// Single, easy-to-read registry of all feature definitions.
#[derive(Debug, Clone, Copy)]
pub struct FeatureSpec {
    pub id: Feature,
    pub key: &'static str,
    pub default_enabled: bool,
    /// When `true`, enabling this feature prints a warning that it is
    /// under active development and may be unstable.
    pub under_development: bool,
}

pub const FEATURES: &[FeatureSpec] = &[
    FeatureSpec {
        id: Feature::ExecPermissionApprovals,
        key: "exec_permission_approvals",
        default_enabled: false,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::RequestPermissionsTool,
        key: "request_permissions_tool",
        default_enabled: false,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::SpawnCsv,
        key: "enable_fanout",
        default_enabled: false,
        under_development: false,
    },
];
