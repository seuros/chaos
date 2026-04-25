//! Applies agent-role configuration layers on top of an existing session config.
//!
//! Roles are selected at spawn time and are loaded with the same config machinery as
//! `config.toml`. This module resolves built-in and user-defined role files, inserts the role as a
//! high-precedence layer, and preserves the caller's current profile/provider unless the role
//! explicitly takes ownership of model selection. It does not decide when to spawn a sub-agent or
//! which role to use; the multi-agent tool handler owns that orchestration.

use crate::config::AgentRoleConfig;
use crate::config::Config;
use crate::config::ConfigOverrides;
use crate::config::agent_roles::parse_agent_role_file_contents;
use crate::config::agent_roles::resolve_roles_by_topics as sysctl_resolve_roles_by_topics;
use crate::config::deserialize_config_toml_with_base;
use crate::config_loader::ConfigLayerEntry;
use crate::config_loader::ConfigLayerStack;
use crate::config_loader::ConfigLayerStackOrdering;
use crate::config_loader::resolve_relative_paths_in_config_toml;
use anyhow::anyhow;
use chaos_ipc::api::ConfigLayerSource;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;
use toml::Value as TomlValue;

/// The role name used when a caller omits `agent_type`.
pub const DEFAULT_ROLE_NAME: &str = "default";
const AGENT_TYPE_UNAVAILABLE_ERROR: &str = "agent type is currently not available";

/// Applies a named role layer to `config` while preserving caller-owned model selection.
///
/// The role layer is inserted at session-flag precedence so it can override persisted config, but
/// the caller's current `profile` and `model_provider` remain sticky runtime choices unless the
/// role explicitly sets `profile`, explicitly sets `model_provider`, or rewrites the active
/// profile's `model_provider` in place. Rebuilding the config without those overrides would make a
/// spawned agent silently fall back to the default provider, which is the bug this preservation
/// logic avoids.
pub(crate) async fn apply_role_to_config(
    config: &mut Config,
    role_name: Option<&str>,
) -> Result<(), String> {
    let role_name = role_name.unwrap_or(DEFAULT_ROLE_NAME);

    let role = resolve_role_config(config, role_name)
        .cloned()
        .ok_or_else(|| format!("unknown agent_type '{role_name}'"))?;

    apply_role_to_config_inner(config, role_name, &role)
        .await
        .map_err(|err| {
            tracing::warn!("failed to apply role to config: {err}");
            AGENT_TYPE_UNAVAILABLE_ERROR.to_string()
        })
}

async fn apply_role_to_config_inner(
    config: &mut Config,
    role_name: &str,
    role: &AgentRoleConfig,
) -> anyhow::Result<()> {
    let is_built_in = !config.agent_roles.contains_key(role_name);
    let Some(config_file) = role.config_file.as_ref() else {
        return Ok(());
    };
    let role_layer_toml = load_role_layer_toml(config, config_file, is_built_in, role_name).await?;
    let (preserve_current_profile, preserve_current_provider) =
        preservation_policy(config, &role_layer_toml);

    *config = reload::build_next_config(
        config,
        role_layer_toml,
        preserve_current_profile,
        preserve_current_provider,
    )?;
    Ok(())
}

async fn load_role_layer_toml(
    config: &Config,
    config_file: &Path,
    is_built_in: bool,
    role_name: &str,
) -> anyhow::Result<TomlValue> {
    let (role_config_toml, role_config_base) = if is_built_in {
        let role_config_contents = built_in::config_file_contents(config_file)
            .map(str::to_owned)
            .ok_or(anyhow!("No corresponding config content"))?;
        let role_config_toml = parse_agent_role_file_contents(
            &role_config_contents,
            config_file,
            config.chaos_home.as_path(),
            Some(role_name),
        )
        .map_err(|e| anyhow!(e))?
        .config;
        (role_config_toml, config.chaos_home.as_path())
    } else {
        let role_config_contents = tokio::fs::read_to_string(config_file).await?;
        let role_config_base = config_file
            .parent()
            .ok_or(anyhow!("No corresponding config content"))?;
        let role_config_toml = parse_agent_role_file_contents(
            &role_config_contents,
            config_file,
            role_config_base,
            Some(role_name),
        )?
        .config;
        (role_config_toml, role_config_base)
    };

    deserialize_config_toml_with_base(role_config_toml.clone(), role_config_base)?;
    Ok(resolve_relative_paths_in_config_toml(
        role_config_toml,
        role_config_base,
    )?)
}

