//! Settings storage without runtime-service initialization.
use anyhow::{Context, bail, ensure};
use chaos_proc::RuntimeDbHandle;
use chaos_sysctl::persistence::{
    PersistenceFuture, SettingsPersistence, SettingsScope, SettingsSnapshot,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use toml::Value;

mod approvals;
mod bootstrap;
pub mod rules;
pub mod storage_setup;

#[cfg(test)]
mod tests;

pub use approvals::{fingerprint, installation_id, workspace_scope};
pub(crate) use approvals::{put_scoped_approval, shell_policy};
pub use bootstrap::{
    BOOTSTRAP_KEYS, BootstrapConfig, ResolvedBootstrap, resolve_bootstrap, set_bootstrap,
};
pub(crate) use rules::database_user_restrictions;

pub async fn open(home: &Path) -> anyhow::Result<RuntimeDbHandle> {
    let bootstrap = resolve_bootstrap_for_open(home).await?;
    let identity = fingerprint(&serde_json::json!({"url": bootstrap.storage_url}))?;
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
    let runtime = crate::runtime_db::open_or_create_runtime_db(
        &bootstrap.storage_url,
        &bootstrap.storage_home,
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

/// Resolve bootstrap for a storage open. A running process keeps the storage it
/// already resolved for `home` if the bootstrap file disappears; a file that is
/// present but invalid still fails closed.
async fn resolve_bootstrap_for_open(home: &Path) -> anyhow::Result<ResolvedBootstrap> {
    static RESOLVED: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, ResolvedBootstrap>>,
    > = std::sync::LazyLock::new(Default::default);
    // Native Keychain access and vault decryption can block for an unlock.
    // Never perform them on the async runtime thread serving other sessions.
    match resolve_bootstrap(home).await {
        Ok(bootstrap) => {
            RESOLVED
                .lock()
                .map_err(|_| anyhow::anyhow!("bootstrap cache lock poisoned"))?
                .insert(home.to_owned(), bootstrap.clone());
            Ok(bootstrap)
        }
        Err(err) if !BootstrapConfig::path(home).exists() => RESOLVED
            .lock()
            .map_err(|_| anyhow::anyhow!("bootstrap cache lock poisoned"))?
            .get(home)
            .cloned()
            .ok_or(err),
        Err(err) => Err(err),
    }
}

pub fn install_persistence() {
    chaos_sysctl::persistence::install(Arc::new(KernelSettingsPersistence));
}

struct KernelSettingsPersistence;

impl SettingsPersistence for KernelSettingsPersistence {
    fn snapshot<'a>(
        &'a self,
        home: &'a Path,
        scope: SettingsScope,
    ) -> PersistenceFuture<'a, SettingsSnapshot> {
        Box::pin(scoped_snapshot(home, scope))
    }

    fn commit<'a>(
        &'a self,
        home: &'a Path,
        scope: SettingsScope,
        revision: i64,
        settings: Value,
    ) -> PersistenceFuture<'a, ()> {
        Box::pin(commit(home, scope, revision, settings))
    }
}

pub async fn commit(
    home: &Path,
    scope: SettingsScope,
    revision: i64,
    settings: Value,
) -> anyhow::Result<()> {
    validate(&settings, home)?;
    let runtime = open(home).await?;
    let settings = serde_json::to_value(settings)?;
    match scope {
        SettingsScope::Global => {
            runtime.commit_settings(revision, &settings, None).await?;
        }
        SettingsScope::Installation => {
            let installation = installation_id(home)?;
            runtime
                .commit_installation_settings(&installation, revision, &settings)
                .await?;
        }
    }
    Ok(())
}

/// The shared user document, before any installation overrides.
pub async fn snapshot(home: &Path) -> anyhow::Result<SettingsSnapshot> {
    scoped_snapshot(home, SettingsScope::Global).await
}

/// This installation's own overrides.
pub async fn installation_snapshot(home: &Path) -> anyhow::Result<SettingsSnapshot> {
    scoped_snapshot(home, SettingsScope::Installation).await
}

pub async fn scoped_snapshot(
    home: &Path,
    scope: SettingsScope,
) -> anyhow::Result<SettingsSnapshot> {
    install_persistence();
    let runtime = open(home).await?;
    let snapshot = match scope {
        SettingsScope::Global => runtime.settings_snapshot().await?,
        SettingsScope::Installation => {
            let installation = installation_id(home)?;
            runtime
                .installation_settings_snapshot(&installation)
                .await?
        }
    };
    let settings = serde_json::from_value::<Value>(snapshot.settings)?;
    validate(&settings, home)?;
    Ok(SettingsSnapshot {
        revision: snapshot.revision,
        settings,
    })
}

/// The user document overlaid by this installation's overrides.
pub async fn effective_settings(home: &Path) -> anyhow::Result<Value> {
    let mut merged = snapshot(home).await?.settings;
    chaos_sysctl::merge_toml_values(&mut merged, &installation_snapshot(home).await?.settings);
    Ok(merged)
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
                            "literal environment/header values must use credential references"
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
