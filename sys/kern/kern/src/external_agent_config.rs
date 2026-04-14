use chaos_ipc::config_types::SANDBOX_MODE_WORKSPACE_WRITE;
use serde_json::Value as JsonValue;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use toml::Value as TomlValue;

const EXTERNAL_AGENT_CONFIG_DETECT_METRIC: &str = "chaos.external_agent_config.detect";
const EXTERNAL_AGENT_CONFIG_IMPORT_METRIC: &str = "chaos.external_agent_config.import";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalAgentConfigDetectOptions {
    pub include_home: bool,
    pub cwds: Option<Vec<PathBuf>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalAgentConfigMigrationItemType {
    Config,
    AgentsMd,
    McpServerConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalAgentConfigMigrationItem {
    pub item_type: ExternalAgentConfigMigrationItemType,
    pub description: String,
    pub cwd: Option<PathBuf>,
}

#[derive(Clone)]
pub struct ExternalAgentConfigService {
    chaos_home: PathBuf,
    claude_home: PathBuf,
}

impl ExternalAgentConfigService {
    pub fn new(chaos_home: PathBuf) -> Self {
        let claude_home = default_claude_home();
        Self {
            chaos_home,
            claude_home,
        }
    }

    #[cfg(test)]
    fn new_for_test(chaos_home: PathBuf, claude_home: PathBuf) -> Self {
        Self {
            chaos_home,
            claude_home,
        }
    }

    pub fn detect(
        &self,
        params: ExternalAgentConfigDetectOptions,
    ) -> io::Result<Vec<ExternalAgentConfigMigrationItem>> {
        let mut items = Vec::new();
        if params.include_home {
            self.detect_migrations(/*repo_root*/ None, &mut items)?;
        }

        for cwd in params.cwds.as_deref().unwrap_or(&[]) {
            let Some(repo_root) = find_repo_root(Some(cwd))? else {
                continue;
            };
            self.detect_migrations(Some(&repo_root), &mut items)?;
        }

        Ok(items)
    }

    pub fn import(&self, migration_items: Vec<ExternalAgentConfigMigrationItem>) -> io::Result<()> {
        for migration_item in migration_items {
            match migration_item.item_type {
                ExternalAgentConfigMigrationItemType::Config => {
                    self.import_config(migration_item.cwd.as_deref())?;
                    emit_migration_metric(
                        EXTERNAL_AGENT_CONFIG_IMPORT_METRIC,
                        ExternalAgentConfigMigrationItemType::Config,
                    );
                }
                ExternalAgentConfigMigrationItemType::AgentsMd => {
                    self.import_agents_md(migration_item.cwd.as_deref())?;
                    emit_migration_metric(
                        EXTERNAL_AGENT_CONFIG_IMPORT_METRIC,
                        ExternalAgentConfigMigrationItemType::AgentsMd,
                    );
                }
                ExternalAgentConfigMigrationItemType::McpServerConfig => {}
            }
        }

        Ok(())
    }

    fn detect_migrations(
        &self,
        repo_root: Option<&Path>,
        items: &mut Vec<ExternalAgentConfigMigrationItem>,
    ) -> io::Result<()> {
        let cwd = repo_root.map(Path::to_path_buf);
        let source_settings = repo_root.map_or_else(
            || self.claude_home.join("settings.json"),
            |repo_root| repo_root.join(".claude").join("settings.json"),
        );
        let target_config = repo_root.map_or_else(
            || self.chaos_home.join("config.toml"),
            |repo_root| repo_root.join(".chaos").join("config.toml"),
        );
        if source_settings.is_file() {
            let raw_settings = fs::read_to_string(&source_settings)?;
            let settings: JsonValue = serde_json::from_str(&raw_settings)
                .map_err(|err| invalid_data_error(err.to_string()))?;
            let migrated = build_config_from_external(&settings)?;
            if !is_empty_toml_table(&migrated) {
                let mut should_include = true;
                if target_config.exists() {
                    let existing_raw = fs::read_to_string(&target_config)?;
                    let mut existing = if existing_raw.trim().is_empty() {
                        TomlValue::Table(Default::default())
                    } else {
                        toml::from_str::<TomlValue>(&existing_raw).map_err(|err| {
                            invalid_data_error(format!("invalid existing config.toml: {err}"))
                        })?
                    };
                    should_include = merge_missing_toml_values(&mut existing, &migrated)?;
                }

                if should_include {
                    items.push(ExternalAgentConfigMigrationItem {
                        item_type: ExternalAgentConfigMigrationItemType::Config,
                        description: format!(
                            "Migrate {} into {}",
                            source_settings.display(),
                            target_config.display()
                        ),
                        cwd: cwd.clone(),
                    });
                    emit_migration_metric(
                        EXTERNAL_AGENT_CONFIG_DETECT_METRIC,
                        ExternalAgentConfigMigrationItemType::Config,
                    );
                }
            }
        }

        let source_agents_md = if let Some(repo_root) = repo_root {
            find_repo_agents_md_source(repo_root)?
        } else {
            let path = self.claude_home.join("CLAUDE.md");
            is_non_empty_text_file(&path)?.then_some(path)
        };
        let target_agents_md = repo_root.map_or_else(
            || self.chaos_home.join("AGENTS.md"),
            |repo_root| repo_root.join("AGENTS.md"),
        );
        if let Some(source_agents_md) = source_agents_md
            && is_missing_or_empty_text_file(&target_agents_md)?
        {
            items.push(ExternalAgentConfigMigrationItem {
                item_type: ExternalAgentConfigMigrationItemType::AgentsMd,
                description: format!(
                    "Import {} to {}",
                    source_agents_md.display(),
                    target_agents_md.display()
                ),
                cwd,
            });
            emit_migration_metric(
                EXTERNAL_AGENT_CONFIG_DETECT_METRIC,
                ExternalAgentConfigMigrationItemType::AgentsMd,
            );
        }

        Ok(())
    }

    fn import_config(&self, cwd: Option<&Path>) -> io::Result<()> {
        let (source_settings, target_config) = if let Some(repo_root) = find_repo_root(cwd)? {
            (
                repo_root.join(".claude").join("settings.json"),
                repo_root.join(".chaos").join("config.toml"),
            )
        } else if cwd.is_some_and(|cwd| !cwd.as_os_str().is_empty()) {
            return Ok(());
        } else {
            (
                self.claude_home.join("settings.json"),
                self.chaos_home.join("config.toml"),
            )
        };
        if !source_settings.is_file() {
            return Ok(());
        }

        let raw_settings = fs::read_to_string(&source_settings)?;
        let settings: JsonValue = serde_json::from_str(&raw_settings)
            .map_err(|err| invalid_data_error(err.to_string()))?;
        let migrated = build_config_from_external(&settings)?;
        if is_empty_toml_table(&migrated) {
            return Ok(());
        }

        let Some(target_parent) = target_config.parent() else {
            return Err(invalid_data_error("config target path has no parent"));
        };
        fs::create_dir_all(target_parent)?;
        if !target_config.exists() {
            write_toml_file(&target_config, &migrated)?;
            return Ok(());
        }

        let existing_raw = fs::read_to_string(&target_config)?;
        let mut existing = if existing_raw.trim().is_empty() {
            TomlValue::Table(Default::default())
        } else {
            toml::from_str::<TomlValue>(&existing_raw)
                .map_err(|err| invalid_data_error(format!("invalid existing config.toml: {err}")))?
        };

        let changed = merge_missing_toml_values(&mut existing, &migrated)?;
        if !changed {
            return Ok(());
        }

        write_toml_file(&target_config, &existing)?;
        Ok(())
    }

    fn import_agents_md(&self, cwd: Option<&Path>) -> io::Result<()> {
        let (source_agents_md, target_agents_md) = if let Some(repo_root) = find_repo_root(cwd)? {
            let Some(source_agents_md) = find_repo_agents_md_source(&repo_root)? else {
                return Ok(());
            };
            (source_agents_md, repo_root.join("AGENTS.md"))
        } else if cwd.is_some_and(|cwd| !cwd.as_os_str().is_empty()) {
            return Ok(());
        } else {
            (
                self.claude_home.join("CLAUDE.md"),
                self.chaos_home.join("AGENTS.md"),
            )
        };
        if !is_non_empty_text_file(&source_agents_md)?
            || !is_missing_or_empty_text_file(&target_agents_md)?
        {
            return Ok(());
        }

        let Some(target_parent) = target_agents_md.parent() else {
            return Err(invalid_data_error("AGENTS.md target path has no parent"));
        };
        fs::create_dir_all(target_parent)?;

        rewrite_and_copy_text_file(&source_agents_md, &target_agents_md)
    }
}

fn default_claude_home() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        return PathBuf::from(home).join(".claude");
    }

    PathBuf::from(".claude")
}

