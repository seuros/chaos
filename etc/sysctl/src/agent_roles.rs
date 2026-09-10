use crate::ConfigLayerStack;
use crate::ConfigLayerStackOrdering;
use crate::types::AgentRoleConfig;
use chaos_realpath::AbsolutePathBufGuard;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use toml::Value as TomlValue;

pub fn load_agent_roles(
    config_layer_stack: &ConfigLayerStack,
    startup_warnings: &mut Vec<String>,
) -> std::io::Result<BTreeMap<String, AgentRoleConfig>> {
    let layers = config_layer_stack.get_layers(
        ConfigLayerStackOrdering::LowestPrecedenceFirst,
        /*include_disabled*/ false,
    );
    let mut roles: BTreeMap<String, AgentRoleConfig> = BTreeMap::new();
    for layer in layers {
        // Resolve role assets relative to bootstrap.
        let folder = match &layer.name {
            chaos_ipc::api::ConfigLayerSource::Bootstrap { file } => file.parent(),
            _ => layer.config_folder(),
        };
        let Some(config_folder) = folder else {
            continue;
        };
        let layer_roles = discover_agent_roles_in_dir(
            config_folder.as_path().join("agents").as_path(),
            startup_warnings,
        )?;

        for (role_name, role) in layer_roles {
            let mut merged_role = role;
            if let Some(existing_role) = roles.get(&role_name) {
                merge_missing_role_fields(&mut merged_role, existing_role);
            }
            if let Err(err) = validate_required_agent_role_description(
                &role_name,
                merged_role.description.as_deref(),
            ) {
                push_agent_role_warning(startup_warnings, err);
                continue;
            }
            roles.insert(role_name, merged_role);
        }
    }

    Ok(roles)
}

fn push_agent_role_warning(startup_warnings: &mut Vec<String>, err: std::io::Error) {
    let message = format!("Ignoring malformed agent role definition: {err}");
    tracing::warn!("{message}");
    startup_warnings.push(message);
}

fn merge_missing_role_fields(role: &mut AgentRoleConfig, fallback: &AgentRoleConfig) {
    role.description = role.description.clone().or(fallback.description.clone());
    role.nickname_candidates = role
        .nickname_candidates
        .clone()
        .or(fallback.nickname_candidates.clone());
    role.topics = role.topics.clone().or(fallback.topics.clone());
    role.catchphrases = role.catchphrases.clone().or(fallback.catchphrases.clone());
}

/// Lightweight stand-in for ConfigToml used only to validate agent role files.
/// Accepts `developer_instructions` and ignores other config fields via flatten.
#[derive(Deserialize, Debug, Clone, PartialEq)]
struct RawAgentRoleFileToml {
    name: Option<String>,
    description: Option<String>,
    nickname_candidates: Option<Vec<String>>,
    developer_instructions: Option<String>,
    topics: Option<Vec<String>>,
    catchphrases: Option<Vec<String>>,
    #[serde(flatten)]
    _rest: TomlValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAgentRoleFile {
    pub role_name: String,
    pub description: Option<String>,
    pub nickname_candidates: Option<Vec<String>>,
    pub topics: Option<Vec<String>>,
    pub catchphrases: Option<Vec<String>>,
    pub config: TomlValue,
}

/// Splits a Markdown file with TOML frontmatter (`---` delimiters) into
/// `(frontmatter, body)`. Returns `None` if the content does not begin with `---`.
fn split_md_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;
    if let Some(pos) = rest.find("\n---\n") {
        Some((&rest[..pos], &rest[pos + 5..]))
    } else if let Some(pos) = rest.find("\n---\r\n") {
        Some((&rest[..pos], &rest[pos + 6..]))
    } else if let Some(stripped) = rest.strip_suffix("\n---") {
        Some((stripped, ""))
    } else {
        None
    }
}

/// Parse a self-contained role file. Only embedded builtins may omit
/// instructions (the default role intentionally imposes none).
pub fn parse_agent_role_file_contents(
    contents: &str,
    role_file_label: &Path,
    config_base_dir: &Path,
    require_developer_instructions: bool,
) -> std::io::Result<ResolvedAgentRoleFile> {
    let is_md = role_file_label.extension().is_some_and(|ext| ext == "md");

    let (toml_str, md_body) = if is_md {
        match split_md_frontmatter(contents) {
            Some((fm, body)) => (fm, body.trim()),
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "agent role file at {} must begin with a TOML frontmatter block (--- ... ---)",
                        role_file_label.display()
                    ),
                ));
            }
        }
    } else {
        (contents, "")
    };

    let role_file_toml: TomlValue = toml::from_str(toml_str).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "failed to parse agent role file at {}: {err}",
                role_file_label.display()
            ),
        )
    })?;
    let _guard = AbsolutePathBufGuard::new(config_base_dir);
    let parsed: RawAgentRoleFileToml = role_file_toml.clone().try_into().map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "failed to deserialize agent role file at {}: {err}",
                role_file_label.display()
            ),
        )
    })?;

    if is_md && !md_body.is_empty() && parsed.developer_instructions.is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "agent role file at {} defines developer_instructions in both frontmatter and body; use one or the other",
                role_file_label.display()
            ),
        ));
    }

    let description = normalize_agent_role_description(
        &format!("agent role file {}.description", role_file_label.display()),
        parsed.description.as_deref(),
    )?;

    let effective_developer_instructions = if is_md && !md_body.is_empty() {
        Some(md_body)
    } else {
        parsed.developer_instructions.as_deref()
    };

    validate_agent_role_file_developer_instructions(
        role_file_label,
        effective_developer_instructions,
        require_developer_instructions,
    )?;

    let role_name = parsed
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "agent role file at {} must define a non-empty `name`",
                    role_file_label.display()
                ),
            )
        })?;

    let nickname_candidates = normalize_agent_role_nickname_candidates(
        &format!(
            "agent role file {}.nickname_candidates",
            role_file_label.display()
        ),
        parsed.nickname_candidates.as_deref(),
    )?;

    let mut config = role_file_toml;
    let Some(config_table) = config.as_table_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "agent role file at {} must contain a TOML table",
                role_file_label.display()
            ),
        ));
    };
    config_table.remove("name");
    config_table.remove("description");
    config_table.remove("nickname_candidates");
    config_table.remove("topics");
    config_table.remove("catchphrases");

    if is_md && !md_body.is_empty() {
        config_table.insert(
            "developer_instructions".to_string(),
            TomlValue::String(md_body.to_owned()),
        );
    }

    Ok(ResolvedAgentRoleFile {
        role_name,
        description,
        nickname_candidates,
        topics: parsed.topics,
        catchphrases: parsed.catchphrases,
        config,
    })
}

