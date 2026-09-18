use super::*;

#[test]
fn load_regex() {
    // The goal of this test is just to compile all the regex to prevent the panic
    let _ = redact_secrets("secret".to_string());
}
