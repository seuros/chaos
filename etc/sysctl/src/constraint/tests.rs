use super::*;
use pretty_assertions::assert_eq;

fn invalid_value(candidate: impl Into<String>, allowed: impl Into<String>) -> ConstraintError {
    ConstraintError::InvalidValue {
        field_name: "<unknown>",
        candidate: candidate.into(),
        allowed: allowed.into(),
        requirement_source: RequirementSource::Unknown,
    }
}

#[test]
fn allow_any_accepts_values_and_uses_the_type_default() {
    let mut constrained = Constrained::allow_any(5);
    constrained.set(-10).expect("allow any accepts all values");
    assert_eq!(constrained.value(), -10);
    let constrained = Constrained::<i32>::allow_any_from_default();
    assert_eq!(constrained.value(), 0);
}

#[test]
fn allow_only_accepts_its_value_and_rejects_changes_without_mutating() {
    let mut constrained = Constrained::allow_only(5);
    constrained
        .set(5)
        .expect("allowed value should be accepted");

    let err = constrained
        .set(6)
        .expect_err("different value should be rejected");
    assert_eq!(err, invalid_value("6", "[5]"));
    assert_eq!(constrained.value(), 5);
}

#[test]
fn validated_values_normalize_and_probe_without_mutating() -> anyhow::Result<()> {
    let mut constrained = Constrained::normalized(-1, |value| value.max(0))?;
    assert_eq!(constrained.value(), 0);
    constrained.set(-5)?;
    assert_eq!(constrained.value(), 0);
    constrained.set(10)?;
    assert_eq!(constrained.value(), 10);
    let invalid_initial = Constrained::new(0, |value| {
        if *value > 0 {
            Ok(())
        } else {
            Err(invalid_value(value.to_string(), "positive values"))
        }
    });
    assert_eq!(invalid_initial, Err(invalid_value("0", "positive values")));
    let mut constrained = Constrained::new(1, |value| {
        if *value > 0 {
            Ok(())
        } else {
            Err(invalid_value(value.to_string(), "positive values"))
        }
    })
    .expect("initial value should be accepted");

    let err = constrained
        .set(-5)
        .expect_err("negative values should be rejected");
    assert_eq!(err, invalid_value("-5", "positive values"));
    assert_eq!(constrained.value(), 1);
    constrained
        .can_set(&2)
        .expect("can_set should accept positive value");
    let err = constrained
        .can_set(&-1)
        .expect_err("can_set should reject negative value");
    assert_eq!(err, invalid_value("-1", "positive values"));
    assert_eq!(constrained.value(), 1);
    Ok(())
}
