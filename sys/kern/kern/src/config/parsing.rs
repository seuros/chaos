//! Deserialization, format parsing, and TOML/JSON config loading.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::Path;

use chaos_ipc::openai_models::ModelsResponse;
use chaos_realpath::AbsolutePathBuf;
use chaos_realpath::AbsolutePathBufGuard;
use toml::Value as TomlValue;

use super::ConfigToml;

/// Deserialize a `ConfigToml` from a merged TOML value, resolving relative
/// paths against `config_base_dir`.
pub(crate) fn deserialize_config_toml_with_base(
    root_value: TomlValue,
    config_base_dir: &Path,
) -> std::io::Result<ConfigToml> {
    // This guard ensures that any relative paths that is deserialized into an
    // [AbsolutePathBuf] is resolved against `config_base_dir`.
    let _guard = AbsolutePathBufGuard::new(config_base_dir);
    root_value
        .try_into()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub(crate) fn load_catalog_json(path: &AbsolutePathBuf) -> std::io::Result<ModelsResponse> {
    let file_contents = std::fs::read_to_string(path)?;
    let catalog = serde_json::from_str::<ModelsResponse>(&file_contents).map_err(|err| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "failed to parse model_catalog_json path `{}` as JSON: {err}",
                path.display()
            ),
        )
    })?;
    if catalog.models.is_empty() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "model_catalog_json path `{}` must contain at least one model",
                path.display()
            ),
        ));
    }
    Ok(catalog)
}

pub(crate) fn load_model_catalog(
    model_catalog_json: Option<AbsolutePathBuf>,
) -> std::io::Result<Option<ModelsResponse>> {
    model_catalog_json
        .map(|path| load_catalog_json(&path))
        .transpose()
}

pub(crate) fn deserialize_model_providers<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, crate::model_provider_info::ModelProviderInfo>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use crate::model_provider_info::ModelProviderInfo;
    use crate::model_provider_info::built_in_model_providers;
    use serde::Deserialize;

    let raw = HashMap::<String, toml::Value>::deserialize(deserializer)?;
    let built_ins = built_in_model_providers();
    let mut out: HashMap<String, ModelProviderInfo> = HashMap::with_capacity(raw.len());

    for (key, value) in raw {
        let provider = if let Some(baseline) = built_ins.get(&key) {
            // Partial overrides of built-in providers (e.g. test harnesses
            // redirecting `openai.base_url` at a mock server) merge onto
            // the built-in baseline so `name` and other required fields
            // don't have to be restated.
            let mut merged = toml::Value::try_from(baseline).map_err(serde::de::Error::custom)?;
            merge_toml_value(&mut merged, value);
            ModelProviderInfo::deserialize(merged).map_err(serde::de::Error::custom)?
        } else {
            ModelProviderInfo::deserialize(value).map_err(serde::de::Error::custom)?
        };
        out.insert(key, provider);
    }

    Ok(out)
}

fn merge_toml_value(base: &mut TomlValue, overlay: TomlValue) {
    match (base, overlay) {
        (TomlValue::Table(base_tbl), TomlValue::Table(overlay_tbl)) => {
            for (k, v) in overlay_tbl {
                match base_tbl.get_mut(&k) {
                    Some(existing) => merge_toml_value(existing, v),
                    None => {
                        base_tbl.insert(k, v);
                    }
                }
            }
        }
        (slot, overlay) => {
            *slot = overlay;
        }
    }
}

/// Load the global config as a raw `ConfigToml` (without applying requirements).
///
/// DEPRECATED: Use `Config::load_with_cli_overrides()` instead because working
/// with `ConfigToml` directly means that `ConfigRequirements` have not been
/// applied yet, which risks failing to enforce required constraints.
pub async fn load_config_as_toml_with_cli_overrides(
    chaos_home: &Path,
    cwd: &AbsolutePathBuf,
    cli_overrides: Vec<(String, TomlValue)>,
) -> std::io::Result<ConfigToml> {
    use crate::config_loader::LoaderOverrides;
    use crate::config_loader::load_config_layers_state;

    if let Err(err) = super::serialization::maybe_migrate_smart_approvals_alias(chaos_home).await {
        tracing::warn!(error = %err, "failed to migrate smart_approvals feature alias");
    }
    let config_layer_stack = load_config_layers_state(
        chaos_home,
        Some(cwd.clone()),
        &cli_overrides,
        LoaderOverrides::default(),
    )
    .await?;

    let merged_toml = config_layer_stack.effective_config();
    let cfg = deserialize_config_toml_with_base(merged_toml, chaos_home).map_err(|e| {
        tracing::error!("Failed to deserialize overridden config: {e}");
        e
    })?;

    Ok(cfg)
}
