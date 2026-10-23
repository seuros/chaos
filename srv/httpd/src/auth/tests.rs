use super::*;

const TOKEN: &str = "s3cret-tok3n";

#[test]
fn bearer_authentication_requires_the_exact_scheme_and_token() {
    for (label, header, expected) in [
        ("correct bearer", Some("Bearer s3cret-tok3n"), Ok(())),
        ("missing header", None, Err("unauthorized")),
        ("wrong scheme", Some("Basic abc"), Err("unauthorized")),
        ("wrong token", Some("Bearer wrong"), Err("unauthorized")),
        ("empty token", Some("Bearer "), Err("unauthorized")),
        (
            "lowercase scheme",
            Some("bearer s3cret-tok3n"),
            Err("unauthorized"),
        ),
        ("missing space", Some("Bearertoken"), Err("unauthorized")),
    ] {
        assert_eq!(validate_bearer(header, TOKEN), expected, "{label}");
    }
}
