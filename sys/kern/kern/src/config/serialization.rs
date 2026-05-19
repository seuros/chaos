//! Serialization, writing config back out, and migration helpers.

use std::path::Path;

use toml_edit::value;

use crate::config::edit::ConfigEdit;
use crate::config::edit::ConfigEditsBuilder;

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
