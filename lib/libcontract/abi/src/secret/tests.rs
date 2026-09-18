use super::*;

#[test]
fn secret_redacts_but_deserializes_and_exposes() {
    let s: Secret<String> = Secret::new("sk-ant-abcdef".into());

    assert_eq!(format!("{s}"), "***");
    assert_eq!(format!("{s:?}"), "Secret(\"***\")");
    assert_eq!(s.expose(), "sk-ant-abcdef");

    assert!(serde_json::to_string(&s).is_err());

    let loaded: Secret<String> = serde_json::from_str("\"hunter2\"").unwrap();
    assert_eq!(loaded.expose(), "hunter2");
    assert_eq!(format!("{loaded:?}"), "Secret(\"***\")");
}
