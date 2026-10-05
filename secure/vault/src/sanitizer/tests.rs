use super::*;

#[test]
fn redacts_application_secret_patterns() {
    // Calling the sanitizer also compiles every lazy application pattern.
    let cases = [
        (
            "OpenAI key",
            "key sk-0123456789abcdefghij ready",
            "key [REDACTED_SECRET] ready",
        ),
        (
            "AWS access key",
            "key AKIA1234567890ABCDEF ready",
            "key [REDACTED_SECRET] ready",
        ),
        (
            "case-insensitive bearer token",
            "Authorization: bEaReR 0123456789abcdef",
            "Authorization: Bearer [REDACTED_SECRET]",
        ),
        (
            "quoted API key assignment",
            r#"api_key = "abcdefgh""#,
            r#"api_key = "[REDACTED_SECRET]""#,
        ),
        ("ordinary text", "secret", "secret"),
    ];

    for (label, input, expected) in cases {
        assert_eq!(redact_secrets(input.to_string()), expected, "{label}");
    }
}
