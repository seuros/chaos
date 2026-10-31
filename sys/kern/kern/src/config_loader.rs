#[cfg(test)]
mod tests;

use crate::async_breaker::BreakerError;
use crate::config::ConfigToml;
use crate::git_info::resolve_root_git_project_for_trust;
use crate::user_settings::ResolvedBootstrap;
use chaos_ipc::api::ConfigLayerSource;
use chaos_ipc::config_types::TrustLevel;
use chaos_ipc::product::OS_NAME;
use chaos_realpath::AbsolutePathBuf;
use chaos_realpath::AbsolutePathBufGuard;
use chaos_sysctl::ConfigRequirementsWithSources;
use chaos_sysctl::types::McpServerConfig;
use serde::Deserialize;
use serde::Serialize;
use std::fs::canonicalize as normalize_path;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use toml::Value as TomlValue;
use tracing::warn;

pub use chaos_sysctl::AppRequirementToml;
pub use chaos_sysctl::AppsRequirementsToml;
pub use chaos_sysctl::AssetFolder;
pub use chaos_sysctl::AssetScope;
pub use chaos_sysctl::ConfigError;
pub use chaos_sysctl::ConfigLayerEntry;
pub use chaos_sysctl::ConfigLayerStack;
pub use chaos_sysctl::ConfigLayerStackOrdering;
pub use chaos_sysctl::ConfigLoadError;
pub use chaos_sysctl::ConfigRequirements;
pub use chaos_sysctl::ConfigRequirementsToml;
pub use chaos_sysctl::ConstrainedWithSource;
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
pub use chaos_sysctl::format_config_error;
pub use chaos_sysctl::format_config_error_with_source;
pub use chaos_sysctl::format_error_chain;
pub use chaos_sysctl::merge_toml_values;
#[cfg(test)]
pub(crate) use chaos_sysctl::version_for_toml;

/// Administrator configuration folder. `requirements.toml` lives here, and
/// subfolders such as `rules/` and `agents/` are honored with admin authority.
pub const SYSTEM_CONFIG_DIR_UNIX: &str = "/etc/chaos";

const DEFAULT_PROJECT_ROOT_MARKERS: &[&str] = &[".git"];
const PROJECT_MCP_JSON_FILE: &str = ".mcp.json";

/// Configuration is built from these layers, lowest precedence first:
///
/// - database  owner settings in the runtime database
/// - project   `<project-root>/.mcp.json` (disabled when the project is untrusted)
/// - runtime   e.g., --config flags, model selector in UI
///
/// Admin constraints come from `/etc/chaos/requirements.toml`. File-based
/// assets (`rules/`, `agents/`) are anchored by the stack's asset folders:
/// `/etc/chaos`, `$CHAOS_HOME`, and trusted `.chaos` folders between the
/// project root and `cwd`.
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
    let bootstrap = crate::user_settings::resolve_bootstrap(chaos_home)
        .await
        .map_err(io::Error::other)?;
    load_config_layers_state_with_bootstrap(chaos_home, cwd, cli_overrides, overrides, &bootstrap)
        .await
}

