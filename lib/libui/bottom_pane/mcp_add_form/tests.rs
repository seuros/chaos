use super::*;
use crate::test_support::make_app_event_sender;
use chaos_kern::config_loader::ConfigLayerStack;
use std::collections::HashMap;
use tempfile::tempdir;

fn make_form() -> McpAddForm {
    let sender = make_app_event_sender();
    McpAddForm::new(
        std::env::temp_dir(),
        ConfigLayerStack::default(),
        None,
        sender,
    )
}

pub(crate) fn mcp_add_form_suite() {
    initial_field_is_name();
    tab_advances_field();
    shift_tab_goes_back();
    esc_marks_complete();
    submit_without_name_shows_error();
    args_parsing_uses_shell_style_quoting();
    args_parsing_rejects_unmatched_quotes();
    env_parsing_splits_key_value();
    http_field_parses_bearer_env_var_name();
    http_field_parses_headers();
    http_field_preserves_commas_inside_header_values();
    http_field_preserves_commas_before_embedded_equals_in_header_values();
    http_field_rejects_mixing_headers_and_env_var_name();
    submit_without_active_process_does_not_write_mcp_json();
}

fn initial_field_is_name() {
    let form = make_form();
    assert_eq!(form.focused, FIELD_NAME);
}

fn tab_advances_field() {
    let mut form = make_form();
    form.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(form.focused, FIELD_COMMAND);
}

fn shift_tab_goes_back() {
    let mut form = make_form();
    form.focused = FIELD_COMMAND;
    form.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
    assert_eq!(form.focused, FIELD_NAME);
}

fn esc_marks_complete() {
    let mut form = make_form();
    form.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(form.is_complete());
}

fn submit_without_name_shows_error() {
    let mut form = make_form();
    // Jump straight to submit path by sitting on final field and pressing Enter
    form.focused = FIELD_ENV;
    form.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(form.error.is_some());
    assert!(!form.is_complete());
}

fn args_parsing_uses_shell_style_quoting() {
    let args = parse_stdio_args(r#"--root "/Users/Alice/My Project" --json '{"k":"v"}'"#)
        .expect("parse shell-style args");
    assert_eq!(
        args,
        vec![
            "--root",
            "/Users/Alice/My Project",
            "--json",
            "{\"k\":\"v\"}",
        ]
    );
}

fn args_parsing_rejects_unmatched_quotes() {
    let err = parse_stdio_args(r#""unterminated"#).expect_err("should reject bad quoting");
    assert!(err.contains("unmatched quotes"));
}

fn env_parsing_splits_key_value() {
    let env: HashMap<String, String> = "FOO=bar,BAZ=qux"
        .split(',')
        .filter_map(|pair| {
            let mut parts = pair.trim().splitn(2, '=');
            let key = parts.next()?.trim().to_string();
            let val = parts.next().unwrap_or("").trim().to_string();
            if key.is_empty() {
                None
            } else {
                Some((key, val))
            }
        })
        .collect();
    assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
    assert_eq!(env.get("BAZ").map(String::as_str), Some("qux"));
}

fn http_field_parses_bearer_env_var_name() {
    assert_eq!(
        parse_http_auth_or_headers("MCP_TOKEN").expect("parse bearer env var"),
        ParsedHttpField {
            bearer_token_env_var: Some("MCP_TOKEN".to_string()),
            http_headers: None,
        }
    );
}

fn http_field_parses_headers() {
    assert_eq!(
        parse_http_auth_or_headers("Authorization=Bearer abc,X-Foo=bar").expect("parse headers"),
        ParsedHttpField {
            bearer_token_env_var: None,
            http_headers: Some(BTreeMap::from([
                ("Authorization".to_string(), "Bearer abc".to_string()),
                ("X-Foo".to_string(), "bar".to_string()),
            ])),
        }
    );
}

fn http_field_preserves_commas_inside_header_values() {
    assert_eq!(
        parse_http_auth_or_headers("Accept=text/html, application/json")
            .expect("parse comma-containing header value"),
        ParsedHttpField {
            bearer_token_env_var: None,
            http_headers: Some(BTreeMap::from([(
                "Accept".to_string(),
                "text/html, application/json".to_string(),
            )])),
        }
    );
}

fn http_field_preserves_commas_before_embedded_equals_in_header_values() {
    assert_eq!(
        parse_http_auth_or_headers("Cookie=a=1,b=2").expect("parse cookie header value"),
        ParsedHttpField {
            bearer_token_env_var: None,
            http_headers: Some(BTreeMap::from([(
                "Cookie".to_string(),
                "a=1,b=2".to_string(),
            )])),
        }
    );
}

fn http_field_rejects_mixing_headers_and_env_var_name() {
    let err = parse_http_auth_or_headers("MCP_TOKEN,Authorization=Bearer abc")
        .expect_err("mixed http auth syntax should fail");
    assert!(err.contains("either KEY=VALUE headers or a single bearer token env var"));
}

fn submit_without_active_process_does_not_write_mcp_json() {
    let tmp = tempdir().expect("tempdir");
    let sender = crate::test_support::make_app_event_sender();
    let mut form = McpAddForm::new(
        tmp.path().to_path_buf(),
        ConfigLayerStack::default(),
        None,
        sender,
    );

    form.fields[FIELD_NAME].insert_str("demo");
    form.fields[FIELD_COMMAND].insert_str("demo-command");
    form.focused = FIELD_ENV;

    form.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        form.error.as_deref(),
        Some("could not reload MCP servers: no active process")
    );
    assert!(!form.is_complete());
    assert!(!form.mcp_json_path().exists());
}
