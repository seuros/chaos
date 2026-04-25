#[cfg(test)]
mod tests;

use crate::config::ConfigToml;
use crate::git_info::resolve_root_git_project_for_trust;
use chaos_ipc::api::ConfigLayerSource;
use chaos_ipc::config_types::TrustLevel;
use chaos_realpath::AbsolutePathBuf;
use chaos_realpath::AbsolutePathBufGuard;
use chaos_sysctl::CONFIG_TOML_FILE;
use chaos_sysctl::ConfigRequirementsWithSources;
use chaos_sysctl::types::McpServerConfig;
use serde::Deserialize;
use serde::Serialize;
use std::fs::canonicalize as normalize_path;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use toml::Value as TomlValue;

pub use chaos_sysctl::AppRequirementToml;
pub use chaos_sysctl::AppsRequirementsToml;
pub use chaos_sysctl::ConfigError;
pub use chaos_sysctl::ConfigLayerEntry;
pub use chaos_sysctl::ConfigLayerStack;
pub use chaos_sysctl::ConfigLayerStackOrdering;
pub use chaos_sysctl::ConfigLoadError;
pub use chaos_sysctl::ConfigRequirements;
pub use chaos_sysctl::ConfigRequirementsToml;
pub use chaos_sysctl::ConstrainedWithSource;
pub use chaos_sysctl::FeatureRequirementsToml;
pub use chaos_sysctl::LoaderOverrides;
pub use chaos_sysctl::McpServerIdentity;
pub use chaos_sysctl::McpServerRequirement;
pub use chaos_sysctl::NetworkConstraints;
pub use chaos_sysctl::NetworkRequirementsToml;
pub use chaos_sysctl::RequirementSource;
pub use chaos_sysctl::ResidencyRequirement;
pub use chaos_sysctl::SandboxModeRequirement;
pub use chaos_sysctl::Sourced;
pub use chaos_sysctl::TextPosition;
pub use chaos_sysctl::TextRange;
pub use chaos_sysctl::WebSearchModeRequirement;
pub(crate) use chaos_sysctl::build_cli_overrides_layer;
pub(crate) use chaos_sysctl::config_error_from_toml;
pub use chaos_sysctl::format_config_error;
pub use chaos_sysctl::format_config_error_with_source;
pub(crate) use chaos_sysctl::io_error_from_config_error;
pub use chaos_sysctl::merge_toml_values;
#[cfg(test)]
pub(crate) use chaos_sysctl::version_for_toml;

/// On Unix systems, load default settings from this file path, if present.
/// Note that /etc/chaos/ is treated as a "config folder," so subfolders such
/// as skills/ and rules/ will also be honored.
pub const SYSTEM_CONFIG_TOML_FILE_UNIX: &str = "/etc/chaos/config.toml";

const DEFAULT_PROJECT_ROOT_MARKERS: &[&str] = &[".git"];
const PROJECT_MCP_JSON_FILE: &str = ".mcp.json";

pub(crate) async fn first_layer_config_error(layers: &ConfigLayerStack) -> Option<ConfigError> {
    chaos_sysctl::first_layer_config_error::<ConfigToml>(layers, CONFIG_TOML_FILE).await
}

pub(crate) async fn first_layer_config_error_from_entries(
    layers: &[ConfigLayerEntry],
) -> Option<ConfigError> {
    chaos_sysctl::first_layer_config_error_from_entries::<ConfigToml>(layers, CONFIG_TOML_FILE)
        .await
}

