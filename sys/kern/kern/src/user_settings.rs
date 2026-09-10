//! Settings storage without runtime-service initialization.
use anyhow::{Context, bail, ensure};
use chaos_proc::{ApprovalState, RememberedApproval, RuntimeDbHandle};
use chaos_sysctl::persistence::{PersistenceFuture, SettingsPersistence, SettingsSnapshot};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use toml::Value;
use uuid::Uuid;

const BOOTSTRAP_KEYS: &[&str] = &["storage_url", "egress_url", "sqlite_home"];

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BootstrapConfig {
    pub storage_url: Option<String>,
    pub egress_url: Option<String>,
    /// Compatibility input, normalized to a SQLite URL by migration.
    pub sqlite_home: Option<PathBuf>,
}

fn read_toml(home: &Path) -> anyhow::Result<Value> {
    match std::fs::read_to_string(home.join(chaos_sysctl::CONFIG_TOML_FILE)) {
        Ok(text) => Ok(toml::from_str(&text).context("invalid bootstrap TOML")?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(Value::Table(Default::default()))
        }
        Err(err) => Err(err.into()),
    }
}

impl BootstrapConfig {
    pub fn read(home: &Path) -> anyhow::Result<Self> {
        Ok(read_toml(home)?.try_into()?)
    }

    pub fn redacted(mut self) -> Self {
        for value in [&mut self.storage_url, &mut self.egress_url]
            .into_iter()
            .flatten()
        {
            if value.starts_with("env:") || chaos_sysctl::secrets::is_reference(value) {
                continue;
            }
            if let Ok(mut url) = url::Url::parse(value) {
                if !url.username().is_empty() || url.password().is_some() {
                    let _ = url.set_username("REDACTED");
                    let _ = url.set_password(None);
                }
                if url.query().is_some() {
                    url.set_query(Some("REDACTED"));
                }
                *value = url.into();
            } else {
                *value = "<invalid URL; value redacted>".into();
            }
        }
        self
    }

    pub fn sqlite_home(&self, home: &Path) -> PathBuf {
        let path = self
            .sqlite_home
            .clone()
            .or_else(|| chaos_proc::sqlite_home_env_value().map(PathBuf::from))
            .unwrap_or_else(|| home.to_path_buf());
        if path.is_absolute() {
            path
        } else {
            home.join(path)
        }
    }

    pub fn resolved_storage_url(&self) -> anyhow::Result<Option<String>> {
        self.storage_url
            .clone()
            .or_else(|| std::env::var("CHAOS_STORAGE_URL").ok())
            .as_deref()
            .map(resolve_reference)
            .transpose()
    }

    pub fn effective_values(&self, home: &Path) -> anyhow::Result<Value> {
        let mut values = toml::map::Map::new();
        let url = self.resolved_storage_url()?.unwrap_or_else(|| {
            format!(
                "sqlite://{}",
                self.sqlite_home(home).join("chaos.sqlite").display()
            )
        });
        values.insert("storage_url".into(), Value::String(url));
        if let Some(url) = &self.egress_url {
            values.insert("egress_url".into(), Value::String(resolve_reference(url)?));
        }
        values.insert(
            "sqlite_home".into(),
            Value::String(self.sqlite_home(home).to_string_lossy().into()),
        );
        Ok(Value::Table(values))
    }
}

fn lock_bootstrap(home: &Path) -> anyhow::Result<std::fs::File> {
    std::fs::create_dir_all(home)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(home.join("settings-migration.lock"))?;
    lock.try_lock()
        .context("another bootstrap edit or settings migration is running")?;
    Ok(lock)
}

