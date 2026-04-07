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
use tracing::info;

/// Unique features toggled via configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    GhostCommit,
    ShellTool,
    UnifiedExec,
    ApplyPatchFreeform,
    ExecPermissionApprovals,
    CodexHooks,
    RequestPermissionsTool,
    ShellSnapshot,
    ChildAgentsMd,
    ImageDetailOriginal,
    EnableRequestCompression,
    Collab,
    SpawnCsv,
    SkillMcpDependencyInstall,
    SkillEnvVarDependencyPrompt,
    DefaultModeRequestUserInput,
    ToolCallMcpElicitation,
    Personality,
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
pub struct FeatureOverrides {
    pub include_apply_patch_tool: Option<bool>,
}

impl FeatureOverrides {
    pub fn apply(self, features: &mut Features) {
        LegacyFeatureToggles {
            include_apply_patch_tool: self.include_apply_patch_tool,
            ..Default::default()
        }
        .apply(features);
    }
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

    pub fn normalize_dependencies(&mut self) {
        if self.enabled(Feature::SpawnCsv) && !self.enabled(Feature::Collab) {
            self.enable(Feature::Collab);
        }
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
    for spec in FEATURES {
        if spec.key == key {
            return Some(spec.id);
        }
    }
    legacy_feature_for_key(key)
}

pub fn canonical_feature_for_key(key: &str) -> Option<Feature> {
    FEATURES
        .iter()
        .find(|spec| spec.key == key)
        .map(|spec| spec.id)
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
        id: Feature::GhostCommit,
        key: "undo",
        default_enabled: false,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::ShellTool,
        key: "shell_tool",
        default_enabled: true,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::UnifiedExec,
        key: "unified_exec",
        default_enabled: true,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::ShellSnapshot,
        key: "shell_snapshot",
        default_enabled: true,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::ChildAgentsMd,
        key: "child_agents_md",
        default_enabled: false,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::ImageDetailOriginal,
        key: "image_detail_original",
        default_enabled: false,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::ApplyPatchFreeform,
        key: "apply_patch_freeform",
        default_enabled: false,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::ExecPermissionApprovals,
        key: "exec_permission_approvals",
        default_enabled: false,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::CodexHooks,
        key: "chaos_dtrace",
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
        id: Feature::EnableRequestCompression,
        key: "enable_request_compression",
        default_enabled: true,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::Collab,
        key: "multi_agent",
        default_enabled: true,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::SpawnCsv,
        key: "enable_fanout",
        default_enabled: false,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::SkillMcpDependencyInstall,
        key: "skill_mcp_dependency_install",
        default_enabled: true,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::SkillEnvVarDependencyPrompt,
        key: "skill_env_var_dependency_prompt",
        default_enabled: false,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::DefaultModeRequestUserInput,
        key: "default_mode_request_user_input",
        default_enabled: false,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::ToolCallMcpElicitation,
        key: "tool_call_mcp_elicitation",
        default_enabled: true,
        under_development: false,
    },
    FeatureSpec {
        id: Feature::Personality,
        key: "personality",
        default_enabled: true,
        under_development: false,
    },
];

// ===== Legacy feature aliases =====

#[derive(Clone, Copy)]
struct Alias {
    legacy_key: &'static str,
    feature: Feature,
}

const ALIASES: &[Alias] = &[
    Alias {
        legacy_key: "experimental_use_unified_exec_tool",
        feature: Feature::UnifiedExec,
    },
    Alias {
        legacy_key: "experimental_use_freeform_apply_patch",
        feature: Feature::ApplyPatchFreeform,
    },
    Alias {
        legacy_key: "include_apply_patch_tool",
        feature: Feature::ApplyPatchFreeform,
    },
    Alias {
        legacy_key: "request_permissions",
        feature: Feature::ExecPermissionApprovals,
    },
    Alias {
        legacy_key: "collab",
        feature: Feature::Collab,
    },
];

pub fn legacy_feature_keys() -> impl Iterator<Item = &'static str> {
    ALIASES.iter().map(|alias| alias.legacy_key)
}

fn legacy_feature_for_key(key: &str) -> Option<Feature> {
    ALIASES
        .iter()
        .find(|alias| alias.legacy_key == key)
        .map(|alias| {
            log_alias(alias.legacy_key, alias.feature);
            alias.feature
        })
}

#[derive(Debug, Default)]
pub struct LegacyFeatureToggles {
    pub include_apply_patch_tool: Option<bool>,
    pub experimental_use_freeform_apply_patch: Option<bool>,
    pub experimental_use_unified_exec_tool: Option<bool>,
}

impl LegacyFeatureToggles {
    pub fn apply(self, features: &mut Features) {
        set_if_some(
            features,
            Feature::ApplyPatchFreeform,
            self.include_apply_patch_tool,
            "include_apply_patch_tool",
        );
        set_if_some(
            features,
            Feature::ApplyPatchFreeform,
            self.experimental_use_freeform_apply_patch,
            "experimental_use_freeform_apply_patch",
        );
        set_if_some(
            features,
            Feature::UnifiedExec,
            self.experimental_use_unified_exec_tool,
            "experimental_use_unified_exec_tool",
        );
    }
}

fn set_if_some(
    features: &mut Features,
    feature: Feature,
    maybe_value: Option<bool>,
    alias_key: &'static str,
) {
    if let Some(enabled) = maybe_value {
        if enabled {
            features.enable(feature);
        } else {
            features.disable(feature);
        }
        log_alias(alias_key, feature);
        features.record_legacy_usage(alias_key, feature);
    }
}

fn log_alias(alias: &str, feature: Feature) {
    let canonical = feature.key();
    if alias == canonical {
        return;
    }
    info!(
        %alias,
        canonical,
        "legacy feature toggle detected; prefer `[features].{canonical}`"
    );
}