pub(crate) fn resolve_role_config<'a>(
    config: &'a Config,
    role_name: &str,
) -> Option<&'a AgentRoleConfig> {
    config
        .agent_roles
        .get(role_name)
        .or_else(|| built_in::configs().get(role_name))
}

fn preservation_policy(config: &Config, role_layer_toml: &TomlValue) -> (bool, bool) {
    let role_selects_provider = role_layer_toml.get("model_provider").is_some();
    let role_selects_profile = role_layer_toml.get("profile").is_some();
    let role_updates_active_profile_provider = config
        .active_profile
        .as_ref()
        .and_then(|active_profile| {
            role_layer_toml
                .get("profiles")
                .and_then(TomlValue::as_table)
                .and_then(|profiles| profiles.get(active_profile))
                .and_then(TomlValue::as_table)
                .map(|profile| profile.contains_key("model_provider"))
        })
        .unwrap_or(false);
    let preserve_current_profile = !role_selects_provider && !role_selects_profile;
    let preserve_current_provider =
        preserve_current_profile && !role_updates_active_profile_provider;
    (preserve_current_profile, preserve_current_provider)
}

mod reload {
    use super::{
        Config, ConfigLayerEntry, ConfigLayerSource, ConfigLayerStack, ConfigLayerStackOrdering,
        ConfigOverrides, TomlValue, deserialize_config_toml_with_base,
    };

    pub(super) fn build_next_config(
        config: &Config,
        role_layer_toml: TomlValue,
        preserve_current_profile: bool,
        preserve_current_provider: bool,
    ) -> anyhow::Result<Config> {
        let active_profile_name = preserve_current_profile
            .then_some(config.active_profile.as_deref())
            .flatten();
        let config_layer_stack =
            build_config_layer_stack(config, &role_layer_toml, active_profile_name)?;
        let mut merged_config = deserialize_effective_config(config, &config_layer_stack)?;
        if preserve_current_profile {
            merged_config.profile = None;
        }

        let mut next_config = Config::load_config_with_layer_stack(
            merged_config,
            reload_overrides(config, preserve_current_provider),
            config.chaos_home.clone(),
            config_layer_stack,
        )?;
        if preserve_current_profile {
            next_config.active_profile = config.active_profile.clone();
        }
        Ok(next_config)
    }

    fn build_config_layer_stack(
        config: &Config,
        role_layer_toml: &TomlValue,
        active_profile_name: Option<&str>,
    ) -> anyhow::Result<ConfigLayerStack> {
        let mut layers = existing_layers(config);
        if let Some(resolved_profile_layer) =
            resolved_profile_layer(config, &layers, role_layer_toml, active_profile_name)?
        {
            insert_layer(&mut layers, resolved_profile_layer);
        }
        insert_layer(&mut layers, role_layer(role_layer_toml.clone()));
        Ok(ConfigLayerStack::new(
            layers,
            config.config_layer_stack.requirements().clone(),
            config.config_layer_stack.requirements_toml().clone(),
        )?)
    }

    fn resolved_profile_layer(
        config: &Config,
        existing_layers: &[ConfigLayerEntry],
        role_layer_toml: &TomlValue,
        active_profile_name: Option<&str>,
    ) -> anyhow::Result<Option<ConfigLayerEntry>> {
        let Some(active_profile_name) = active_profile_name else {
            return Ok(None);
        };

        let mut layers = existing_layers.to_vec();
        insert_layer(&mut layers, role_layer(role_layer_toml.clone()));
        let merged_config = deserialize_effective_config(
            config,
            &ConfigLayerStack::new(
                layers,
                config.config_layer_stack.requirements().clone(),
                config.config_layer_stack.requirements_toml().clone(),
            )?,
        )?;
        let resolved_profile =
            merged_config.get_config_profile(Some(active_profile_name.to_string()))?;
        Ok(Some(ConfigLayerEntry::new(
            ConfigLayerSource::SessionFlags,
            TomlValue::try_from(resolved_profile)?,
        )))
    }