fn find_repo_root(cwd: Option<&Path>) -> io::Result<Option<PathBuf>> {
    let Some(cwd) = cwd.filter(|cwd| !cwd.as_os_str().is_empty()) else {
        return Ok(None);
    };

    let mut current = if cwd.is_absolute() {
        cwd.to_path_buf()
    } else {
        std::env::current_dir()?.join(cwd)
    };

    if !current.exists() {
        return Ok(None);
    }

    if current.is_file() {
        let Some(parent) = current.parent() else {
            return Ok(None);
        };
        current = parent.to_path_buf();
    }

    let fallback = current.clone();
    loop {
        let git_path = current.join(".git");
        if git_path.is_dir() || git_path.is_file() {
            return Ok(Some(current));
        }
        if !current.pop() {
            break;
        }
    }

    Ok(Some(fallback))
}

fn is_missing_or_empty_text_file(path: &Path) -> io::Result<bool> {
    if !path.exists() {
        return Ok(true);
    }
    if !path.is_file() {
        return Ok(false);
    }

    Ok(fs::read_to_string(path)?.trim().is_empty())
}

fn is_non_empty_text_file(path: &Path) -> io::Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }

    Ok(!fs::read_to_string(path)?.trim().is_empty())
}

fn find_repo_agents_md_source(repo_root: &Path) -> io::Result<Option<PathBuf>> {
    for candidate in [
        repo_root.join("CLAUDE.md"),
        repo_root.join(".claude").join("CLAUDE.md"),
    ] {
        if is_non_empty_text_file(&candidate)? {
            return Ok(Some(candidate));
        }
    }

    Ok(None)
}

