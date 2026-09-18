use pretty_assertions::assert_eq;

use super::*;

#[test]
fn resource_size_deserializes_without_narrowing() {
    let resource = serde_json::json!({
        "name": "big",
        "uri": "file:///tmp/big",
        "size": 5_000_000_000u64,
    });

    let parsed = Resource::from_mcp_value(resource).expect("should deserialize");
    assert_eq!(parsed.size, Some(5_000_000_000));

    let resource = serde_json::json!({
        "name": "negative",
        "uri": "file:///tmp/negative",
        "size": -1,
    });

    let parsed = Resource::from_mcp_value(resource).expect("should deserialize");
    assert_eq!(parsed.size, Some(-1));

    let resource = serde_json::json!({
        "name": "too_big_for_i64",
        "uri": "file:///tmp/too_big_for_i64",
        "size": 18446744073709551615u64,
    });

    let parsed = Resource::from_mcp_value(resource).expect("should deserialize");
    assert_eq!(parsed.size, None);
}
