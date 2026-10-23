use super::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use tracing::Event;
use tracing::Subscriber;
use tracing::field::Visit;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

fn setup_tag_collector() -> Arc<Mutex<BTreeMap<String, String>>> {
    Arc::new(Mutex::new(BTreeMap::new()))
}

#[test]
fn test_try_parse_error_message() {
    let text = r#"{
  "error": {
    "message": "Your refresh token has already been used to generate a new access token. Please try signing in again.",
    "type": "invalid_request_error",
    "param": null,
    "code": "refresh_token_reused"
  }
}"#;
    let message = try_parse_error_message(text);
    assert_eq!(
        message,
        "Your refresh token has already been used to generate a new access token. Please try signing in again."
    );
}

#[test]
fn test_try_parse_error_message_no_error() {
    let text = r#"{"message": "test"}"#;
    let message = try_parse_error_message(text);
    assert_eq!(message, r#"{"message": "test"}"#);
}

#[derive(Default)]
struct TagCollectorVisitor {
    tags: BTreeMap<String, String>,
}

impl Visit for TagCollectorVisitor {
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.tags
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.tags
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.tags
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

#[derive(Clone)]
struct TagCollectorLayer {
    tags: Arc<Mutex<BTreeMap<String, String>>>,
}

impl<S> Layer<S> for TagCollectorLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != "feedback_tags" {
            return;
        }
        let mut visitor = TagCollectorVisitor::default();
        event.record(&mut visitor);
        self.tags.lock().unwrap().extend(visitor.tags);
    }
}

#[test]
fn emit_feedback_request_tags_records_sentry_feedback_fields() {
    let tags = setup_tag_collector();
    let _guard = tracing_subscriber::registry()
        .with(TagCollectorLayer { tags: tags.clone() })
        .set_default();

    emit_feedback_request_tags(&FeedbackRequestTags {
        endpoint: "/responses",
        auth_header_attached: true,
        auth_header_name: Some("authorization"),
        auth_mode: Some("chatgpt"),
        auth_retry_after_unauthorized: Some(false),
        auth_recovery_mode: Some("managed"),
        auth_recovery_phase: Some("refresh_token"),
        auth_connection_reused: Some(true),
        auth_request_id: Some("req-123"),
        auth_cf_ray: Some("ray-123"),
        auth_error: Some("missing_authorization_header"),
        auth_error_code: Some("token_expired"),
        auth_recovery_followup_success: Some(true),
        auth_recovery_followup_status: Some(200),
    });

    let tags = tags.lock().unwrap().clone();
    assert_eq!(
        tags.get("endpoint").map(String::as_str),
        Some("\"/responses\"")
    );
    assert_eq!(
        tags.get("auth_header_attached").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        tags.get("auth_header_name").map(String::as_str),
        Some("\"authorization\"")
    );
    assert_eq!(
        tags.get("auth_request_id").map(String::as_str),
        Some("\"req-123\"")
    );
    assert_eq!(
        tags.get("auth_error_code").map(String::as_str),
        Some("\"token_expired\"")
    );
    assert_eq!(
        tags.get("auth_recovery_followup_success")
            .map(String::as_str),
        Some("\"true\"")
    );
    assert_eq!(
        tags.get("auth_recovery_followup_status")
            .map(String::as_str),
        Some("\"200\"")
    );
}

#[test]
fn feedback_tags_records_debug_only_values() {
    #[derive(Debug)]
    struct OnlyDebug;

    let tags = setup_tag_collector();
    let _guard = tracing_subscriber::registry()
        .with(TagCollectorLayer { tags: tags.clone() })
        .set_default();
    feedback_tags!(model = "serpent", cached = true, debug_only = OnlyDebug);
    let tags = tags.lock().unwrap();
    assert_eq!(
        tags.get("debug_only").map(String::as_str),
        Some("OnlyDebug")
    );
}