fn rewrite_and_copy_text_file(source: &Path, target: &Path) -> io::Result<()> {
    let source_contents = fs::read_to_string(source)?;
    let rewritten = rewrite_claude_terms(&source_contents);
    fs::write(target, rewritten)
}

fn rewrite_claude_terms(content: &str) -> String {
    let mut rewritten = replace_case_insensitive_with_boundaries(content, "claude.md", "AGENTS.md");
    for from in [
        "claude code",
        "claude-code",
        "claude_code",
        "claudecode",
        "claude",
    ] {
        rewritten = replace_case_insensitive_with_boundaries(&rewritten, from, "Chaos");
    }
    rewritten
}

fn replace_case_insensitive_with_boundaries(
    input: &str,
    needle: &str,
    replacement: &str,
) -> String {
    let needle_lower = needle.to_ascii_lowercase();
    if needle_lower.is_empty() {
        return input.to_string();
    }

    let haystack_lower = input.to_ascii_lowercase();
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut last_emitted = 0usize;
    let mut search_start = 0usize;

    while let Some(relative_pos) = haystack_lower[search_start..].find(&needle_lower) {
        let start = search_start + relative_pos;
        let end = start + needle_lower.len();
        let boundary_before = start == 0 || !is_word_byte(bytes[start - 1]);
        let boundary_after = end == bytes.len() || !is_word_byte(bytes[end]);

        if boundary_before && boundary_after {
            output.push_str(&input[last_emitted..start]);
            output.push_str(replacement);
            last_emitted = end;
        }

        search_start = start + 1;
    }

    if last_emitted == 0 {
        return input.to_string();
    }

    output.push_str(&input[last_emitted..]);
    output
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn build_config_from_external(settings: &JsonValue) -> io::Result<TomlValue> {
    let Some(settings_obj) = settings.as_object() else {
        return Err(invalid_data_error(
            "external agent settings root must be an object",
        ));
    };

    let mut root = toml::map::Map::new();

    if let Some(env) = settings_obj.get("env").and_then(JsonValue::as_object)
        && !env.is_empty()
    {
        let mut shell_policy = toml::map::Map::new();
        shell_policy.insert("inherit".to_string(), TomlValue::String("core".to_string()));
        shell_policy.insert(
            "set".to_string(),
            TomlValue::Table(json_object_to_env_toml_table(env)),
        );
        root.insert(
            "shell_environment_policy".to_string(),
            TomlValue::Table(shell_policy),
        );
    }

    if let Some(sandbox_enabled) = settings_obj
        .get("sandbox")
        .and_then(JsonValue::as_object)
        .and_then(|sandbox| sandbox.get("enabled"))
        .and_then(JsonValue::as_bool)
        && sandbox_enabled
    {
        root.insert(
            "sandbox_mode".to_string(),
            TomlValue::String(SANDBOX_MODE_WORKSPACE_WRITE.to_string()),
        );
    }

    Ok(TomlValue::Table(root))
}

fn json_object_to_env_toml_table(
    object: &serde_json::Map<String, JsonValue>,
) -> toml::map::Map<String, TomlValue> {
    let mut table = toml::map::Map::new();
    for (key, value) in object {
        if let Some(value) = json_env_value_to_string(value) {
            table.insert(key.clone(), TomlValue::String(value));
        }
    }
    table
}

fn json_env_value_to_string(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(value) => Some(value.clone()),
        JsonValue::Null => None,
        JsonValue::Bool(value) => Some(value.to_string()),
        JsonValue::Number(value) => Some(value.to_string()),
        JsonValue::Array(_) | JsonValue::Object(_) => None,
    }
}