pub(crate) async fn load_config_layers_state_with_bootstrap(
    chaos_home: &Path,
    cwd: Option<AbsolutePathBuf>,
    cli_overrides: &[(String, TomlValue)],
    overrides: LoaderOverrides,
    bootstrap: &ResolvedBootstrap,
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
        let mut cli_overrides_layer = build_cli_overrides_layer(cli_overrides);
        for key in ["storage_url", "sqlite_home", "egress_url"] {
            if cli_overrides_layer.get(key).is_some() {
                return Err(io::Error::other(format!(
                    "{key} must be configured through bootstrap or its environment variable, not session overrides"
                )));
            }
        }
        reject_legacy_global_mcp_servers_in_cli_overrides(&cli_overrides_layer)?;
        strip_global_mcp_servers(&mut cli_overrides_layer);
        let base_dir = cwd
            .as_ref()
            .map(AbsolutePathBuf::as_path)
            .unwrap_or(chaos_home);
        Some(resolve_relative_paths_in_config_toml(
            cli_overrides_layer,
            base_dir,
        )?)
    };

    let snapshot = crate::user_settings::snapshot(chaos_home)
        .await
        .map_err(io::Error::other)?;
    layers.push(ConfigLayerEntry::new(
        ConfigLayerSource::UserDatabase {
            revision: snapshot.revision,
        },
        snapshot.settings,
    ));
    let installation = crate::user_settings::installation_snapshot(chaos_home)
        .await
        .map_err(io::Error::other)?;
    layers.push(ConfigLayerEntry::new(
        ConfigLayerSource::InstallationDatabase {
            revision: installation.revision,
        },
        installation.settings,
    ));

    let mut asset_folders = vec![AssetFolder::new(
        AssetScope::Admin,
        AbsolutePathBuf::from_absolute_path(Path::new(SYSTEM_CONFIG_DIR_UNIX))?,
    )];

    if let Some(cwd) = cwd {
        let mut merged_so_far = TomlValue::Table(toml::map::Map::new());
        for layer in &layers {
            merge_toml_values(&mut merged_so_far, &layer.config);
        }
        if let Some(cli_overrides_layer) = cli_overrides_layer.as_ref() {
            merge_toml_values(&mut merged_so_far, cli_overrides_layer);
        }

        let project_root_markers = project_root_markers_from_config(&merged_so_far)?
            .unwrap_or_else(default_project_root_markers);
        let project_trust_context =
            project_trust_context(&cwd, &project_root_markers, bootstrap).await?;
        if let Some(layer) = load_project_mcp_layer(&project_trust_context).await? {
            layers.push(layer);
        }
        asset_folders
            .extend(project_asset_folders(&cwd, &project_trust_context, chaos_home).await?);
    }

    // Add a layer for runtime overrides from the CLI or UI, if any exist.
    if let Some(cli_overrides_layer) = cli_overrides_layer {
        layers.push(ConfigLayerEntry::new(
            ConfigLayerSource::SessionFlags,
            cli_overrides_layer,
        ));
    }

    Ok(ConfigLayerStack::new(
        layers,
        config_requirements_toml.clone().try_into()?,
        config_requirements_toml.into_toml(),
    )?
    .with_asset_folders(asset_folders))
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
    AbsolutePathBuf::from_absolute_path(Path::new(SYSTEM_CONFIG_DIR_UNIX).join("requirements.toml"))
}