/// To build up the set of admin-enforced constraints, we load from the system
/// requirements file `/etc/chaos/requirements.toml`.
///
/// Configuration is built up from multiple layers in the following order:
///
/// - system    `/etc/chaos/config.toml`
/// - user      `${CHAOS_HOME}/config.toml`
/// - cwd       `${PWD}/config.toml` (loaded but disabled when the directory is untrusted)
/// - tree      parent directories up to root looking for `./.chaos/config.toml`
///   (loaded but disabled when untrusted)
/// - repo      `$(git rev-parse --show-toplevel)/.chaos/config.toml` (loaded
///   but disabled when untrusted)
/// - runtime   e.g., --config flags, model selector in UI
///
/// When loading the config stack for a thread, there should be a `cwd`
/// associated with it such that `cwd` should be `Some(...)`. Only for
/// thread-agnostic config loading (e.g., for the app server's `/config`
/// endpoint) should `cwd` be `None`.
pub async fn load_config_layers_state(
    chaos_home: &Path,
    cwd: Option<AbsolutePathBuf>,
    cli_overrides: &[(String, TomlValue)],
    overrides: LoaderOverrides,
) -> io::Result<ConfigLayerStack> {
    let mut config_requirements_toml = ConfigRequirementsWithSources::default();

    let _overrides = overrides;

    // Honor the system requirements.toml location.
    let requirements_toml_file = system_requirements_toml_file()?;
    load_requirements_toml(&mut config_requirements_toml, requirements_toml_file).await?;

    let mut layers = Vec::<ConfigLayerEntry>::new();

    let cli_overrides_layer = if cli_overrides.is_empty() {
        None
    } else {
        let cli_overrides_layer = build_cli_overrides_layer(cli_overrides);
        let base_dir = cwd
            .as_ref()
            .map(AbsolutePathBuf::as_path)
            .unwrap_or(chaos_home);
        Some(resolve_relative_paths_in_config_toml(
            cli_overrides_layer,
            base_dir,
        )?)
    };

    // Include an entry for the "system" config folder, loading its config.toml,
    // if it exists.
    let system_config_toml_file = system_config_toml_file()?;
    let system_layer =
        load_config_toml_for_required_layer(&system_config_toml_file, |config_toml| {
            ConfigLayerEntry::new(
                ConfigLayerSource::System {
                    file: system_config_toml_file.clone(),
                },
                config_toml,
            )
        })
        .await?;
    layers.push(system_layer);

    // Add a layer for $CHAOS_HOME/config.toml if it exists. Note if the file
    // exists, but is malformed, then this error should be propagated to the
    // user.
    let user_file = AbsolutePathBuf::resolve_path_against_base(CONFIG_TOML_FILE, chaos_home)?;
    let user_layer = load_config_toml_for_required_layer(&user_file, |config_toml| {
        ConfigLayerEntry::new(
            ConfigLayerSource::User {
                file: user_file.clone(),
            },
            config_toml,
        )
    })
    .await?;
    layers.push(user_layer);

    if let Some(cwd) = cwd {
        let mut merged_so_far = TomlValue::Table(toml::map::Map::new());
        for layer in &layers {
            merge_toml_values(&mut merged_so_far, &layer.config);
        }
        if let Some(cli_overrides_layer) = cli_overrides_layer.as_ref() {
            merge_toml_values(&mut merged_so_far, cli_overrides_layer);
        }

        let project_root_markers = match project_root_markers_from_config(&merged_so_far) {
            Ok(markers) => markers.unwrap_or_else(default_project_root_markers),
            Err(err) => {
                if let Some(config_error) = first_layer_config_error_from_entries(&layers).await {
                    return Err(io_error_from_config_error(
                        io::ErrorKind::InvalidData,
                        config_error,
                        /*source*/ None,
                    ));
                }
                return Err(err);
            }
        };
        let sqlite_home = sqlite_home_from_merged_config(&merged_so_far, chaos_home);
        let project_trust_context =
            match project_trust_context(&cwd, &project_root_markers, &sqlite_home).await {
                Ok(context) => context,
                Err(err) => {
                    let source = err
                        .get_ref()
                        .and_then(|err| err.downcast_ref::<toml::de::Error>())
                        .cloned();
                    if let Some(config_error) = first_layer_config_error_from_entries(&layers).await
                    {
                        return Err(io_error_from_config_error(
                            io::ErrorKind::InvalidData,
                            config_error,
                            source,
                        ));
                    }
                    return Err(err);
                }
            };
        let project_layers = load_project_layers(
            &cwd,
            &project_trust_context.project_root,
            &project_trust_context,
            chaos_home,
        )
        .await?;
        layers.extend(project_layers);
    }

    // Add a layer for runtime overrides from the CLI or UI, if any exist.
    if let Some(cli_overrides_layer) = cli_overrides_layer {
        layers.push(ConfigLayerEntry::new(
            ConfigLayerSource::SessionFlags,
            cli_overrides_layer,
        ));
    }

    ConfigLayerStack::new(
        layers,
        config_requirements_toml.clone().try_into()?,
        config_requirements_toml.into_toml(),
    )
}

