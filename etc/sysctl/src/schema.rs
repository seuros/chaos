use crate::features::FEATURES;
use schemars::Schema;
use schemars::SchemaGenerator;
use serde_json::Map;
use serde_json::Value;

/// Schema for the `[features]` map with known keys only.
pub fn features_schema(schema_gen: &mut SchemaGenerator) -> Schema {
    let mut properties = Map::new();
    for feature in FEATURES {
        properties.insert(
            feature.key.to_string(),
            schema_gen.subschema_for::<bool>().to_value(),
        );
    }

    let mut schema = Schema::default();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert("properties".to_string(), Value::Object(properties));
    schema.insert("additionalProperties".to_string(), false.into());
    schema
}
