use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use chaos_ipc::config_types::CollaborationMode;
use chaos_ipc::config_types::ModeKind;
use chaos_ipc::config_types::Settings;
use chaos_ipc::openai_models::ReasoningEffort;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;

use crate::collaboration_modes::CollaborationModesConfig;
use crate::collaboration_modes::builtin_collaboration_mode_presets;

pub(crate) const DEFAULT_MODE_ID: &str = "default";
pub(crate) const PLAN_MODE_ID: &str = "plan";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ModeCapabilities {
    /// Whether tools that can mutate user or repository state may be exposed.
    pub(crate) mutation: bool,
    /// Whether the mode may ask structured questions.
    pub(crate) request_user_input: bool,
    /// Whether the mode may maintain the implementation checklist.
    pub(crate) update_plan: bool,
}

impl Default for ModeCapabilities {
    fn default() -> Self {
        Self {
            mutation: true,
            request_user_input: true,
            update_plan: true,
        }
    }
}

impl ModeCapabilities {
    pub(crate) fn permits_child(self, child: Self) -> bool {
        (self.mutation || !child.mutation)
            && (self.request_user_input || !child.request_user_input)
            && (self.update_plan || !child.update_plan)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModeDefinition {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) instructions: String,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) capabilities: ModeCapabilities,
    pub(crate) builtin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModeRegistry {
    modes: BTreeMap<String, ModeDefinition>,
}

impl ModeRegistry {
    pub(crate) fn load(
        chaos_home: &Path,
        collaboration_modes_config: CollaborationModesConfig,
    ) -> Result<Self> {
        let mut registry = Self::builtins(collaboration_modes_config)?;
        let modes_dir = chaos_home.join("modes");
        let entries = match fs::read_dir(&modes_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(registry),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to read mode directory {}", modes_dir.display())
                });
            }
        };

        let mut mode_paths = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            })
            .collect::<Vec<_>>();
        mode_paths.sort();

        for path in mode_paths {
            let source = fs::read_to_string(&path)
                .with_context(|| format!("failed to read custom mode {}", path.display()))?;
            let definition = parse_custom_mode(&source)
                .with_context(|| format!("failed to parse custom mode {}", path.display()))?;
            if registry.modes.contains_key(&definition.id) {
                bail!("duplicate or reserved mode id `{}`", definition.id);
            }
            registry.modes.insert(definition.id.clone(), definition);
        }

        Ok(registry)
    }

    fn builtins(collaboration_modes_config: CollaborationModesConfig) -> Result<Self> {
        let presets = builtin_collaboration_mode_presets(collaboration_modes_config);
        let mut modes = BTreeMap::new();
        for preset in presets {
            let mode = preset
                .mode
                .context("built-in collaboration preset is missing its mode")?;
            let id = match mode {
                ModeKind::Default => DEFAULT_MODE_ID,
                ModeKind::Plan => PLAN_MODE_ID,
                _ => continue,
            };
            let instructions = preset
                .developer_instructions
                .flatten()
                .context("built-in collaboration preset is missing instructions")?;
            let reasoning_effort = preset.reasoning_effort.flatten();
            let (description, capabilities) = match mode {
                ModeKind::Default => (
                    "General-purpose execution mode.",
                    ModeCapabilities {
                        request_user_input: collaboration_modes_config
                            .default_mode_request_user_input,
                        ..ModeCapabilities::default()
                    },
                ),
                ModeKind::Plan => (
                    "Conversational planning mode with repository mutation disabled.",
                    ModeCapabilities {
                        mutation: false,
                        request_user_input: true,
                        update_plan: false,
                    },
                ),
                _ => continue,
            };
            modes.insert(
                id.to_string(),
                ModeDefinition {
                    id: id.to_string(),
                    title: preset.name,
                    description: description.to_string(),
                    instructions,
                    reasoning_effort,
                    capabilities,
                    builtin: true,
                },
            );
        }
        Ok(Self { modes })
    }

    pub(crate) fn get(&self, id: &str) -> Option<&ModeDefinition> {
        self.modes.get(id)
    }

    pub(crate) fn ids(&self) -> BTreeSet<String> {
        self.modes.keys().cloned().collect()
    }

    pub(crate) fn apply_mode(
        &self,
        id: &str,
        base: &CollaborationMode,
    ) -> Result<CollaborationMode, String> {
        let definition = self.get(id).ok_or_else(|| format!("unknown mode `{id}`"))?;
        Ok(CollaborationMode {
            // Keep the existing protocol compatible. Custom mode identity is
            // kernel-owned; clients that only understand Default and Plan see
            // custom modes as Default.
            mode: if id == PLAN_MODE_ID {
                ModeKind::Plan
            } else {
                ModeKind::Default
            },
            settings: Settings {
                model: base.model().to_string(),
                reasoning_effort: definition.reasoning_effort.or(base.reasoning_effort()),
                developer_instructions: Some(definition.instructions.clone()),
            },
        })
    }

    pub(crate) fn mode_for_legacy_update<'a>(
        &self,
        current_mode: &'a str,
        collaboration_mode: &CollaborationMode,
    ) -> &'a str {
        match collaboration_mode.mode {
            ModeKind::Plan => PLAN_MODE_ID,
            ModeKind::Default if current_mode == PLAN_MODE_ID => DEFAULT_MODE_ID,
            ModeKind::Default => current_mode,
            _ => current_mode,
        }
    }

    pub(crate) fn resource_json(&self, policy: &ModePolicy) -> Result<String, String> {
        self.resource_json_with_scope(
            policy,
            "This catalog and active mode are scoped to the current ChaOS session/process. A child session cannot change its parent or siblings.",
        )
    }

    pub(crate) fn installation_resource_json(&self, policy: &ModePolicy) -> Result<String, String> {
        self.resource_json_with_scope(
            policy,
            "This is the installation's default root catalog. A live ChaOS session may expose a narrower caller-filtered catalog and a different active mode.",
        )
    }

    fn resource_json_with_scope(&self, policy: &ModePolicy, scope: &str) -> Result<String, String> {
        let modes = policy
            .allowed_modes
            .iter()
            .filter_map(|id| self.get(id))
            .map(|definition| {
                json!({
                    "id": definition.id,
                    "title": definition.title,
                    "description": definition.description,
                    "builtin": definition.builtin,
                    "active": definition.id == policy.active_mode,
                    "capabilities": definition.capabilities,
                })
            })
            .collect::<Vec<_>>();
        serde_json::to_string(&json!({
            "active_mode": policy.active_mode,
            "switching_allowed": policy.switching_allowed,
            "scope": scope,
            "harness": {
                "discovery": "Read chaos://modes to discover only the modes allowed for this session.",
                "switching": "When the switch_mode tool is present, call it with an allowed mode id. The next model sample in the same user turn uses the new mode.",
                "authority": "Modes shape instructions and can remove tools. They never grant filesystem, network, approval, sandbox, delegation, or other security authority."
            },
            "modes": modes,
        }))
        .map_err(|err| format!("failed to serialize ChaOS modes resource: {err}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModePolicy {
    pub(crate) active_mode: String,
    pub(crate) allowed_modes: BTreeSet<String>,
    pub(crate) switching_allowed: bool,
}

impl ModePolicy {
    pub(crate) fn root(registry: &ModeRegistry) -> Self {
        Self {
            active_mode: DEFAULT_MODE_ID.to_string(),
            allowed_modes: registry.ids(),
            switching_allowed: registry.modes.len() > 1,
        }
    }

    pub(crate) fn validate(&self, registry: &ModeRegistry) -> Result<(), String> {
        if self.allowed_modes.is_empty() {
            return Err("mode policy must allow at least one mode".to_string());
        }
        if !self.allowed_modes.contains(&self.active_mode) {
            return Err(format!(
                "active mode `{}` is not present in the allowed mode set",
                self.active_mode
            ));
        }
        for mode in &self.allowed_modes {
            if registry.get(mode).is_none() {
                return Err(format!("mode policy references unknown mode `{mode}`"));
            }
        }
        if self.switching_allowed && self.allowed_modes.len() < 2 {
            return Err(
                "mode switching requires at least two allowed modes in the session".to_string(),
            );
        }
        Ok(())
    }

    pub(crate) fn child(
        &self,
        registry: &ModeRegistry,
        parent_capabilities: ModeCapabilities,
        requested_mode: Option<&str>,
        requested_allowed_modes: Option<&[String]>,
        requested_switching: Option<bool>,
    ) -> Result<Self, String> {
        let capability_limited_parent_modes = self
            .allowed_modes
            .iter()
            .filter(|mode| {
                registry.get(mode).is_some_and(|definition| {
                    parent_capabilities.permits_child(definition.capabilities)
                })
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        let allowed_modes = match requested_allowed_modes {
            Some(requested) => {
                if requested.is_empty() {
                    return Err("allowed_modes must not be empty".to_string());
                }
                let requested = requested.iter().cloned().collect::<BTreeSet<_>>();
                for mode in &requested {
                    registry
                        .get(mode)
                        .ok_or_else(|| format!("unknown child mode `{mode}`"))?;
                    if !capability_limited_parent_modes.contains(mode) {
                        return Err(format!(
                            "child mode `{mode}` is not allowed by the active parent mode"
                        ));
                    }
                }
                requested
            }
            None => match requested_mode {
                Some(mode) => BTreeSet::from([mode.to_string()]),
                None => capability_limited_parent_modes.clone(),
            },
        };

        let active_mode = match requested_mode {
            Some(mode) => {
                registry
                    .get(mode)
                    .ok_or_else(|| format!("unknown child mode `{mode}`"))?;
                if !capability_limited_parent_modes.contains(mode) {
                    return Err(format!(
                        "child mode `{mode}` is not allowed by the active parent mode"
                    ));
                }
                if !allowed_modes.contains(mode) {
                    return Err(format!(
                        "requested child mode `{mode}` is not present in allowed_modes"
                    ));
                }
                mode.to_string()
            }
            None if allowed_modes.contains(&self.active_mode) => self.active_mode.clone(),
            None => allowed_modes
                .iter()
                .next()
                .cloned()
                .ok_or_else(|| "allowed mode set must not be empty".to_string())?,
        };

        let switching_allowed = requested_switching.unwrap_or_else(|| {
            if requested_mode.is_some() {
                false
            } else if requested_allowed_modes.is_some() {
                allowed_modes.len() > 1
            } else {
                self.switching_allowed && allowed_modes.len() > 1
            }
        });
        let policy = Self {
            active_mode,
            allowed_modes,
            switching_allowed,
        };
        policy.validate(registry)?;
        Ok(policy)
    }
}

#[derive(Debug, Deserialize)]
struct CustomModeFrontMatter {
    id: String,
    title: String,
    description: String,
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    capabilities: ModeCapabilities,
}

fn parse_custom_mode(source: &str) -> Result<ModeDefinition> {
    let source = source.strip_prefix("+++\n").ok_or_else(|| {
        anyhow::anyhow!("mode file must begin with TOML front matter delimited by `+++`")
    })?;
    let (front_matter, instructions) = source
        .split_once("\n+++\n")
        .ok_or_else(|| anyhow::anyhow!("mode file is missing the closing `+++` delimiter"))?;
    let metadata: CustomModeFrontMatter =
        toml::from_str(front_matter).context("invalid TOML front matter")?;
    validate_mode_id(&metadata.id)?;
    let instructions = instructions.trim();
    if instructions.is_empty() {
        bail!("mode instructions must not be empty");
    }
    Ok(ModeDefinition {
        id: metadata.id,
        title: metadata.title,
        description: metadata.description,
        instructions: instructions.to_string(),
        reasoning_effort: metadata.reasoning_effort,
        capabilities: metadata.capabilities,
        builtin: false,
    })
}

fn validate_mode_id(id: &str) -> Result<()> {
    if matches!(id, DEFAULT_MODE_ID | PLAN_MODE_ID) {
        bail!("mode id `{id}` is reserved");
    }
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        bail!("mode id must not be empty");
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        bail!("mode id must start with a lowercase ASCII letter or digit");
    }
    if chars.any(|character| {
        !character.is_ascii_lowercase()
            && !character.is_ascii_digit()
            && character != '-'
            && character != '_'
    }) {
        bail!("mode id may contain only lowercase ASCII letters, digits, `-`, and `_`");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_custom_modes_and_omits_instructions_from_resource() {
        let chaos_home = tempfile::tempdir().expect("temp chaos home");
        let modes_dir = chaos_home.path().join("modes");
        fs::create_dir_all(&modes_dir).expect("create modes dir");
        fs::write(
            modes_dir.join("research.md"),
            r#"+++
id = "research"
title = "Research"
description = "Evidence-first investigation."
reasoning_effort = "high"

[capabilities]
mutation = false
request_user_input = true
update_plan = true
+++
Secret full instructions that must not appear in the metadata resource.
"#,
        )
        .expect("write mode");

        let registry = ModeRegistry::load(chaos_home.path(), CollaborationModesConfig::default())
            .expect("load registry");
        let policy = ModePolicy::root(&registry);
        let resource = registry.resource_json(&policy).expect("serialize resource");

        assert!(registry.get("research").is_some());
        assert!(resource.contains("\"research\""));
        assert!(!resource.contains("Secret full instructions"));
    }

    #[test]
    fn requested_child_mode_without_catalog_is_fixed() {
        let registry =
            ModeRegistry::builtins(CollaborationModesConfig::default()).expect("built-in registry");
        let parent = ModePolicy::root(&registry);
        let child = parent
            .child(
                &registry,
                ModeCapabilities::default(),
                Some(PLAN_MODE_ID),
                None,
                None,
            )
            .expect("child policy");

        assert_eq!(
            child,
            ModePolicy {
                active_mode: PLAN_MODE_ID.to_string(),
                allowed_modes: BTreeSet::from([PLAN_MODE_ID.to_string()]),
                switching_allowed: false,
            }
        );
    }

    #[test]
    fn session_resource_only_exposes_policy_allowed_modes() {
        let registry =
            ModeRegistry::builtins(CollaborationModesConfig::default()).expect("built-in registry");
        let policy = ModePolicy {
            active_mode: PLAN_MODE_ID.to_string(),
            allowed_modes: BTreeSet::from([PLAN_MODE_ID.to_string()]),
            switching_allowed: false,
        };

        let resource = registry.resource_json(&policy).expect("serialize resource");
        assert!(
            !resource.contains('\n'),
            "model-facing JSON must be compact"
        );
        let resource: serde_json::Value = serde_json::from_str(&resource).expect("parse resource");

        assert_eq!(resource["active_mode"], PLAN_MODE_ID);
        assert_eq!(resource["switching_allowed"], false);
        assert_eq!(resource["modes"].as_array().expect("mode list").len(), 1);
        assert_eq!(resource["modes"][0]["id"], PLAN_MODE_ID);
    }

    #[test]
    fn child_policy_cannot_broaden_parent_modes() {
        let registry =
            ModeRegistry::builtins(CollaborationModesConfig::default()).expect("built-in registry");
        let parent = ModePolicy {
            active_mode: PLAN_MODE_ID.to_string(),
            allowed_modes: BTreeSet::from([PLAN_MODE_ID.to_string()]),
            switching_allowed: false,
        };
        let error = parent
            .child(
                &registry,
                ModeCapabilities {
                    mutation: false,
                    request_user_input: true,
                    update_plan: false,
                },
                Some(DEFAULT_MODE_ID),
                Some(&[DEFAULT_MODE_ID.to_string()]),
                Some(false),
            )
            .expect_err("must reject broadened child policy");

        assert!(error.contains("not allowed by the active parent mode"));
    }
}