pub fn set_bootstrap(home: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    ensure!(
        matches!(key, "storage_url" | "egress_url"),
        "only storage_url and egress_url are bootstrap settings"
    );
    if let Some(name) = value.strip_prefix("env:") {
        ensure!(!name.is_empty(), "empty environment reference");
    } else if chaos_sysctl::secrets::is_reference(value) {
        Uuid::parse_str(value.rsplit('/').next().unwrap_or_default())
            .context("invalid credential reference")?;
    } else {
        let url = url::Url::parse(value)
            .context("bootstrap value must be a URL or credential/environment reference")?;
        ensure!(
            url.password().is_none(),
            "use env:VARIABLE for credential-bearing URLs"
        );
        if key == "storage_url" {
            crate::config::normalize_storage_url(Some(value))?;
        } else {
            chaos_client::Egress::parse(value).map_err(anyhow::Error::msg)?;
        }
    }
    // Serialize bootstrap edits with migration; keep recovery offline.
    let _lock = lock_bootstrap(home)?;
    // Bootstrap edits are top-level, never profile-scoped.
    chaos_sysctl::edit::apply_file_edits_blocking(
        home,
        None,
        &[chaos_sysctl::edit::ConfigEdit::SetPath {
            segments: vec![key.to_owned()],
            value: toml_edit::value(value),
        }],
    )
}

fn resolve_reference(value: &str) -> anyhow::Result<String> {
    if let Some(name) = value.strip_prefix("env:") {
        ensure!(!name.is_empty(), "empty environment reference");
        return std::env::var(name)
            .with_context(|| format!("missing bootstrap environment variable {name}"));
    }
    chaos_sysctl::secrets::resolve(value)
}

pub async fn open(home: &Path) -> anyhow::Result<RuntimeDbHandle> {
    let bootstrap = BootstrapConfig::read(home)?;
    let url = bootstrap.resolved_storage_url()?;
    let sqlite_home = bootstrap.sqlite_home(home);
    let effective_url = url
        .clone()
        .unwrap_or_else(|| format!("sqlite://{}", sqlite_home.join("chaos.sqlite").display()));
    let identity = fingerprint(&serde_json::json!({"url": effective_url}))?;
    static MOUNTS: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, String>>,
    > = std::sync::LazyLock::new(Default::default);
    {
        let mounts = MOUNTS
            .lock()
            .map_err(|_| anyhow::anyhow!("settings mount lock poisoned"))?;
        ensure!(
            mounts
                .get(home)
                .is_none_or(|previous| previous == &identity),
            "bootstrap storage changed; restart ChaOS before reading or writing settings"
        );
    }
    let runtime = crate::runtime_db::open_or_create_runtime_db_with_config(
        url.as_deref(),
        &sqlite_home,
        "settings",
    )
    .await
    .context("authoritative settings storage is unavailable")?;
    let mut mounts = MOUNTS
        .lock()
        .map_err(|_| anyhow::anyhow!("settings mount lock poisoned"))?;
    ensure!(
        mounts
            .get(home)
            .is_none_or(|previous| previous == &identity),
        "bootstrap storage changed while connecting; restart ChaOS"
    );
    mounts.insert(home.to_owned(), identity);
    Ok(runtime)
}

pub fn install_persistence() {
    chaos_sysctl::persistence::install(Arc::new(KernelSettingsPersistence));
}

struct KernelSettingsPersistence;

impl SettingsPersistence for KernelSettingsPersistence {
    fn snapshot<'a>(&'a self, home: &'a Path) -> PersistenceFuture<'a, SettingsSnapshot> {
        Box::pin(snapshot(home))
    }

    fn commit<'a>(
        &'a self,
        home: &'a Path,
        revision: i64,
        settings: Value,
    ) -> PersistenceFuture<'a, ()> {
        Box::pin(async move {
            validate(&settings, home)?;
            let runtime = open(home).await?;
            runtime
                .commit_settings(revision, &serde_json::to_value(settings)?, None)
                .await?;
            Ok(())
        })
    }
}

