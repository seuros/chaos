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