#[test]
fn emit_feedback_auth_recovery_tags_preserves_401_specific_fields() {
    let tags = setup_tag_collector();
    let _guard = tracing_subscriber::registry()
        .with(TagCollectorLayer { tags: tags.clone() })
        .set_default();

    emit_feedback_auth_recovery_tags(
        "managed",
        "refresh_token",
        "recovery_succeeded",
        Some("req-401"),
        Some("ray-401"),
        Some("missing_authorization_header"),
        Some("token_expired"),
    );

    let tags = tags.lock().unwrap().clone();
    assert_eq!(
        tags.get("auth_401_request_id").map(String::as_str),
        Some("\"req-401\"")
    );
    assert_eq!(
        tags.get("auth_401_cf_ray").map(String::as_str),
        Some("\"ray-401\"")
    );
    assert_eq!(
        tags.get("auth_401_error").map(String::as_str),
        Some("\"missing_authorization_header\"")
    );
    assert_eq!(
        tags.get("auth_401_error_code").map(String::as_str),
        Some("\"token_expired\"")
    );
}

#[test]
fn emit_feedback_auth_recovery_tags_clears_stale_401_fields() {
    let tags = setup_tag_collector();
    let _guard = tracing_subscriber::registry()
        .with(TagCollectorLayer { tags: tags.clone() })
        .set_default();

    emit_feedback_auth_recovery_tags(
        "managed",
        "refresh_token",
        "recovery_failed_transient",
        Some("req-401-a"),
        Some("ray-401-a"),
        Some("missing_authorization_header"),
        Some("token_expired"),
    );
    emit_feedback_auth_recovery_tags(
        "managed",
        "done",
        "recovery_not_run",
        Some("req-401-b"),
        None,
        None,
        None,
    );

    let tags = tags.lock().unwrap().clone();
    assert_eq!(
        tags.get("auth_401_request_id").map(String::as_str),
        Some("\"req-401-b\"")
    );
    assert_eq!(
        tags.get("auth_401_cf_ray").map(String::as_str),
        Some("\"\"")
    );
    assert_eq!(tags.get("auth_401_error").map(String::as_str), Some("\"\""));
    assert_eq!(
        tags.get("auth_401_error_code").map(String::as_str),
        Some("\"\"")
    );
}

#[test]
fn emit_feedback_request_tags_preserves_latest_auth_fields_after_unauthorized() {
    let tags = setup_tag_collector();
    let _guard = tracing_subscriber::registry()
        .with(TagCollectorLayer { tags: tags.clone() })
        .set_default();

    emit_feedback_request_tags(&FeedbackRequestTags {
        endpoint: "/responses",
        auth_header_attached: true,
        auth_header_name: Some("authorization"),
        auth_mode: Some("chatgpt"),
        auth_retry_after_unauthorized: Some(true),
        auth_recovery_mode: Some("managed"),
        auth_recovery_phase: Some("refresh_token"),
        auth_connection_reused: None,
        auth_request_id: Some("req-123"),
        auth_cf_ray: Some("ray-123"),
        auth_error: Some("missing_authorization_header"),
        auth_error_code: Some("token_expired"),
        auth_recovery_followup_success: Some(false),
        auth_recovery_followup_status: Some(401),
    });

    let tags = tags.lock().unwrap().clone();
    assert_eq!(
        tags.get("auth_request_id").map(String::as_str),
        Some("\"req-123\"")
    );
    assert_eq!(
        tags.get("auth_cf_ray").map(String::as_str),
        Some("\"ray-123\"")
    );
    assert_eq!(
        tags.get("auth_error").map(String::as_str),
        Some("\"missing_authorization_header\"")
    );
    assert_eq!(
        tags.get("auth_error_code").map(String::as_str),
        Some("\"token_expired\"")
    );
    assert_eq!(
        tags.get("auth_recovery_followup_success")
            .map(String::as_str),
        Some("\"false\"")
    );
}