fn read_resolved_agent_role_file(path: &Path) -> std::io::Result<ResolvedAgentRoleFile> {
    let contents = std::fs::read_to_string(path)?;
    parse_agent_role_file_contents(
        &contents,
        path,
        path.parent().unwrap_or(path),
        /*require_developer_instructions*/ true,
    )
}

fn normalize_agent_role_description(
    field_label: &str,
    description: Option<&str>,
) -> std::io::Result<Option<String>> {
    match description.map(str::trim) {
        Some("") => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{field_label} cannot be blank"),
        )),
        Some(description) => Ok(Some(description.to_string())),
        None => Ok(None),
    }
}

fn validate_required_agent_role_description(
    role_name: &str,
    description: Option<&str>,
) -> std::io::Result<()> {
    if description.is_some() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("agent role `{role_name}` must define a description"),
        ))
    }
}

fn validate_agent_role_file_developer_instructions(
    role_file_label: &Path,
    developer_instructions: Option<&str>,
    require_present: bool,
) -> std::io::Result<()> {
    match developer_instructions.map(str::trim) {
        Some("") => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "agent role file at {}.developer_instructions cannot be blank",
                role_file_label.display()
            ),
        )),
        Some(_) => Ok(()),
        None if require_present => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "agent role file at {} must define `developer_instructions`",
                role_file_label.display()
            ),
        )),
        None => Ok(()),
    }
}

fn normalize_agent_role_nickname_candidates(
    field_label: &str,
    nickname_candidates: Option<&[String]>,
) -> std::io::Result<Option<Vec<String>>> {
    let Some(nickname_candidates) = nickname_candidates else {
        return Ok(None);
    };

    if nickname_candidates.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{field_label} must contain at least one name"),
        ));
    }

    let mut normalized_candidates = Vec::with_capacity(nickname_candidates.len());
    let mut seen_candidates = BTreeSet::new();

    for nickname in nickname_candidates {
        let normalized_nickname = nickname.trim();
        if normalized_nickname.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{field_label} cannot contain blank names"),
            ));
        }

        if !seen_candidates.insert(normalized_nickname.to_owned()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{field_label} cannot contain duplicates"),
            ));
        }

        if !normalized_nickname
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "{field_label} may only contain ASCII letters, digits, spaces, hyphens, and underscores"
                ),
            ));
        }

        normalized_candidates.push(normalized_nickname.to_owned());
    }

    Ok(Some(normalized_candidates))
}

fn discover_agent_roles_in_dir(
    agents_dir: &Path,
    startup_warnings: &mut Vec<String>,
) -> std::io::Result<BTreeMap<String, AgentRoleConfig>> {
    let mut roles = BTreeMap::new();

    for agent_file in collect_agent_role_files(agents_dir)? {
        let parsed_file = match read_resolved_agent_role_file(&agent_file) {
            Ok(parsed_file) => parsed_file,
            Err(err) => {
                push_agent_role_warning(startup_warnings, err);
                continue;
            }
        };
        let role_name = parsed_file.role_name;
        if roles.contains_key(&role_name) {
            push_agent_role_warning(
                startup_warnings,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "duplicate agent role name `{role_name}` discovered in {}",
                        agents_dir.display()
                    ),
                ),
            );
            continue;
        }
        roles.insert(
            role_name,
            AgentRoleConfig {
                description: parsed_file.description,
                config_file: Some(agent_file),
                nickname_candidates: parsed_file.nickname_candidates,
                topics: parsed_file.topics,
                catchphrases: parsed_file.catchphrases,
            },
        );
    }

    Ok(roles)
}

/// Returns references to all roles whose declared topics overlap with `requested_topics`.
///
/// The caller is responsible for sampling one from the returned list. Returns an empty
/// vec when no role matches, which the caller should treat as a cache miss and fall back
/// to the default role while surfacing the missing topic to the user.
pub fn resolve_roles_by_topics<'a>(
    roles: &'a BTreeMap<String, AgentRoleConfig>,
    requested_topics: &[String],
) -> Vec<(&'a str, &'a AgentRoleConfig)> {
    if requested_topics.is_empty() {
        return Vec::new();
    }
    roles
        .iter()
        .filter(|(_, role)| {
            role.topics.as_deref().is_some_and(|role_topics| {
                role_topics
                    .iter()
                    .any(|t| requested_topics.iter().any(|r| r.eq_ignore_ascii_case(t)))
            })
        })
        .map(|(name, role)| (name.as_str(), role))
        .collect()
}

fn collect_agent_role_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_agent_role_files_recursive(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_agent_role_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    for entry in read_dir {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_agent_role_files_recursive(&path, files)?;
            continue;
        }
        if file_type.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext == "toml" || ext == "md")
        {
            files.push(path);
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "agent_roles_tests.rs"]
mod tests;
