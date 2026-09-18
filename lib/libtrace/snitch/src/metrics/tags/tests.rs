use super::APP_VERSION_TAG;
use super::AUTH_MODE_TAG;
use super::MODEL_TAG;
use super::ORIGINATOR_TAG;
use super::SERVICE_NAME_TAG;
use super::SESSION_SOURCE_TAG;
use super::SessionMetricTagValues;
use chaos_test_fixtures::TEST_MODEL;
use pretty_assertions::assert_eq;

#[test]
fn session_metric_tags_include_expected_tags_in_order() {
    let tags = SessionMetricTagValues {
        auth_mode: Some("api_key"),
        session_source: "cli",
        originator: "codex_cli",
        service_name: Some("desktop_app"),
        model: TEST_MODEL,
        app_version: "1.2.3",
    }
    .into_tags()
    .expect("tags");

    assert_eq!(
        tags,
        vec![
            (AUTH_MODE_TAG, "api_key"),
            (SESSION_SOURCE_TAG, "cli"),
            (ORIGINATOR_TAG, "codex_cli"),
            (SERVICE_NAME_TAG, "desktop_app"),
            (MODEL_TAG, TEST_MODEL),
            (APP_VERSION_TAG, "1.2.3"),
        ]
    );
}

#[test]
fn session_metric_tags_skip_missing_optional_tags() {
    let tags = SessionMetricTagValues {
        auth_mode: None,
        session_source: "exec",
        originator: "chaos_fork",
        service_name: None,
        model: TEST_MODEL,
        app_version: "1.2.3",
    }
    .into_tags()
    .expect("tags");

    assert_eq!(
        tags,
        vec![
            (SESSION_SOURCE_TAG, "exec"),
            (ORIGINATOR_TAG, "chaos_fork"),
            (MODEL_TAG, TEST_MODEL),
            (APP_VERSION_TAG, "1.2.3"),
        ]
    );
}