/// Attempts to load a config.toml file from `config_toml`.
/// - If the file exists and is valid TOML, passes the parsed `toml::Value` to
///   `create_entry` and returns the resulting layer entry.
/// - If the file does not exist, uses an empty `Table` with `create_entry` and
///   returns the resulting layer entry.
/// - If there is an error reading the file or parsing the TOML, returns an
///   error.
async fn load_config_toml_for_required_layer(
    config_toml: impl AsRef<Path>,
    create_entry: impl FnOnce(TomlValue) -> ConfigLayerEntry,
) -> io::Result<ConfigLayerEntry> {
    let toml_file = config_toml.as_ref();
    let toml_value = match tokio::fs::read_to_string(toml_file).await {
        Ok(contents) => {
            let config: TomlValue = toml::from_str(&contents).map_err(|err| {
                let config_error = config_error_from_toml(toml_file, &contents, err.clone());
                io_error_from_config_error(io::ErrorKind::InvalidData, config_error, Some(err))
            })?;
            let config_parent = toml_file.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Config file {} has no parent directory",
                        toml_file.display()
                    ),
                )
            })?;
            resolve_relative_paths_in_config_toml(config, config_parent)
        }
        Err(e) => {
            if e.kind() == io::ErrorKind::NotFound {
                Ok(TomlValue::Table(toml::map::Map::new()))
            } else {
                Err(io::Error::new(
                    e.kind(),
                    format!("Failed to read config file {}: {e}", toml_file.display()),
                ))
            }
        }
    }?;

    Ok(create_entry(toml_value))
}

/// If available, apply requirements from the platform system
/// `requirements.toml` location to `config_requirements_toml` by filling in
/// any unset fields.
async fn load_requirements_toml(
    config_requirements_toml: &mut ConfigRequirementsWithSources,
    requirements_toml_file: impl AsRef<Path>,
) -> io::Result<()> {
    let requirements_toml_file =
        AbsolutePathBuf::from_absolute_path(requirements_toml_file.as_ref())?;
    match tokio::fs::read_to_string(&requirements_toml_file).await {
        Ok(contents) => {
            let requirements_config: ConfigRequirementsToml =
                toml::from_str(&contents).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Error parsing requirements file {}: {e}",
                            requirements_toml_file.as_ref().display(),
                        ),
                    )
                })?;
            config_requirements_toml.merge_unset_fields(
                RequirementSource::SystemRequirementsToml {
                    file: requirements_toml_file.clone(),
                },
                requirements_config,
            );
        }
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                return Err(io::Error::new(
                    e.kind(),
                    format!(
                        "Failed to read requirements file {}: {e}",
                        requirements_toml_file.as_ref().display(),
                    ),
                ));
            }
        }
    }

    Ok(())
}

fn system_requirements_toml_file() -> io::Result<AbsolutePathBuf> {
    AbsolutePathBuf::from_absolute_path(Path::new("/etc/chaos/requirements.toml"))
}

fn system_config_toml_file() -> io::Result<AbsolutePathBuf> {
    AbsolutePathBuf::from_absolute_path(Path::new(SYSTEM_CONFIG_TOML_FILE_UNIX))
}