pub async fn snapshot(home: &Path) -> anyhow::Result<SettingsSnapshot> {
    install_persistence();
    let file = read_toml(home)?;
    let legacy = file
        .as_table()
        .context("configuration must be a table")?
        .keys()
        .any(|key| !BOOTSTRAP_KEYS.contains(&key.as_str()));
    ensure!(
        !legacy,
        "user settings must be migrated: run `chaos config migrate --dry-run`, then `chaos config migrate`"
    );
    let snapshot = open(home).await?.settings_snapshot().await?;
    ensure!(
        snapshot.migration_digest.is_some() || legacy_rule_sources(home)?.is_empty(),
        "legacy user rules must be migrated: run `chaos config migrate`"
    );
    let settings = serde_json::from_value::<Value>(snapshot.settings)?;
    validate(&settings, home)?;
    Ok(SettingsSnapshot {
        revision: snapshot.revision,
        settings,
    })
}

/// Reject grants in generic settings; restrictions remain valid.
pub fn validate(settings: &Value, home: &Path) -> anyhow::Result<()> {
    let table = settings.as_table().context("settings must be a table")?;
    static SCHEMA: std::sync::LazyLock<serde_json::Value> =
        std::sync::LazyLock::new(|| schemars::schema_for!(crate::config::ConfigToml).to_value());
    let properties = SCHEMA["properties"]
        .as_object()
        .context("missing configuration schema")?;
    for key in table.keys() {
        ensure!(
            !BOOTSTRAP_KEYS.contains(&key.as_str()),
            "{key} is bootstrap-only; use `chaos config bootstrap set`"
        );
        ensure!(properties.contains_key(key), "unknown user setting: {key}");
    }
    fn check(value: &Value, parent: &str) -> anyhow::Result<()> {
        match value {
            Value::Table(table) => {
                for (key, value) in table {
                    ensure!(
                        !BOOTSTRAP_KEYS.contains(&key.as_str()),
                        "{key} cannot be nested in user settings"
                    );
                    if matches!(
                        key.as_str(),
                        "bearer_token" | "api_key" | "experimental_bearer_token"
                    ) && value
                        .as_str()
                        .is_some_and(|text| !chaos_sysctl::secrets::is_reference(text))
                    {
                        bail!(
                            "literal credentials are not allowed; use a credential/environment reference"
                        );
                    }
                    if matches!(key.as_str(), "http_headers" | "headers" | "env")
                        && let Some(map) = value.as_table()
                    {
                        ensure!(
                            map.values().all(|value| value
                                .as_str()
                                .is_none_or(chaos_sysctl::secrets::is_reference)),
                            "literal environment/header values must be migrated to credential references"
                        );
                    }
                    if (key.ends_with("_url") || key == "url")
                        && value
                            .as_str()
                            .and_then(|text| url::Url::parse(text).ok())
                            .is_some_and(|url| url.password().is_some())
                    {
                        bail!("credential-bearing URLs must use an environment reference");
                    }
                    if matches!(parent, "mcp_tool_approvals" | "apps")
                        && value.as_str() == Some("approve")
                    {
                        bail!(
                            "positive approvals must be created through the approval interaction"
                        );
                    }
                    check(
                        value,
                        if matches!(key.as_str(), "mcp_tool_approvals" | "apps") {
                            key
                        } else {
                            parent
                        },
                    )?;
                }
            }
            Value::Array(values) => {
                for value in values {
                    check(value, parent)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    check(settings, "")?;
    let _guard = chaos_realpath::AbsolutePathBufGuard::new(home);
    let _: crate::config::ConfigToml = settings
        .clone()
        .try_into()
        .context("invalid user settings")?;
    Ok(())
}

/// Installation IDs are local application state, not database content or export.
pub fn installation_id(home: &Path) -> anyhow::Result<String> {
    std::fs::create_dir_all(home)?;
    let path = home.join("installation-id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        Uuid::parse_str(id.trim())
            .context("invalid installation identity; do not adopt database grants")?;
        return Ok(id.trim().to_owned());
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            let id = Uuid::new_v4().to_string();
            writeln!(file, "{id}")?;
            file.sync_all()?;
            Ok(id)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let id = std::fs::read_to_string(path)?;
            Uuid::parse_str(id.trim()).context("installation identity is not ready; retry")?;
            Ok(id.trim().into())
        }
        Err(err) => Err(err.into()),
    }
}

pub fn fingerprint(value: &serde_json::Value) -> anyhow::Result<String> {
    // Canonicalize maps regardless of serde_json's preserve_order feature.
    fn canonical(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let sorted: std::collections::BTreeMap<_, _> =
                    map.iter().map(|(k, v)| (k.clone(), canonical(v))).collect();
                serde_json::Value::Object(sorted.into_iter().collect())
            }
            serde_json::Value::Array(values) => values.iter().map(canonical).collect(),
            _ => value.clone(),
        }
    }
    Ok(format!(
        "v1:{}",
        digest_hex(&serde_json::to_vec(&canonical(value))?)
    ))
}

fn digest_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn workspace_scope(cwd: &Path) -> anyhow::Result<String> {
    let cwd = std::fs::canonicalize(cwd)?;
    let root = cwd
        .ancestors()
        .find(|path| path.join(".git").exists())
        .unwrap_or(&cwd);
    Ok(root.to_string_lossy().into_owned())
}

pub(crate) async fn put_scoped_approval(
    home: &Path,
    cwd: &Path,
    kind: &str,
    payload: serde_json::Value,
) -> anyhow::Result<()> {
    let subject = fingerprint(&payload)?;
    open(home)
        .await?
        .put_approval(&RememberedApproval {
            id: Uuid::new_v4().to_string(),
            installation_id: installation_id(home)?,
            scope: workspace_scope(cwd)?,
            kind: kind.into(),
            subject,
            identity: "v1".into(),
            state: ApprovalState::Active,
            payload,
        })
        .await
}

pub(crate) async fn shell_policy(home: &Path, cwd: &Path) -> anyhow::Result<chaos_selinux::Policy> {
    let scope = workspace_scope(cwd)?;
    let mut policy = chaos_selinux::Policy::empty();
    for grant in open(home)
        .await?
        .list_approvals(&installation_id(home)?)
        .await?
    {
        if grant.kind == "shell" && grant.scope == scope && grant.state == ApprovalState::Active {
            let prefix: Vec<String> = serde_json::from_value(grant.payload["prefix"].clone())?;
            policy.add_prefix_rule(&prefix, chaos_selinux::Decision::Allow)?;
        }
    }
    Ok(policy)
}

#[derive(Debug, Serialize)]
pub struct MigrationReport {
    pub settings: Vec<String>,
    pub grants_requiring_reapproval: usize,
    pub completed: bool,
}

fn legacy_rule_sources(home: &Path) -> anyhow::Result<Vec<String>> {
    let directory = home.join("rules");
    let mut paths = match std::fs::read_dir(&directory) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    paths.sort();
    let mut sources = Vec::new();
    for path in paths {
        if path.extension().and_then(|extension| extension.to_str()) != Some("decrees") {
            continue;
        }
        sources.push(std::fs::read_to_string(path)?);
    }
    Ok(sources)
}

fn legacy_rule_grants(sources: &[String]) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
    let mut grants = Vec::new();
    for source in sources {
        let mut parser = chaos_selinux::PolicyParser::new();
        parser.parse("legacy user policy", source)?;
        let policy = parser.build();
        ensure!(
            policy.host_executables().is_empty(),
            "move host executable identities to administrator policy before migration"
        );
        for prefix in policy.get_allowed_prefixes() {
            grants.push(("shell".into(), serde_json::json!({"prefix": prefix})));
        }
        for rule in policy.network_rules() {
            if rule.decision == chaos_selinux::Decision::Allow {
                grants.push(("network".into(), serde_json::json!({
                    "host": rule.host, "protocol": rule.protocol.as_policy_string(), "decision": "allow"
                })));
            }
        }
    }
    Ok(grants)
}

