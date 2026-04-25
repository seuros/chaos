//! Serialization, writing config back out, and migration helpers.

use std::path::Path;

use toml_edit::value;

use crate::config::edit::ConfigEdit;
use crate::config::edit::ConfigEditsBuilder;
use crate::features::FeaturesToml;

use super::ConfigToml;

fn feature_scope_segments(scope: &[String], feature_key: &str) -> Vec<String> {
    let mut segments = scope.to_vec();
    segments.push("features".to_string());
    segments.push(feature_key.to_string());
    segments
}

fn push_smart_approvals_alias_migration_edits(
    edits: &mut Vec<ConfigEdit>,
    scope: &[String],
    features: &FeaturesToml,
) {
    if !features.entries.contains_key("smart_approvals") {
        return;
    }
    // Remove the deprecated smart_approvals key. The guardian approval
    // system it pointed to has been removed entirely.
    edits.push(ConfigEdit::ClearPath {
        segments: feature_scope_segments(scope, "smart_approvals"),
    });
    // Also clean up any lingering guardian_approval flag.
    if features.entries.contains_key("guardian_approval") {
        edits.push(ConfigEdit::ClearPath {
            segments: feature_scope_segments(scope, "guardian_approval"),
        });
    }
}

/// Removes the legacy `smart_approvals` and `guardian_approval` feature
/// flags from `config.toml` since the guardian approval system has been
/// removed.
pub(crate) async fn maybe_migrate_smart_approvals_alias(
    chaos_home: &Path,
) -> std::io::Result<bool> {
    use crate::config::CONFIG_TOML_FILE;

    let config_path = chaos_home.join(CONFIG_TOML_FILE);
    if !tokio::fs::try_exists(&config_path).await? {
        return Ok(false);
    }

    let config_contents = tokio::fs::read_to_string(&config_path).await?;
    let Ok(config_toml) = toml::from_str::<ConfigToml>(&config_contents) else {
        return Ok(false);
    };

    let mut edits = Vec::new();

    let root_scope = Vec::new();
    if let Some(features) = config_toml.features.as_ref() {
        push_smart_approvals_alias_migration_edits(&mut edits, &root_scope, features);
    }

    for (profile_name, profile) in &config_toml.profiles {
        if let Some(features) = profile.features.as_ref() {
            let scope = vec!["profiles".to_string(), profile_name.clone()];
            push_smart_approvals_alias_migration_edits(&mut edits, &scope, features);
        }
    }

    if edits.is_empty() {
        return Ok(false);
    }

    ConfigEditsBuilder::new(chaos_home)
        .with_edits(edits)
        .apply()
        .await
        .map_err(|err| {
            std::io::Error::other(format!(
                "failed to clean up deprecated approval aliases: {err}"
            ))
        })?;
    Ok(true)
}

/// Save the default OSS provider preference to config.toml.
pub fn set_default_oss_provider(chaos_home: &Path, provider: &str) -> std::io::Result<()> {
    // Any non-empty provider string is accepted and written to config.
    if provider.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Invalid OSS provider ''. Provider must not be empty.",
        ));
    }

    let edits = [ConfigEdit::SetPath {
        segments: vec!["oss_provider".to_string()],
        value: value(provider),
    }];

    ConfigEditsBuilder::new(chaos_home)
        .with_edits(edits)
        .apply_blocking()
        .map_err(|err| std::io::Error::other(format!("failed to persist config.toml: {err}")))
}

pub(crate) fn uses_deprecated_instructions_file(
    config_layer_stack: &crate::config_loader::ConfigLayerStack,
) -> bool {
    config_layer_stack
        .layers_high_to_low()
        .into_iter()
        .any(|layer| toml_uses_deprecated_instructions_file(&layer.config))
}

fn toml_uses_deprecated_instructions_file(value: &toml::Value) -> bool {
    let Some(table) = value.as_table() else {
        return false;
    };
    if table.contains_key("experimental_instructions_file") {
        return true;
    }
    let Some(profiles) = table.get("profiles").and_then(toml::Value::as_table) else {
        return false;
    };
    profiles.values().any(|profile| {
        profile.as_table().is_some_and(|profile_table| {
            profile_table.contains_key("experimental_instructions_file")
        })
    })
}