/// Reads `project_root_markers` from the [toml::Value] produced by merging
/// `config.toml` from the config layers in the stack preceding
/// [ConfigLayerSource::Project].
///
/// Invariants:
/// - If `project_root_markers` is not specified, returns `Ok(None)`.
/// - If `project_root_markers` is specified, returns `Ok(Some(markers))` where
///   `markers` is a `Vec<String>` (including `Ok(Some(Vec::new()))` for an
///   empty array, which indicates that root detection should be disabled).
/// - Returns an error if `project_root_markers` is specified but is not an
///   array of strings.
pub(crate) fn project_root_markers_from_config(
    config: &TomlValue,
) -> io::Result<Option<Vec<String>>> {
    let Some(table) = config.as_table() else {
        return Ok(None);
    };
    let Some(markers_value) = table.get("project_root_markers") else {
        return Ok(None);
    };
    let TomlValue::Array(entries) = markers_value else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "project_root_markers must be an array of strings",
        ));
    };
    if entries.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let mut markers = Vec::new();
    for entry in entries {
        let Some(marker) = entry.as_str() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "project_root_markers must be an array of strings",
            ));
        };
        markers.push(marker.to_string());
    }
    Ok(Some(markers))
}

pub(crate) fn default_project_root_markers() -> Vec<String> {
    DEFAULT_PROJECT_ROOT_MARKERS
        .iter()
        .map(ToString::to_string)
        .collect()
}

pub(crate) fn project_root_markers_from_stack(
    config_layer_stack: &ConfigLayerStack,
) -> Vec<String> {
    let mut merged = TomlValue::Table(toml::map::Map::new());
    for layer in config_layer_stack.get_layers(
        ConfigLayerStackOrdering::LowestPrecedenceFirst,
        /*include_disabled*/ false,
    ) {
        if matches!(
            layer.name,
            ConfigLayerSource::Project { .. } | ConfigLayerSource::ProjectMcp { .. }
        ) {
            continue;
        }
        merge_toml_values(&mut merged, &layer.config);
    }

    match project_root_markers_from_config(&merged) {
        Ok(Some(markers)) => markers,
        Ok(None) => default_project_root_markers(),
        Err(err) => {
            tracing::warn!("invalid project_root_markers: {err}");
            default_project_root_markers()
        }
    }
}

pub(crate) async fn resolve_active_project_trust(
    sqlite_home: &Path,
    cwd: &Path,
    config_layer_stack: &ConfigLayerStack,
) -> io::Result<crate::config::ProjectTrust> {
    let cwd = normalize_absolute_path_for_trust(cwd)?;
    let project_root_markers = project_root_markers_from_stack(config_layer_stack);
    let trust_context = project_trust_context(&cwd, &project_root_markers, sqlite_home).await?;
    Ok(crate::config::ProjectTrust {
        trust_level: trust_context.decision_for_dir(&cwd).trust_level,
    })
}

pub(crate) fn find_project_root_sync(cwd: &Path, project_root_markers: &[String]) -> PathBuf {
    if project_root_markers.is_empty() {
        return cwd.to_path_buf();
    }

    for ancestor in cwd.ancestors() {
        for marker in project_root_markers {
            if ancestor.join(marker).exists() {
                return ancestor.to_path_buf();
            }
        }
    }

    cwd.to_path_buf()
}

pub(crate) fn project_mcp_json_path_for_stack(
    config_layer_stack: &ConfigLayerStack,
    cwd: &Path,
) -> PathBuf {
    let project_root_markers = project_root_markers_from_stack(config_layer_stack);
    find_project_root_sync(cwd, &project_root_markers).join(PROJECT_MCP_JSON_FILE)
}

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ProjectMcpJson {
    #[serde(rename = "mcpServers")]
    mcp_servers: std::collections::HashMap<String, McpServerConfig>,
}

