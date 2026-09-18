use super::*;

const TOKEN: &str = "s3cret-tok3n";

#[test]
fn accepts_correct_bearer() {
    assert!(validate_bearer(Some("Bearer s3cret-tok3n"), TOKEN).is_ok());
}

#[test]
fn rejects_missing_header() {
    assert_eq!(validate_bearer(None, TOKEN), Err("unauthorized"));
}

#[test]
fn rejects_wrong_scheme() {
    assert_eq!(
        validate_bearer(Some("Basic abc"), TOKEN),
        Err("unauthorized")
    );
}

#[test]
fn rejects_wrong_token() {
    assert_eq!(
        validate_bearer(Some("Bearer wrong"), TOKEN),
        Err("unauthorized")
    );
}

#[test]
fn rejects_empty_bearer_value() {
    assert_eq!(validate_bearer(Some("Bearer "), TOKEN), Err("unauthorized"));
}

#[test]
fn rejects_lowercase_bearer() {
    assert_eq!(
        validate_bearer(Some("bearer s3cret-tok3n"), TOKEN),
        Err("unauthorized")
    );
}

#[test]
fn rejects_no_space_after_bearer() {
    assert_eq!(
        validate_bearer(Some("Bearertoken"), TOKEN),
        Err("unauthorized")
    );
}
