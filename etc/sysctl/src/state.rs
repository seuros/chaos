use crate::config_requirements::ConfigRequirements;
use crate::config_requirements::ConfigRequirementsToml;

use super::fingerprint::record_origins;
use super::fingerprint::version_for_toml;
use super::merge::merge_toml_values;
use chaos_ipc::api::ConfigLayer;
use chaos_ipc::api::ConfigLayerMetadata;
use chaos_ipc::api::ConfigLayerSource;
use chaos_realpath::AbsolutePathBuf;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::PathBuf;
use toml::Value as TomlValue;

/// LoaderOverrides overrides managed configuration inputs (primarily for tests).
#[derive(Debug, Default, Clone)]
pub struct LoaderOverrides {
    pub managed_config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigLayerEntry {
    pub name: ConfigLayerSource,
    pub config: TomlValue,
    pub raw_toml: Option<String>,
    pub version: String,
    pub disabled_reason: Option<String>,
}

impl ConfigLayerEntry {
    pub fn new(name: ConfigLayerSource, config: TomlValue) -> Self {
        let version = version_for_toml(&config);
        Self {
            name,
            config,
            raw_toml: None,
            version,
            disabled_reason: None,
        }
    }

    pub fn new_with_raw_toml(name: ConfigLayerSource, config: TomlValue, raw_toml: String) -> Self {
        let version = version_for_toml(&config);
        Self {
            name,
            config,
            raw_toml: Some(raw_toml),
            version,
            disabled_reason: None,
        }
    }