#[derive(Debug, Serialize)]
struct ProjectMcpToml {
    mcp_servers: std::collections::HashMap<String, McpServerConfig>,
}

pub(crate) fn parse_project_mcp_json(contents: &str) -> io::Result<TomlValue> {
    let parsed: ProjectMcpJson = serde_json::from_str(contents).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to parse {PROJECT_MCP_JSON_FILE}: {err}"),
        )
    })?;

    TomlValue::try_from(ProjectMcpToml {
        mcp_servers: parsed.mcp_servers,
    })
    .map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to convert {PROJECT_MCP_JSON_FILE} into config layer: {err}"),
        )
    })
}

struct ProjectTrustContext {
    project_root: AbsolutePathBuf,
    project_root_key: String,
    repo_root_key: Option<String>,
    trust_levels_by_path: std::collections::HashMap<String, TrustLevel>,
}

#[derive(Deserialize)]
struct TrustStorageConfigToml {
    sqlite_home: Option<AbsolutePathBuf>,
}

struct ProjectTrustDecision {
    trust_level: Option<TrustLevel>,
    trust_key: String,
}

impl ProjectTrustDecision {
    fn is_trusted(&self) -> bool {
        matches!(self.trust_level, Some(TrustLevel::Trusted))
    }
}

impl ProjectTrustContext {
    fn decision_for_dir(&self, dir: &AbsolutePathBuf) -> ProjectTrustDecision {
        let dir_key = dir.as_path().to_string_lossy().to_string();
        if let Some(trust_level) = self.trust_levels_by_path.get(&dir_key).copied() {
            return ProjectTrustDecision {
                trust_level: Some(trust_level),
                trust_key: dir_key,
            };
        }

        if let Some(trust_level) = self
            .trust_levels_by_path
            .get(&self.project_root_key)
            .copied()
        {
            return ProjectTrustDecision {
                trust_level: Some(trust_level),
                trust_key: self.project_root_key.clone(),
            };
        }

        if let Some(repo_root_key) = self.repo_root_key.as_ref()
            && let Some(trust_level) = self.trust_levels_by_path.get(repo_root_key).copied()
        {
            return ProjectTrustDecision {
                trust_level: Some(trust_level),
                trust_key: repo_root_key.clone(),
            };
        }

        ProjectTrustDecision {
            trust_level: None,
            trust_key: self
                .repo_root_key
                .clone()
                .unwrap_or_else(|| self.project_root_key.clone()),
        }
    }

    fn disabled_reason_for_dir(&self, dir: &AbsolutePathBuf) -> Option<String> {
        let decision = self.decision_for_dir(dir);
        if decision.is_trusted() {
            return None;
        }

        let trust_key = decision.trust_key.as_str();
        match decision.trust_level {
            Some(TrustLevel::Untrusted) => Some(format!(
                "{trust_key} is marked as untrusted in ChaOS project trust state. To load config.toml, mark it trusted."
            )),
            _ => Some(format!("To load config.toml, trust {trust_key} in ChaOS.")),
        }
    }
}

fn project_layer_entry(
    trust_context: &ProjectTrustContext,
    dot_chaos_folder: &AbsolutePathBuf,
    layer_dir: &AbsolutePathBuf,
    config: TomlValue,
    config_toml_exists: bool,
) -> ConfigLayerEntry {
    let source = ConfigLayerSource::Project {
        dot_codex_folder: dot_chaos_folder.clone(),
    };

    if config_toml_exists && let Some(reason) = trust_context.disabled_reason_for_dir(layer_dir) {
        ConfigLayerEntry::new_disabled(source, config, reason)
    } else {
        ConfigLayerEntry::new(source, config)
    }
}