/// Migrate explicitly; retry cleanup without overwriting newer settings.
pub async fn migrate(home: &Path, dry_run: bool) -> anyhow::Result<MigrationReport> {
    let policy_sources = legacy_rule_sources(home)?;
    let legacy_rules = legacy_rule_grants(&policy_sources)?;
    let source = read_toml(home)?;
    let mut settings =
        crate::config_loader::resolve_relative_paths_in_config_toml(source.clone(), home)?;
    let table = settings
        .as_table_mut()
        .context("configuration must be a table")?;
    for key in BOOTSTRAP_KEYS {
        table.remove(*key);
    }
    let mut legacy = Vec::new();
    fn deactivate(value: &mut Value, path: &str, legacy: &mut Vec<String>) {
        if let Some(table) = value.as_table_mut() {
            for (key, value) in table {
                let path = format!("{path}.{key}");
                if value.as_str() == Some("approve") {
                    legacy.push(path);
                    *value = Value::String("auto".into());
                } else {
                    deactivate(value, &path, legacy);
                }
            }
        }
    }
    for key in ["mcp_tool_approvals", "apps"] {
        if let Some(value) = settings.get_mut(key) {
            deactivate(value, key, &mut legacy);
        }
    }
    // Dry-run validates credential-bearing input without touching the keychain.
    let mut validation_settings = settings.clone();
    fn redact(value: &mut Value) {
        if let Value::Array(values) = value {
            for value in values {
                redact(value);
            }
            return;
        }
        if let Some(table) = value.as_table_mut() {
            for (key, value) in table {
                if matches!(
                    key.as_str(),
                    "bearer_token" | "api_key" | "experimental_bearer_token"
                ) && value.is_str()
                {
                    *value = Value::String("keyring:chaos-settings/dry-run".into());
                } else if matches!(key.as_str(), "env" | "headers" | "http_headers") {
                    if let Some(map) = value.as_table_mut() {
                        for (_, value) in map.iter_mut() {
                            if value.is_str() {
                                *value = Value::String("keyring:chaos-settings/dry-run".into());
                            }
                        }
                    }
                } else {
                    redact(value);
                }
            }
        }
    }
    redact(&mut validation_settings);
    validate(&validation_settings, home)?;
    let mut report = MigrationReport {
        settings: settings
            .as_table()
            .context("settings must be a table")?
            .keys()
            .cloned()
            .collect(),
        grants_requiring_reapproval: legacy.len() + legacy_rules.len(),
        completed: false,
    };
    if dry_run {
        return Ok(report);
    }
    let _lock = lock_bootstrap(home)?;
    let serialized = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };
    ensure!(
        read_toml(home)? == source,
        "configuration changed during migration; retry"
    );
    ensure!(
        legacy_rule_sources(home)? == policy_sources,
        "user policy changed during migration; retry"
    );
    let digest = fingerprint(&serde_json::json!({
        "settings": settings, "policy_sources": policy_sources,
    }))?;
    let runtime = open(home).await?;
    let snapshot = runtime.settings_snapshot().await?;
    if let Some(previous) = &snapshot.migration_digest {
        if !report.settings.is_empty() {
            ensure!(
                previous == &digest,
                "legacy configuration changed after migration; use explicit import"
            );
        }
    } else {
        ensure!(
            snapshot.revision == 0 || report.settings.is_empty(),
            "database settings already exist; use explicit import"
        );
        let backup = home.join(format!(
            "config.toml.backup-{}",
            digest_hex(serialized.as_bytes())
        ));
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&backup) {
            Ok(mut file) => {
                file.write_all(serialized.as_bytes())?;
                file.sync_all()?;
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    std::fs::read_to_string(&backup)? == serialized,
                    "migration backup mismatch"
                );
            }
            Err(err) => return Err(err.into()),
        }
        let mut json = serde_json::to_value(&settings)?;
        chaos_sysctl::secrets::transform(&mut json, true)?;
        settings = serde_json::from_value(json)?;
        validate(&settings, home)?;
        for (name, config) in runtime.list_global_mcp_servers().await? {
            crate::config::ensure_mcp_endpoint_has_no_credentials(&config)?;
            let secure = chaos_sysctl::secrets::externalize_mcp(&config)?;
            if secure != config {
                runtime.upsert_global_mcp_server(&name, &secure).await?;
            }
        }
        let installation = installation_id(home)?;
        let mut pending_grants = legacy
            .into_iter()
            .map(|subject| RememberedApproval {
                id: Uuid::new_v4().to_string(),
                installation_id: installation.clone(),
                scope: "legacy".into(),
                kind: "mcp".into(),
                subject,
                identity: "unverified".into(),
                state: ApprovalState::PendingReapproval,
                payload: serde_json::Value::Null,
            })
            .collect::<Vec<_>>();
        for (kind, payload) in legacy_rules {
            pending_grants.push(RememberedApproval {
                id: Uuid::new_v4().to_string(),
                installation_id: installation.clone(),
                scope: "legacy".into(),
                kind,
                subject: fingerprint(&payload)?,
                identity: "unverified".into(),
                state: ApprovalState::PendingReapproval,
                payload,
            });
        }
        let imported = if snapshot.revision == 0 {
            serde_json::to_value(&settings)?
        } else {
            snapshot.settings
        };
        runtime
            .commit_settings_import(
                snapshot.revision,
                &imported,
                Some(&digest),
                &pending_grants,
                Some(&serde_json::to_value(&policy_sources)?),
            )
            .await?;
    }
    let mut bootstrap: BootstrapConfig = source.try_into()?;
    for value in [&mut bootstrap.storage_url, &mut bootstrap.egress_url]
        .into_iter()
        .flatten()
    {
        if url::Url::parse(value).is_ok_and(|url| url.password().is_some()) {
            *value = chaos_sysctl::secrets::externalize(value)?;
        }
    }
    if bootstrap.resolved_storage_url()?.is_none() {
        let sqlite_home = bootstrap.sqlite_home(home);
        if bootstrap.sqlite_home.is_some() || chaos_proc::sqlite_home_env_value().is_some() {
            bootstrap.storage_url = Some(format!(
                "sqlite://{}",
                sqlite_home.join("chaos.sqlite").display()
            ));
        }
    }
    bootstrap.sqlite_home = None;
    chaos_sysctl::path_utils::write_atomically(
        &home.join("config.toml"),
        &toml::to_string_pretty(&bootstrap)?,
    )?;
    report.completed = true;
    Ok(report)
}