#[test]
fn emit_feedback_request_tags_clears_stale_latest_auth_fields() {
    let tags = setup_tag_collector();
    let _guard = tracing_subscriber::registry()
        .with(TagCollectorLayer { tags: tags.clone() })
        .set_default();

    emit_feedback_request_tags(&FeedbackRequestTags {
        endpoint: "/responses",
        auth_header_attached: true,
        auth_header_name: Some("authorization"),
        auth_mode: Some("chatgpt"),
        auth_retry_after_unauthorized: Some(false),
        auth_recovery_mode: Some("managed"),
        auth_recovery_phase: Some("refresh_token"),
        auth_connection_reused: Some(true),
        auth_request_id: Some("req-123"),
        auth_cf_ray: Some("ray-123"),
        auth_error: Some("missing_authorization_header"),
        auth_error_code: Some("token_expired"),
        auth_recovery_followup_success: Some(true),
        auth_recovery_followup_status: Some(200),
    });
    emit_feedback_request_tags(&FeedbackRequestTags {
        endpoint: "/responses",
        auth_header_attached: true,
        auth_header_name: None,
        auth_mode: None,
        auth_retry_after_unauthorized: None,
        auth_recovery_mode: None,
        auth_recovery_phase: None,
        auth_connection_reused: None,
        auth_request_id: None,
        auth_cf_ray: None,
        auth_error: None,
        auth_error_code: None,
        auth_recovery_followup_success: None,
        auth_recovery_followup_status: None,
    });

    let tags = tags.lock().unwrap().clone();
    assert_eq!(
        tags.get("auth_header_name").map(String::as_str),
        Some("\"\"")
    );
    assert_eq!(tags.get("auth_mode").map(String::as_str), Some("\"\""));
    assert_eq!(
        tags.get("auth_request_id").map(String::as_str),
        Some("\"\"")
    );
    assert_eq!(tags.get("auth_cf_ray").map(String::as_str), Some("\"\""));
    assert_eq!(tags.get("auth_error").map(String::as_str), Some("\"\""));
    assert_eq!(
        tags.get("auth_error_code").map(String::as_str),
        Some("\"\"")
    );
    assert_eq!(
        tags.get("auth_recovery_followup_success")
            .map(String::as_str),
        Some("\"\"")
    );
    assert_eq!(
        tags.get("auth_recovery_followup_status")
            .map(String::as_str),
        Some("\"\"")
    );
}

#[test]
fn normalize_process_name_trims_and_rejects_empty() {
    for (input, expected) in [
        ("   ", None),
        ("  my thread  ", Some("my thread")),
        (
            "  Compaction\n\x1b]0;bad\x07  timing ",
            Some("Compaction ]0;bad timing"),
        ),
    ] {
        assert_eq!(
            normalize_process_name(input).as_deref(),
            expected,
            "{input:?}"
        );
    }
}

#[test]
fn resume_command_selects_identity_and_quotes_names() {
    let process_id = ProcessId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();
    for (name, id, expected) in [
        (
            Some("my-thread"),
            Some(process_id),
            Some("chaos resume my-thread"),
        ),
        (
            None,
            Some(process_id),
            Some("chaos resume 123e4567-e89b-12d3-a456-426614174000"),
        ),
        (None, None, None),
        (
            Some("-starts-with-dash"),
            None,
            Some("chaos resume -- -starts-with-dash"),
        ),
        (Some("two words"), None, Some("chaos resume 'two words'")),
        (
            Some("quote'case"),
            None,
            Some("chaos resume \"quote'case\""),
        ),
    ] {
        assert_eq!(resume_command(name, id).as_deref(), expected, "{name:?}");
    }
}

#[test]
fn resume_commands_include_name_and_id() {
    let process_id = ProcessId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();
    let commands = resume_commands(Some("my thread"), Some(process_id));
    assert_eq!(
        commands,
        vec![
            "chaos resume 'my thread'".to_string(),
            "chaos resume 123e4567-e89b-12d3-a456-426614174000".to_string(),
        ]
    );
}
