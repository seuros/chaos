use chaos_ipc::api::ConfigLayerMetadata;
use serde_json::Value as JsonValue;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use toml::Value as TomlValue;

pub(super) fn record_origins(
    value: &TomlValue,
    meta: &ConfigLayerMetadata,
    path: &mut Vec<String>,
    origins: &mut HashMap<String, ConfigLayerMetadata>,
) {
    match value {
        TomlValue::Table(table) => {
            for (key, val) in table {
                path.push(key.clone());
                record_origins(val, meta, path, origins);
                path.pop();
            }
        }
        TomlValue::Array(items) => {
            for (idx, item) in (0_i32..).zip(items.iter()) {
                path.push(idx.to_string());
                record_origins(item, meta, path, origins);
                path.pop();
            }
        }
        _ => {
            if !path.is_empty() {
                origins.insert(path.join("."), meta.clone());
            }
        }
    }
}

pub fn version_for_toml(value: &TomlValue) -> String {
    let json = toml_to_json(value);
    let canonical = canonical_json(&json);
    let serialized = canonical.to_string();
    let mut hasher = Sha256::new();
    hasher.update(serialized);
    let hash = hasher.finalize();
    let hex = hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

#[allow(
    clippy::expect_used,
    reason = "TOML values have only string keys and JSON-serializable values"
)]
pub(super) fn toml_to_json(value: &TomlValue) -> JsonValue {
    serde_json::to_value(value).expect("TOML values serialize to JSON")
}

fn canonical_json(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Object(map) => {
            let mut sorted = serde_json::Map::new();
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(key, _)| *key);
            for (key, val) in entries {
                sorted.insert(key.clone(), canonical_json(val));
            }
            JsonValue::Object(sorted)
        }
        JsonValue::Array(items) => JsonValue::Array(items.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_preserves_sorted_json_and_toml_scalar_encoding() -> Result<(), toml::de::Error> {
        let value: TomlValue = toml::from_str(
            r#"
array = [1, true, "x", nan, inf, -inf]
date = 1979-05-27T07:32:00Z
[nested]
z = 1
a = 2
"#,
        )?;
        let expected = r#"{"array":[1,true,"x",null,null,null],"date":{"$__toml_private_datetime":"1979-05-27T07:32:00Z"},"nested":{"a":2,"z":1}}"#;

        assert_eq!(canonical_json(&toml_to_json(&value)).to_string(), expected);
        assert_eq!(
            version_for_toml(&value),
            "sha256:1348a76030004e52572f238e6712175271a397be93b4d56f9c7e99300578b4ca"
        );
        Ok(())
    }
}