    fn deserialize_effective_config(
        config: &Config,
        config_layer_stack: &ConfigLayerStack,
    ) -> anyhow::Result<crate::config::ConfigToml> {
        Ok(deserialize_config_toml_with_base(
            config_layer_stack.effective_config(),
            &config.chaos_home,
        )?)
    }

    fn existing_layers(config: &Config) -> Vec<ConfigLayerEntry> {
        config
            .config_layer_stack
            .get_layers(
                ConfigLayerStackOrdering::LowestPrecedenceFirst,
                /*include_disabled*/ true,
            )
            .into_iter()
            .cloned()
            .collect()
    }

    fn insert_layer(layers: &mut Vec<ConfigLayerEntry>, layer: ConfigLayerEntry) {
        let insertion_index =
            layers.partition_point(|existing_layer| existing_layer.name <= layer.name);
        layers.insert(insertion_index, layer);
    }

    fn role_layer(role_layer_toml: TomlValue) -> ConfigLayerEntry {
        ConfigLayerEntry::new(ConfigLayerSource::SessionFlags, role_layer_toml)
    }

    fn reload_overrides(config: &Config, preserve_current_provider: bool) -> ConfigOverrides {
        ConfigOverrides {
            cwd: Some(config.cwd.clone()),
            model_provider: preserve_current_provider.then(|| config.model_provider_id.clone()),
            active_project_trust: Some(config.active_project_trust.clone()),
            alcatraz_linux_exe: config.alcatraz_linux_exe.clone(),
            alcatraz_freebsd_exe: config.alcatraz_freebsd_exe.clone(),
            ..Default::default()
        }
    }
}

pub(crate) mod spawn_tool_spec {
    use super::{
        AgentRoleConfig, BTreeMap, BTreeSet, DEFAULT_ROLE_NAME, TomlValue, built_in,
        parse_agent_role_file_contents,
    };

    /// Builds the spawn-agent tool description text from built-in and configured roles.
    pub(crate) fn build(user_defined_agent_roles: &BTreeMap<String, AgentRoleConfig>) -> String {
        let built_in_roles = built_in::configs();
        build_from_configs(built_in_roles, user_defined_agent_roles)
    }

    // This function is not inlined for testing purpose.
    fn build_from_configs(
        built_in_roles: &BTreeMap<String, AgentRoleConfig>,
        user_defined_roles: &BTreeMap<String, AgentRoleConfig>,
    ) -> String {
        let mut seen = BTreeSet::new();
        let mut formatted_roles = Vec::new();
        for (name, declaration) in user_defined_roles {
            if seen.insert(name.as_str()) {
                formatted_roles.push(format_role(name, declaration));
            }
        }
        for (name, declaration) in built_in_roles {
            if seen.insert(name.as_str()) {
                formatted_roles.push(format_role(name, declaration));
            }
        }

        format!(
            "Optional type name for the new agent. If omitted, `{DEFAULT_ROLE_NAME}` is used.\nAvailable roles:\n{}",
            formatted_roles.join("\n"),
        )
    }

    fn format_role(name: &str, declaration: &AgentRoleConfig) -> String {
        let locked_settings_note = declaration
            .config_file
            .as_ref()
            .and_then(|config_file| {
                let contents = built_in::config_file_contents(config_file)
                    .map(str::to_owned)
                    .or_else(|| std::fs::read_to_string(config_file).ok())?;
                parse_agent_role_file_contents(
                    &contents,
                    config_file,
                    std::path::Path::new("."),
                    Some(name),
                )
                .ok()
            })
            .map(|parsed| {
                let model = parsed.config.get("model").and_then(TomlValue::as_str);
                let reasoning_effort = parsed
                    .config
                    .get("model_reasoning_effort")
                    .and_then(TomlValue::as_str);

                match (model, reasoning_effort) {
                    (Some(model), Some(reasoning_effort)) => {
                        format!(" [model={model}, reasoning_effort={reasoning_effort}, locked]")
                    }
                    (Some(model), None) => format!(" [model={model}, locked]"),
                    (None, Some(reasoning_effort)) => {
                        format!(" [reasoning_effort={reasoning_effort}, locked]")
                    }
                    (None, None) => String::new(),
                }
            })
            .unwrap_or_default();
        format!("{name}{locked_settings_note}")
    }
}

