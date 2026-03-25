use crate::features::FEATURES;
use crate::features::legacy_feature_keys;
use crate::types::RawMcpServerConfig;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::InstanceType;
use schemars::schema::ObjectValidation;
use schemars::schema::Schema;
use schemars::schema::SchemaObject;

/// Schema for the `[features]` map with known + legacy keys only.
pub fn features_schema(schema_gen: &mut SchemaGenerator) -> Schema {
    let mut object = SchemaObject {
        instance_type: Some(InstanceType::Object.into()),
        ..Default::default()
    };

    let mut validation = ObjectValidation::default();
    for feature in FEATURES {
        validation
            .properties
            .insert(feature.key.to_string(), schema_gen.subschema_for::<bool>());
    }
    for legacy_key in legacy_feature_keys() {
        validation
            .properties
            .insert(legacy_key.to_string(), schema_gen.subschema_for::<bool>());
    }
    validation.additional_properties = Some(Box::new(Schema::Bool(false)));
    object.object = Some(Box::new(validation));

    Schema::Object(object)
}

/// Schema for the `[mcp_servers]` map using the raw input shape.
pub fn mcp_servers_schema(schema_gen: &mut SchemaGenerator) -> Schema {
    let mut object = SchemaObject {
        instance_type: Some(InstanceType::Object.into()),
        ..Default::default()
    };

    let validation = ObjectValidation {
        additional_properties: Some(Box::new(schema_gen.subschema_for::<RawMcpServerConfig>())),
        ..Default::default()
    };
    object.object = Some(Box::new(validation));

    Schema::Object(object)
}