    pub fn new_disabled(
        name: ConfigLayerSource,
        config: TomlValue,
        disabled_reason: impl Into<String>,
    ) -> Self {
        let version = version_for_toml(&config);
        Self {
            name,
            config,
            raw_toml: None,
            version,
            disabled_reason: Some(disabled_reason.into()),
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled_reason.is_some()
    }

    pub fn raw_toml(&self) -> Option<&str> {
        self.raw_toml.as_deref()
    }

    pub fn metadata(&self) -> ConfigLayerMetadata {
        ConfigLayerMetadata {
            name: self.name.clone(),
            version: self.version.clone(),
        }
    }

    pub fn as_layer(&self) -> ConfigLayer {
        ConfigLayer {
            name: self.name.clone(),
            version: self.version.clone(),
            config: serde_json::to_value(&self.config).unwrap_or(JsonValue::Null),
            disabled_reason: self.disabled_reason.clone(),
        }
    }

    // Get the `.codex/` folder associated with this config layer, if any.
    pub fn config_folder(&self) -> Option<AbsolutePathBuf> {
        match &self.name {
            ConfigLayerSource::Mdm { .. } => None,
            ConfigLayerSource::System { file } => file.parent(),
            ConfigLayerSource::User { file } => file.parent(),
            ConfigLayerSource::Project { dot_codex_folder } => Some(dot_codex_folder.clone()),
            ConfigLayerSource::ProjectMcp { file } => file.parent(),
            ConfigLayerSource::SessionFlags => None,
            ConfigLayerSource::LegacyManagedConfigTomlFromFile { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLayerStackOrdering {
    LowestPrecedenceFirst,
    HighestPrecedenceFirst,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigLayerStack {
    /// Layers are listed from lowest precedence (base) to highest (top), so
    /// later entries in the Vec override earlier ones.
    layers: Vec<ConfigLayerEntry>,

    /// Index into [layers] of the user config layer, if any.
    user_layer_index: Option<usize>,

    /// Constraints that must be enforced when deriving a [Config] from the
    /// layers.
    requirements: ConfigRequirements,

    /// Raw requirements data as loaded from requirements.toml/MDM/legacy
    /// sources. This preserves the original allow-lists so they can be
    /// surfaced via APIs.
    requirements_toml: ConfigRequirementsToml,
}

impl ConfigLayerStack {
    pub fn new(
        layers: Vec<ConfigLayerEntry>,
        requirements: ConfigRequirements,
        requirements_toml: ConfigRequirementsToml,
    ) -> std::io::Result<Self> {
        let user_layer_index = verify_layer_ordering(&layers)?;
        Ok(Self {
            layers,
            user_layer_index,
            requirements,
            requirements_toml,
        })
    }

    /// Returns the user config layer, if any.
    pub fn get_user_layer(&self) -> Option<&ConfigLayerEntry> {
        self.user_layer_index
            .and_then(|index| self.layers.get(index))
    }

    pub fn requirements(&self) -> &ConfigRequirements {
        &self.requirements
    }

    pub fn requirements_toml(&self) -> &ConfigRequirementsToml {
        &self.requirements_toml
    }

    /// Creates a new [ConfigLayerStack] using the specified values to inject a
    /// "user layer" into the stack. If such a layer already exists, it is
    /// replaced; otherwise, it is inserted into the stack at the appropriate
    /// position based on precedence rules.
    pub fn with_user_config(&self, config_toml: &AbsolutePathBuf, user_config: TomlValue) -> Self {
        let user_layer = ConfigLayerEntry::new(
            ConfigLayerSource::User {
                file: config_toml.clone(),
            },
            user_config,
        );

        let mut layers = self.layers.clone();
        match self.user_layer_index {
            Some(index) => {
                layers[index] = user_layer;
                Self {
                    layers,
                    user_layer_index: self.user_layer_index,
                    requirements: self.requirements.clone(),
                    requirements_toml: self.requirements_toml.clone(),
                }
            }
            None => {
                let user_layer_index = match layers
                    .iter()
                    .position(|layer| layer.name.precedence() > user_layer.name.precedence())
                {
                    Some(index) => {
                        layers.insert(index, user_layer);
                        index
                    }
                    None => {
                        layers.push(user_layer);
                        layers.len() - 1
                    }
                };
                Self {
                    layers,
                    user_layer_index: Some(user_layer_index),
                    requirements: self.requirements.clone(),
                    requirements_toml: self.requirements_toml.clone(),
                }
            }
        }
    }

    /// Replace the project-scoped `.mcp.json` layer, or remove it entirely when
    /// `layer` is `None`.
    pub fn with_project_mcp_layer(&self, layer: Option<ConfigLayerEntry>) -> Self {
        let mut layers = self
            .layers
            .iter()
            .filter(|entry| !matches!(entry.name, ConfigLayerSource::ProjectMcp { .. }))
            .cloned()
            .collect::<Vec<_>>();

        if let Some(layer) = layer {
            let insert_at = layers
                .iter()
                .position(|existing| existing.name.precedence() > layer.name.precedence())
                .unwrap_or(layers.len());
            layers.insert(insert_at, layer);
        }

        debug_assert!(
            verify_layer_ordering(&layers).is_ok(),
            "project MCP layer replacement must preserve layer ordering"
        );
        let user_layer_index = layers
            .iter()
            .position(|entry| matches!(entry.name, ConfigLayerSource::User { .. }));
        Self {
            layers,
            user_layer_index,
            requirements: self.requirements.clone(),
            requirements_toml: self.requirements_toml.clone(),
        }
    }

    pub fn effective_config(&self) -> TomlValue {
        let mut merged = TomlValue::Table(toml::map::Map::new());
        for layer in self.get_layers(
            ConfigLayerStackOrdering::LowestPrecedenceFirst,
            /*include_disabled*/ false,
        ) {
            merge_toml_values(&mut merged, &layer.config);
        }
        merged
    }

    pub fn origins(&self) -> HashMap<String, ConfigLayerMetadata> {
        let mut origins = HashMap::new();
        let mut path = Vec::new();

        for layer in self.get_layers(
            ConfigLayerStackOrdering::LowestPrecedenceFirst,
            /*include_disabled*/ false,
        ) {
            record_origins(&layer.config, &layer.metadata(), &mut path, &mut origins);
        }

        origins
    }

    /// Returns the highest-precedence to lowest-precedence layers, so
    /// `ConfigLayerSource::SessionFlags` would be first, if present.
    pub fn layers_high_to_low(&self) -> Vec<&ConfigLayerEntry> {
        self.get_layers(
            ConfigLayerStackOrdering::HighestPrecedenceFirst,
            /*include_disabled*/ false,
        )
    }

    /// Returns the highest-precedence to lowest-precedence layers, so
    /// `ConfigLayerSource::SessionFlags` would be first, if present.
    pub fn get_layers(
        &self,
        ordering: ConfigLayerStackOrdering,
        include_disabled: bool,
    ) -> Vec<&ConfigLayerEntry> {
        let mut layers: Vec<&ConfigLayerEntry> = self
            .layers
            .iter()
            .filter(|layer| include_disabled || !layer.is_disabled())
            .collect();
        if ordering == ConfigLayerStackOrdering::HighestPrecedenceFirst {
            layers.reverse();
        }
        layers
    }
}

/// Ensures precedence ordering of config layers is correct. Returns the index
/// of the user config layer, if any (at most one should exist).
fn verify_layer_ordering(layers: &[ConfigLayerEntry]) -> std::io::Result<Option<usize>> {
    if !layers.iter().map(|layer| &layer.name).is_sorted() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "config layers are not in correct precedence order",
        ));
    }

    // The previous check ensured `layers` is sorted by precedence, so now we
    // further verify that:
    // 1. There is at most one user config layer.
    // 2. Project layers are ordered from root to cwd.
    let mut user_layer_index: Option<usize> = None;
    let mut previous_project_dot_codex_folder: Option<&AbsolutePathBuf> = None;
    for (index, layer) in layers.iter().enumerate() {
        if matches!(layer.name, ConfigLayerSource::User { .. }) {
            if user_layer_index.is_some() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "multiple user config layers found",
                ));
            }
            user_layer_index = Some(index);
        }

        if let ConfigLayerSource::Project {
            dot_codex_folder: current_project_dot_codex_folder,
        } = &layer.name
        {
            if let Some(previous) = previous_project_dot_codex_folder {
                let Some(parent) = previous.as_path().parent() else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "project layer has no parent directory",
                    ));
                };
                if previous == current_project_dot_codex_folder
                    || !current_project_dot_codex_folder
                        .as_path()
                        .ancestors()
                        .any(|ancestor| ancestor == parent)
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "project layers are not ordered from root to cwd",
                    ));
                }
            }
            previous_project_dot_codex_folder = Some(current_project_dot_codex_folder);
        }
    }

    Ok(user_layer_index)
}