fn project_mcp_layer_entry(
    trust_context: &ProjectTrustContext,
    file: &AbsolutePathBuf,
    config: TomlValue,
) -> ConfigLayerEntry {
    let source = ConfigLayerSource::ProjectMcp { file: file.clone() };
    let Some(project_root) = file.parent() else {
        return ConfigLayerEntry::new(source, config);
    };

    if let Some(reason) = trust_context.disabled_reason_for_dir(&project_root) {
        ConfigLayerEntry::new_disabled(source, config, reason)
    } else {
        ConfigLayerEntry::new(source, config)
    }
}

async fn project_trust_context(
    cwd: &AbsolutePathBuf,
    project_root_markers: &[String],
    sqlite_home: &Path,
) -> io::Result<ProjectTrustContext> {
    let project_root = find_project_root(cwd, project_root_markers).await?;
    let project_root = normalize_absolute_path_for_trust(project_root.as_path())?;

    let project_root_key = project_root.as_path().to_string_lossy().to_string();
    let repo_root = resolve_root_git_project_for_trust(cwd.as_path())
        .map(|root| normalize_absolute_path_for_trust(root.as_path()))
        .transpose()?;
    let repo_root_key = repo_root
        .as_ref()
        .map(|root| root.as_path().to_string_lossy().to_string());

    let mut trust_candidates = std::collections::BTreeSet::new();
    for ancestor in cwd.as_path().ancestors() {
        trust_candidates.insert(crate::runtime_db::normalize_cwd_for_runtime_db(ancestor));
    }
    trust_candidates.insert(project_root.as_path().to_path_buf());
    if let Some(repo_root) = repo_root.as_ref() {
        trust_candidates.insert(repo_root.as_path().to_path_buf());
    }

    let runtime = crate::runtime_db::open_or_create_runtime_db(sqlite_home, "unknown")
        .await
        .map_err(|err| io::Error::other(format!("failed to open runtime storage: {err}")))?;
    let mut trust_levels_by_path = std::collections::HashMap::new();
    for candidate in trust_candidates {
        if let Some(trust_level) = runtime
            .get_project_trust(candidate.as_path())
            .await
            .map_err(|err| io::Error::other(format!("failed to read project trust: {err}")))?
        {
            trust_levels_by_path.insert(candidate.to_string_lossy().to_string(), trust_level);
        }
    }

    Ok(ProjectTrustContext {
        project_root,
        project_root_key,
        repo_root_key,
        trust_levels_by_path,
    })
}

fn normalize_absolute_path_for_trust(path: &Path) -> io::Result<AbsolutePathBuf> {
    AbsolutePathBuf::from_absolute_path(crate::runtime_db::normalize_cwd_for_runtime_db(path))
}

fn sqlite_home_from_merged_config(merged_config: &TomlValue, chaos_home: &Path) -> PathBuf {
    merged_config
        .clone()
        .try_into::<TrustStorageConfigToml>()
        .ok()
        .and_then(|config| config.sqlite_home.map(|path| path.to_path_buf()))
        .unwrap_or_else(|| chaos_home.to_path_buf())
}

/// Takes a `toml::Value` parsed from a config.toml file and walks through it,
/// resolving any `AbsolutePathBuf` fields against `base_dir`, returning a new
/// `toml::Value` with the same shape but with paths resolved.
///
/// This ensures that multiple config layers can be merged together correctly
/// even if they were loaded from different directories.
pub(crate) fn resolve_relative_paths_in_config_toml(
    value_from_config_toml: TomlValue,
    base_dir: &Path,
) -> io::Result<TomlValue> {
    // Use the serialize/deserialize round-trip to convert the
    // `toml::Value` into a `ConfigToml` with `AbsolutePath
    let _guard = AbsolutePathBufGuard::new(base_dir);
    let Ok(resolved) = value_from_config_toml.clone().try_into::<ConfigToml>() else {
        return Ok(value_from_config_toml);
    };
    drop(_guard);

    let resolved_value = TomlValue::try_from(resolved).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to serialize resolved config: {e}"),
        )
    })?;

    Ok(copy_shape_from_original(
        &value_from_config_toml,
        &resolved_value,
    ))
}