pub(crate) async fn migrated_user_restrictions(
    home: &Path,
) -> anyhow::Result<Option<chaos_selinux::Policy>> {
    let runtime = open(home).await?;
    if runtime
        .settings_snapshot()
        .await?
        .migration_digest
        .is_none()
    {
        return Ok(None);
    }
    let sources: Vec<String> = serde_json::from_value(runtime.user_policy_sources().await?)?;
    let mut policy = chaos_selinux::Policy::empty();
    for source in sources {
        let mut parser = chaos_selinux::PolicyParser::new();
        parser.parse("database user restrictions", &source)?;
        let parsed = parser.build();
        ensure!(
            parsed.host_executables().is_empty(),
            "user policy cannot redefine executable identities"
        );
        policy = policy.merge_overlay(&parsed.restrictions_only());
    }
    Ok(Some(policy))
}

/// Project configuration is not a source of grants or bootstrap redirection.
pub(crate) fn validate_project(value: &Value) -> anyhow::Result<()> {
    if let Some(table) = value.as_table() {
        for (key, value) in table {
            ensure!(
                !matches!(
                    key.as_str(),
                    "storage_url"
                        | "sqlite_home"
                        | "egress_url"
                        | "mcp_tool_approvals"
                        | "apps"
                        | "approval_policy"
                        | "sandbox_mode"
                        | "sandbox_workspace_write"
                        | "permissions"
                        | "default_permissions"
                        | "cli_auth_credentials_store"
                        | "mcp_oauth_credentials_store"
                ),
                "project configuration cannot set security/bootstrap field {key}"
            );
            if value.is_table() {
                validate_project(value)?;
            }
        }
    }
    Ok(())
}