/// Reads `project_root_markers` from the [toml::Value] produced by merging the
/// config layers in the stack preceding the project `.mcp.json` layer.
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
        if matches!(layer.name, ConfigLayerSource::ProjectMcp { .. }) {
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
    bootstrap: &ResolvedBootstrap,
    cwd: &Path,
    config_layer_stack: &ConfigLayerStack,
) -> io::Result<crate::config::ProjectTrust> {
    let cwd = normalize_absolute_path_for_trust(cwd)?;
    let project_root_markers = project_root_markers_from_stack(config_layer_stack);
    let trust_context = project_trust_context(&cwd, &project_root_markers, bootstrap).await?;
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
                "{trust_key} is marked as untrusted in {OS_NAME} project trust state. \
                 To load its .mcp.json, rules, and agents, mark it trusted."
            )),
            _ => Some(format!(
                "To load its .mcp.json, rules, and agents, trust {trust_key} in {OS_NAME}."
            )),
        }
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
    bootstrap: &ResolvedBootstrap,
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

    let mut trust_levels_by_path = std::collections::HashMap::new();
    let runtime = match crate::runtime_db::open_or_create_runtime_db(
        &bootstrap.storage_url,
        &bootstrap.storage_home,
        "unknown",
    )
    .await
    {
        Ok(runtime) => runtime,
        Err(err) => {
            warn!(
                error = %err,
                "runtime storage unavailable while loading project trust; treating the project as untrusted"
            );
            return Ok(ProjectTrustContext {
                project_root,
                project_root_key,
                repo_root_key,
                trust_levels_by_path,
            });
        }
    };
    for candidate in trust_candidates {
        match crate::runtime_db::with_runtime_storage_breaker(&runtime, || {
            runtime.get_project_trust(candidate.as_path())
        })
        .await
        {
            Ok(Some(trust_level)) => {
                trust_levels_by_path.insert(candidate.to_string_lossy().to_string(), trust_level);
            }
            Ok(None) => {}
            Err(BreakerError::Open) => break,
            Err(BreakerError::Operation(err)) => {
                warn!(
                    error = %err,
                    "runtime storage failed while loading project trust; treating unresolved paths as untrusted"
                );
                break;
            }
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

fn strip_global_mcp_servers(value: &mut TomlValue) {
    let Some(table) = value.as_table_mut() else {
        return;
    };
    table.remove("mcp_servers");
}

pub(crate) fn reject_legacy_global_mcp_servers_in_cli_overrides(
    value: &TomlValue,
) -> io::Result<()> {
    if value
        .as_table()
        .is_some_and(|table| table.contains_key("mcp_servers"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "top-level `mcp_servers` no longer belongs in config; use `chaos mcp add/remove/list/get` for global MCP servers or `.mcp.json` for project-local MCP servers",
        ));
    }
    Ok(())
}

/// Takes a `toml::Value` holding configuration and walks through it,
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

/// Load `<project-root>/.mcp.json` as the project MCP layer, if present.
async fn load_project_mcp_layer(
    trust_context: &ProjectTrustContext,
) -> io::Result<Option<ConfigLayerEntry>> {
    let project_root = &trust_context.project_root;
    let project_mcp_json = project_root.join(PROJECT_MCP_JSON_FILE);
    match tokio::fs::read_to_string(project_mcp_json.as_path()).await {
        Ok(contents) => match parse_project_mcp_json(&contents) {
            Ok(config) => Ok(Some(project_mcp_layer_entry(
                trust_context,
                &project_mcp_json,
                config,
            ))),
            Err(err) => {
                if trust_context.decision_for_dir(project_root).is_trusted() {
                    Err(err)
                } else {
                    Ok(None)
                }
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io::Error::new(
            err.kind(),
            format!(
                "Failed to read project MCP file {}: {err}",
                project_mcp_json.as_path().display()
            ),
        )),
    }
}

/// `.chaos` folders between the project root and `cwd`, inclusive, ordered from
/// the project root (lowest precedence) to `cwd` (highest). Untrusted folders
/// are listed as disabled so the UI can explain why their assets are ignored.
async fn project_asset_folders(
    cwd: &AbsolutePathBuf,
    trust_context: &ProjectTrustContext,
    chaos_home: &Path,
) -> io::Result<Vec<AssetFolder>> {
    let project_root = &trust_context.project_root;
    let chaos_home_abs = AbsolutePathBuf::try_from(chaos_home)?;
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

    let mut folders = Vec::new();
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
        let dot_chaos_abs = AbsolutePathBuf::from_absolute_path(&dot_chaos)?;
        let dot_chaos_normalized =
            normalize_path(dot_chaos_abs.as_path()).unwrap_or_else(|_| dot_chaos_abs.to_path_buf());
        if dot_chaos_abs == chaos_home_abs || dot_chaos_normalized == chaos_home_normalized {
            continue;
        }
        folders.push(match trust_context.disabled_reason_for_dir(&layer_dir) {
            Some(reason) => AssetFolder::disabled(AssetScope::Project, dot_chaos_abs, reason),
            None => AssetFolder::new(AssetScope::Project, dot_chaos_abs),
        });
    }

    Ok(folders)
}