/// Ensure that every field in `original` is present in the returned
/// `toml::Value`, taking the value from `resolved` where possible. This ensures
/// the fields that we "removed" during the serialize/deserialize round-trip in
/// `resolve_config_paths` are preserved, out of an abundance of caution.
fn copy_shape_from_original(original: &TomlValue, resolved: &TomlValue) -> TomlValue {
    match (original, resolved) {
        (TomlValue::Table(original_table), TomlValue::Table(resolved_table)) => {
            let mut table = toml::map::Map::new();
            for (key, original_value) in original_table {
                let resolved_value = resolved_table.get(key).unwrap_or(original_value);
                table.insert(
                    key.clone(),
                    copy_shape_from_original(original_value, resolved_value),
                );
            }
            TomlValue::Table(table)
        }
        (TomlValue::Array(original_array), TomlValue::Array(resolved_array)) => {
            let mut items = Vec::new();
            for (index, original_value) in original_array.iter().enumerate() {
                let resolved_value = resolved_array.get(index).unwrap_or(original_value);
                items.push(copy_shape_from_original(original_value, resolved_value));
            }
            TomlValue::Array(items)
        }
        (_, resolved_value) => resolved_value.clone(),
    }
}

async fn find_project_root(
    cwd: &AbsolutePathBuf,
    project_root_markers: &[String],
) -> io::Result<AbsolutePathBuf> {
    if project_root_markers.is_empty() {
        return Ok(cwd.clone());
    }

    for ancestor in cwd.as_path().ancestors() {
        for marker in project_root_markers {
            let marker_path = ancestor.join(marker);
            if tokio::fs::metadata(&marker_path).await.is_ok() {
                return AbsolutePathBuf::from_absolute_path(ancestor);
            }
        }
    }
    Ok(cwd.clone())
}

