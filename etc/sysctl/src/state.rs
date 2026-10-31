use crate::assets::AssetFolder;
use crate::config_requirements::ConfigRequirements;
use crate::config_requirements::ConfigRequirementsToml;

use super::fingerprint::record_origins;
use super::fingerprint::toml_to_json;
use super::fingerprint::version_for_toml;
use super::merge::merge_toml_values;
use chaos_ipc::api::ConfigLayer;
use chaos_ipc::api::ConfigLayerMetadata;
use chaos_ipc::api::ConfigLayerSource;
use chaos_ipc::api::SettingsScope;
use std::collections::HashMap;
use toml::Value as TomlValue;

/// Overrides for config loading (primarily for tests).
#[derive(Debug, Default, Clone)]
pub struct LoaderOverrides {}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigLayerEntry {
    pub name: ConfigLayerSource,
    pub config: TomlValue,
    pub version: String,
    pub disabled_reason: Option<String>,
}

impl ConfigLayerEntry {
    pub fn new(name: ConfigLayerSource, config: TomlValue) -> Self {
        let version = match &name {
            ConfigLayerSource::UserDatabase { revision } => {
                format!("db:{revision}:{}", version_for_toml(&config))
            }
            ConfigLayerSource::InstallationDatabase { revision } => {
                format!("inst:{revision}:{}", version_for_toml(&config))
            }
            _ => version_for_toml(&config),
        };
        Self {
            name,
            config,
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
            version,
            disabled_reason: Some(disabled_reason.into()),
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled_reason.is_some()
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
            config: toml_to_json(&self.config),
            disabled_reason: self.disabled_reason.clone(),
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

    /// Index into [layers] of the installation config layer, if any.
    installation_layer_index: Option<usize>,

    /// Folders that anchor file-based assets (rules, agents), lowest
    /// precedence first.
    asset_folders: Vec<AssetFolder>,

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
        let (user_layer_index, installation_layer_index) = verify_layer_ordering(&layers)?;
        Ok(Self {
            layers,
            user_layer_index,
            installation_layer_index,
            asset_folders: Vec::new(),
            requirements,
            requirements_toml,
        })
    }

    /// Attach the asset folders (rules/agents anchors) discovered by the loader.
    pub fn with_asset_folders(mut self, asset_folders: Vec<AssetFolder>) -> Self {
        self.asset_folders = asset_folders;
        self
    }

    /// Asset folders in increasing precedence: admin, home, then trusted
    /// project folders from the project root toward cwd.
    pub fn asset_folders(&self) -> &[AssetFolder] {
        &self.asset_folders
    }

    /// Returns the user config layer, if any.
    pub fn get_user_layer(&self) -> Option<&ConfigLayerEntry> {
        self.user_layer_index
            .and_then(|index| self.layers.get(index))
    }

    /// Returns the installation config layer, if any.
    pub fn get_installation_layer(&self) -> Option<&ConfigLayerEntry> {
        self.installation_layer_index
            .and_then(|index| self.layers.get(index))
    }

    pub fn get_database_layer(&self, scope: SettingsScope) -> Option<&ConfigLayerEntry> {
        match scope {
            SettingsScope::Global => self.get_user_layer(),
            SettingsScope::Installation => self.get_installation_layer(),
        }
    }

    /// Owner-controlled settings only: the user database overlaid by the
    /// installation database, without session flags or project files.
    pub fn database_settings(&self) -> Option<TomlValue> {
        let user = self.get_user_layer()?;
        let mut merged = user.config.clone();
        if let Some(installation) = self.get_installation_layer() {
            merge_toml_values(&mut merged, &installation.config);
        }
        Some(merged)
    }

    pub fn requirements(&self) -> &ConfigRequirements {
        &self.requirements
    }

    pub fn requirements_toml(&self) -> &ConfigRequirementsToml {
        &self.requirements_toml
    }

    pub fn with_database_settings(
        &self,
        scope: SettingsScope,
        revision: i64,
        settings: TomlValue,
    ) -> Self {
        let mut layers = self.layers.clone();
        let layer = ConfigLayerEntry::new(scope.layer_source(revision), settings);
        let existing = match scope {
            SettingsScope::Global => self.user_layer_index,
            SettingsScope::Installation => self.installation_layer_index,
        };
        if let Some(index) = existing {
            layers[index] = layer;
        } else {
            let index = layers
                .iter()
                .position(|entry| entry.name.precedence() > layer.name.precedence())
                .unwrap_or(layers.len());
            layers.insert(index, layer);
        }
        let (user_layer_index, installation_layer_index) = database_layer_indexes(&layers);
        Self {
            layers,
            user_layer_index,
            installation_layer_index,
            asset_folders: self.asset_folders.clone(),
            requirements: self.requirements.clone(),
            requirements_toml: self.requirements_toml.clone(),
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
        let (user_layer_index, installation_layer_index) = database_layer_indexes(&layers);
        Self {
            layers,
            user_layer_index,
            installation_layer_index,
            asset_folders: self.asset_folders.clone(),
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

fn database_layer_indexes(layers: &[ConfigLayerEntry]) -> (Option<usize>, Option<usize>) {
    let user = layers
        .iter()
        .position(|entry| matches!(entry.name, ConfigLayerSource::UserDatabase { .. }));
    let installation = layers
        .iter()
        .position(|entry| matches!(entry.name, ConfigLayerSource::InstallationDatabase { .. }));
    (user, installation)
}

/// Ensures precedence ordering of config layers is correct. Returns the indexes
/// of the user and installation config layers, if any (at most one of each).
fn verify_layer_ordering(
    layers: &[ConfigLayerEntry],
) -> std::io::Result<(Option<usize>, Option<usize>)> {
    if !layers.iter().map(|layer| &layer.name).is_sorted() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "config layers are not in correct precedence order",
        ));
    }

    for (label, matcher) in [
        (
            "user",
            (|name: &ConfigLayerSource| matches!(name, ConfigLayerSource::UserDatabase { .. }))
                as fn(&ConfigLayerSource) -> bool,
        ),
        ("installation", |name| {
            matches!(name, ConfigLayerSource::InstallationDatabase { .. })
        }),
    ] {
        if layers.iter().filter(|layer| matcher(&layer.name)).count() > 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("multiple {label} config layers found"),
            ));
        }
    }

    Ok(database_layer_indexes(layers))
}