fn merge_missing_toml_values(existing: &mut TomlValue, incoming: &TomlValue) -> io::Result<bool> {
    match (existing, incoming) {
        (TomlValue::Table(existing_table), TomlValue::Table(incoming_table)) => {
            let mut changed = false;
            for (key, incoming_value) in incoming_table {
                match existing_table.get_mut(key) {
                    Some(existing_value) => {
                        if matches!(
                            (&*existing_value, incoming_value),
                            (TomlValue::Table(_), TomlValue::Table(_))
                        ) && merge_missing_toml_values(existing_value, incoming_value)?
                        {
                            changed = true;
                        }
                    }
                    None => {
                        existing_table.insert(key.clone(), incoming_value.clone());
                        changed = true;
                    }
                }
            }
            Ok(changed)
        }
        _ => Err(invalid_data_error(
            "expected TOML table while merging migrated config values",
        )),
    }
}

fn write_toml_file(path: &Path, value: &TomlValue) -> io::Result<()> {
    let serialized = toml::to_string_pretty(value)
        .map_err(|err| invalid_data_error(format!("failed to serialize config.toml: {err}")))?;
    fs::write(path, format!("{}\n", serialized.trim_end()))
}

fn is_empty_toml_table(value: &TomlValue) -> bool {
    match value {
        TomlValue::Table(table) => table.is_empty(),
        TomlValue::String(_)
        | TomlValue::Integer(_)
        | TomlValue::Float(_)
        | TomlValue::Boolean(_)
        | TomlValue::Datetime(_)
        | TomlValue::Array(_) => false,
    }
}

fn invalid_data_error(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn migration_metric_tags(
    item_type: ExternalAgentConfigMigrationItemType,
) -> Vec<(&'static str, String)> {
    let migration_type = match item_type {
        ExternalAgentConfigMigrationItemType::Config => "config",
        ExternalAgentConfigMigrationItemType::AgentsMd => "agents_md",
        ExternalAgentConfigMigrationItemType::McpServerConfig => "mcp_server_config",
    };
    vec![("migration_type", migration_type.to_string())]
}

fn emit_migration_metric(metric_name: &str, item_type: ExternalAgentConfigMigrationItemType) {
    let Some(metrics) = chaos_syslog::metrics::global() else {
        return;
    };
    let tags = migration_metric_tags(item_type);
    let tag_refs = tags
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect::<Vec<_>>();
    let _ = metrics.counter(metric_name, /*inc*/ 1, &tag_refs);
}

#[cfg(test)]
#[path = "external_agent_config_tests.rs"]
mod tests;
