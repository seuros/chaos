//! Opaque credential references in configuration. No plaintext fallback.
use anyhow::{Context, ensure};
use chaos_keyring::{DefaultKeyringStore, KeyringStore};
use serde_json::Value;

const PREFIX: &str = "keyring:chaos-settings/";
const SERVICE: &str = "chaos-settings";

#[cfg(test)]
#[path = "secrets_tests.rs"]
mod tests;

pub fn is_reference(value: &str) -> bool {
    value.starts_with(PREFIX)
}

pub fn resolve(value: &str) -> anyhow::Result<String> {
    let Some(account) = value.strip_prefix(PREFIX) else {
        return Ok(value.into());
    };
    uuid::Uuid::parse_str(account).context("invalid credential reference")?;
    DefaultKeyringStore
        .load(SERVICE, account)?
        .context("configuration credential is unavailable in the secure store")
}

pub fn externalize(value: &str) -> anyhow::Result<String> {
    if is_reference(value) {
        // Validate references offline; resolve them at execution.
        uuid::Uuid::parse_str(&value[PREFIX.len()..]).context("invalid credential reference")?;
        return Ok(value.into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    DefaultKeyringStore.save(SERVICE, &id, value)?;
    ensure!(
        DefaultKeyringStore.load(SERVICE, &id)?.as_deref() == Some(value),
        "secure credential store failed read-back verification"
    );
    Ok(format!("{PREFIX}{id}"))
}

pub fn has_literals(value: &Value) -> bool {
    if let Some(values) = value.as_array() {
        return values.iter().any(has_literals);
    }
    let Some(object) = value.as_object() else {
        return false;
    };
    object.iter().any(|(key, value)| {
        if matches!(
            key.as_str(),
            "bearer_token" | "experimental_bearer_token" | "api_key"
        ) {
            value.as_str().is_some_and(|text| !is_reference(text))
        } else if matches!(key.as_str(), "env" | "http_headers" | "headers") {
            value.as_object().is_some_and(|map| {
                map.values()
                    .any(|value| value.as_str().is_some_and(|text| !is_reference(text)))
            })
        } else {
            has_literals(value)
        }
    })
}

/// Transform credential fields only, never instructions or tool arguments.
pub fn transform(value: &mut Value, store: bool) -> anyhow::Result<()> {
    if let Some(values) = value.as_array_mut() {
        for value in values {
            transform(value, store)?;
        }
        return Ok(());
    }
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    for (key, value) in object {
        if matches!(
            key.as_str(),
            "bearer_token" | "experimental_bearer_token" | "api_key"
        ) {
            if let Some(text) = value.as_str() {
                *value = Value::String(if store {
                    externalize(text)?
                } else {
                    resolve(text)?
                });
            }
        } else if matches!(key.as_str(), "env" | "http_headers" | "headers") {
            if let Some(map) = value.as_object_mut() {
                for value in map.values_mut() {
                    if let Some(text) = value.as_str() {
                        *value = Value::String(if store {
                            externalize(text)?
                        } else {
                            resolve(text)?
                        });
                    }
                }
            }
        } else {
            transform(value, store)?;
        }
    }
    Ok(())
}

pub fn externalize_mcp(
    config: &crate::types::McpServerConfig,
) -> anyhow::Result<crate::types::McpServerConfig> {
    let mut value = serde_json::to_value(config)?;
    transform(&mut value, true)?;
    Ok(serde_json::from_value(value)?)
}
