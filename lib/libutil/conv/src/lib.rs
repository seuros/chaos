use serde_json::Value as JsonValue;
use toml::Value as TomlValue;

/// Convert a `serde_json::Value` into a semantically equivalent `toml::Value`.
pub fn json_to_toml(v: JsonValue) -> TomlValue {
    match v {
        JsonValue::Null => TomlValue::String(String::new()),
        JsonValue::Bool(b) => TomlValue::Boolean(b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                TomlValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                TomlValue::Float(f)
            } else {
                TomlValue::String(n.to_string())
            }
        }
        JsonValue::String(s) => TomlValue::String(s),
        JsonValue::Array(arr) => TomlValue::Array(arr.into_iter().map(json_to_toml).collect()),
        JsonValue::Object(map) => {
            let tbl = map
                .into_iter()
                .map(|(k, v)| (k, json_to_toml(v)))
                .collect::<toml::value::Table>();
            TomlValue::Table(tbl)
        }
    }
}