/// Collects all roles (user-defined and built-in) whose topics overlap with
/// `requested_topics`. Returns a `Vec` for the caller to sample from. An empty result
/// means no role covers those topics; the caller should fall back to `default` and warn
/// the user that the topics are unregistered.
pub(crate) fn collect_roles_by_topics<'a>(
    config: &'a Config,
    requested_topics: &[String],
) -> Vec<(&'a str, &'a AgentRoleConfig)> {
    let mut matches: Vec<(&str, &AgentRoleConfig)> = Vec::new();

    // User-defined roles take precedence and are checked first.
    let user_matches = sysctl_resolve_roles_by_topics(&config.agent_roles, requested_topics);
    matches.extend(user_matches);

    // Also search built-ins, skipping any already shadowed by user-defined names.
    let user_names: BTreeSet<&str> = config.agent_roles.keys().map(String::as_str).collect();
    for (name, role) in sysctl_resolve_roles_by_topics(built_in::configs(), requested_topics) {
        if !user_names.contains(name) {
            matches.push((name, role));
        }
    }

    matches
}

mod built_in {
    use super::{AgentRoleConfig, BTreeMap, LazyLock, Path, parse_agent_role_file_contents};
    use include_dir::Dir;
    use include_dir::include_dir;

    /// Core operational roles: default, scout, task, sentinel, …
    static BUILTINS_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/minions/builtins");

    /// Personality overlays: dhh, gordon, fireship, primeagen, carmack, …
    /// Drop a new `.md` file here — no code changes required.
    static PERSONAS_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/minions/personas");

    /// Returns the cached role declarations from both builtins and personas.
    pub(super) fn configs() -> &'static BTreeMap<String, AgentRoleConfig> {
        static CONFIG: LazyLock<BTreeMap<String, AgentRoleConfig>> = LazyLock::new(|| {
            [&BUILTINS_DIR, &PERSONAS_DIR]
                .iter()
                .flat_map(|dir| dir.files())
                .filter(|f| f.path().extension().is_some_and(|e| e == "md"))
                .filter_map(|file| {
                    let path = file.path();
                    let stem = path.file_stem()?.to_str()?;
                    let content = file.contents_utf8()?;
                    let parsed = parse_agent_role_file_contents(
                        content,
                        path,
                        std::path::Path::new("."),
                        Some(stem),
                    )
                    .unwrap_or_else(|err| {
                        panic!("built-in role file {} is invalid: {err}", path.display())
                    });
                    let config_file = if parsed.config.as_table().is_some_and(|t| !t.is_empty()) {
                        Some(path.to_path_buf())
                    } else {
                        None
                    };
                    Some((
                        parsed.role_name.clone(),
                        AgentRoleConfig {
                            description: parsed.description,
                            config_file,
                            nickname_candidates: parsed.nickname_candidates,
                            topics: parsed.topics,
                            catchphrases: parsed.catchphrases,
                        },
                    ))
                })
                .collect()
        });
        &CONFIG
    }

    /// Resolves a built-in role `config_file` path to its embedded content.
    /// Checks builtins first, then personas.
    pub(super) fn config_file_contents(path: &Path) -> Option<&'static str> {
        BUILTINS_DIR
            .get_file(path)
            .or_else(|| PERSONAS_DIR.get_file(path))
            .and_then(|f| f.contents_utf8())
    }
}

#[cfg(test)]
#[path = "role_tests.rs"]
mod tests;
