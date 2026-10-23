use super::*;

#[test]
fn overrides_parse_values_and_only_split_on_first_equals() {
    let overrides = CliConfigOverrides {
        raw_overrides: [
            " model = o3=mini ",
            "enabled=true",
            "limits=[1, 2, 3]",
            "features={a = 1, b = 2}",
        ]
        .map(str::to_owned)
        .to_vec(),
    };
    let parsed = overrides.parse_overrides().expect("parse overrides");
    for (index, (key, value)) in [
        ("model", Value::String("o3=mini".into())),
        ("enabled", Value::Boolean(true)),
        (
            "limits",
            Value::Array(vec![
                Value::Integer(1),
                Value::Integer(2),
                Value::Integer(3),
            ]),
        ),
        (
            "features",
            Value::Table(
                [
                    ("a".into(), Value::Integer(1)),
                    ("b".into(), Value::Integer(2)),
                ]
                .into_iter()
                .collect(),
            ),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(&parsed[index], &(key.to_owned(), value), "override {index}");
    }
}

#[test]
fn invalid_overrides_fail_without_mutating_config() {
    for raw in ["missing_equals", "=true", "  = false"] {
        let overrides = CliConfigOverrides {
            raw_overrides: vec![raw.into()],
        };
        let original = Value::Table(
            [("keep".into(), Value::Boolean(true))]
                .into_iter()
                .collect(),
        );
        let mut config = original.clone();
        assert!(overrides.apply_on_value(&mut config).is_err(), "{raw}");
        assert_eq!(config, original, "{raw}");
    }
}

#[test]
fn applying_overrides_creates_nested_tables_and_replaces_scalar_parents() {
    let overrides = CliConfigOverrides {
        raw_overrides: vec!["mode=original".into(), "mode.nested=42".into()],
    };
    let mut config = Value::Table(Default::default());
    overrides.apply_on_value(&mut config).expect("apply");
    assert_eq!(config["mode"]["nested"].as_integer(), Some(42));
}
