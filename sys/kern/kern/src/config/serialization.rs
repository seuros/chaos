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
