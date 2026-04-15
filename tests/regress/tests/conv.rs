//! Public-API tests for `chaos-conv` — JSON values crossing the void into TOML.
//!
//! Two formats, two type systems, one impedance mismatch. JSON has
//! `null`; TOML doesn't. JSON conflates int and float; TOML doesn't.
//! TOML demands tables; JSON shrugs and gives you objects. This test
//! pins down exactly how the conversion picks a side at every fork in
//! the type lattice.

use chaos_conv::json_to_toml;
use pretty_assertions::assert_eq;
use serde_json::Value as JsonValue;
use serde_json::json;
use toml::Value as TomlValue;

#[test]
fn json_to_toml_covers_every_value_variant() {
    // Scalars across both type systems.
    assert_eq!(
        json_to_toml(JsonValue::Null),
        TomlValue::String(String::new())
    );
    assert_eq!(json_to_toml(json!(false)), TomlValue::Boolean(false));
    assert_eq!(json_to_toml(json!(123)), TomlValue::Integer(123));
    assert_eq!(json_to_toml(json!(1.25)), TomlValue::Float(1.25));
    assert_eq!(
        json_to_toml(json!("hello")),
        TomlValue::String("hello".to_string())
    );

    // Arrays carry the element conversion through.
    assert_eq!(
        json_to_toml(json!([true, 1])),
        TomlValue::Array(vec![TomlValue::Boolean(true), TomlValue::Integer(1)])
    );

    // Objects nest into tables — recursion holds.
    let nested = json_to_toml(json!({ "outer": { "inner": 2 } }));
    let expected = {
        let mut inner = toml::value::Table::new();
        inner.insert("inner".into(), TomlValue::Integer(2));
        let mut outer = toml::value::Table::new();
        outer.insert("outer".into(), TomlValue::Table(inner));
        TomlValue::Table(outer)
    };
    assert_eq!(nested, expected);
}