/// Return the appropriate list of layers (each with
/// [ConfigLayerSource::Project] as the source) between `cwd` and
/// `project_root`, inclusive. The list is ordered in _increasing_ precdence,
/// starting from folders closest to `project_root` (which is the lowest
/// precedence) to those closest to `cwd` (which is the highest precedence).
async fn load_project_layers(
    cwd: &AbsolutePathBuf,
    project_root: &AbsolutePathBuf,
    trust_context: &ProjectTrustContext,
    chaos_home: &Path,
) -> io::Result<Vec<ConfigLayerEntry>> {
    let chaos_home_abs = AbsolutePathBuf::from_absolute_path(chaos_home)?;
    let chaos_home_normalized =
        normalize_path(chaos_home_abs.as_path()).unwrap_or_else(|_| chaos_home_abs.to_path_buf());
    let mut dirs = cwd
        .as_path()
        .ancestors()
        .scan(false, |done, a| {
            if *done {
                None
            } else {
                if a == project_root.as_path() {
                    *done = true;
                }
                Some(a)
            }
        })
        .collect::<Vec<_>>();
    dirs.reverse();

    let mut layers = Vec::new();
    let project_mcp_json = project_root.join(PROJECT_MCP_JSON_FILE)?;
    match tokio::fs::read_to_string(project_mcp_json.as_path()).await {
        Ok(contents) => match parse_project_mcp_json(&contents) {
            Ok(config) => layers.push(project_mcp_layer_entry(
                trust_context,
                &project_mcp_json,
                config,
            )),
            Err(err) => {
                if trust_context.decision_for_dir(project_root).is_trusted() {
                    return Err(err);
                }
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(io::Error::new(
                err.kind(),
                format!(
                    "Failed to read project MCP file {}: {err}",
                    project_mcp_json.as_path().display()
                ),
            ));
        }
    }

    for dir in dirs {
        let dot_chaos = dir.join(".chaos");
        if !tokio::fs::metadata(&dot_chaos)
            .await
            .map(|meta| meta.is_dir())
            .unwrap_or(false)
        {
            continue;
        }

        let layer_dir = AbsolutePathBuf::from_absolute_path(dir)?;
        let decision = trust_context.decision_for_dir(&layer_dir);
        let dot_chaos_abs = AbsolutePathBuf::from_absolute_path(&dot_chaos)?;
        let dot_chaos_normalized =
            normalize_path(dot_chaos_abs.as_path()).unwrap_or_else(|_| dot_chaos_abs.to_path_buf());
        if dot_chaos_abs == chaos_home_abs || dot_chaos_normalized == chaos_home_normalized {
            continue;
        }
        let config_file = dot_chaos_abs.join(CONFIG_TOML_FILE)?;
        match tokio::fs::read_to_string(&config_file).await {
            Ok(contents) => {
                let config: TomlValue = match toml::from_str(&contents) {
                    Ok(config) => config,
                    Err(e) => {
                        if decision.is_trusted() {
                            let config_file_display = config_file.as_path().display();
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "Error parsing project config file {config_file_display}: {e}"
                                ),
                            ));
                        }
                        layers.push(project_layer_entry(
                            trust_context,
                            &dot_chaos_abs,
                            &layer_dir,
                            TomlValue::Table(toml::map::Map::new()),
                            /*config_toml_exists*/ true,
                        ));
                        continue;
                    }
                };
                let config =
                    resolve_relative_paths_in_config_toml(config, dot_chaos_abs.as_path())?;
                let entry = project_layer_entry(
                    trust_context,
                    &dot_chaos_abs,
                    &layer_dir,
                    config,
                    /*config_toml_exists*/ true,
                );
                layers.push(entry);
            }
            Err(err) => {
                if err.kind() == io::ErrorKind::NotFound {
                    // If there is no config.toml file, record an empty entry
                    // for this project layer, as this may still have subfolders
                    // that are significant in the overall ConfigLayerStack.
                    layers.push(project_layer_entry(
                        trust_context,
                        &dot_chaos_abs,
                        &layer_dir,
                        TomlValue::Table(toml::map::Map::new()),
                        /*config_toml_exists*/ false,
                    ));
                } else {
                    let config_file_display = config_file.as_path().display();
                    return Err(io::Error::new(
                        err.kind(),
                        format!("Failed to read project config file {config_file_display}: {err}"),
                    ));
                }
            }
        }
    }

    Ok(layers)
}

// Cannot name this `mod tests` because of tests.rs in this folder.
#[cfg(test)]
mod unit_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn ensure_resolve_relative_paths_in_config_toml_preserves_all_fields() -> anyhow::Result<()> {
        let tmp = tempdir()?;
        let base_dir = tmp.path();
        let contents = r#"
# This is a field recognized by config.toml that is an AbsolutePathBuf in
# the ConfigToml struct.
model_instructions_file = "./some_file.md"

# This is a field recognized by config.toml.
model = "gpt-1000"

# This is a field not recognized by config.toml.
foo = "xyzzy"
"#;
        let user_config: TomlValue = toml::from_str(contents)?;

        let normalized_toml_value = resolve_relative_paths_in_config_toml(user_config, base_dir)?;
        let mut expected_toml_value = toml::map::Map::new();
        expected_toml_value.insert(
            "model_instructions_file".to_string(),
            TomlValue::String(
                AbsolutePathBuf::resolve_path_against_base("./some_file.md", base_dir)?
                    .as_path()
                    .to_string_lossy()
                    .to_string(),
            ),
        );
        expected_toml_value.insert(
            "model".to_string(),
            TomlValue::String("gpt-1000".to_string()),
        );
        expected_toml_value.insert("foo".to_string(), TomlValue::String("xyzzy".to_string()));
        assert_eq!(normalized_toml_value, TomlValue::Table(expected_toml_value));
        Ok(())
    }
}
