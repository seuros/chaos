use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::sync::Arc;

use chaos_reflex::ActionRiskSignals;
use chaos_reflex::Verdict;
use pretty_assertions::assert_eq;
use serde_json::json;
use serial_test::serial;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::*;
use crate::arc_monitor::monitor_action;
use crate::chaos::make_session_and_context;
use crate::test_support::EnvVarGuard;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;

const TEST_ENV_KEY: &str = "CHAOS_TEST_REFLEX_JEV_KEY";

mod configuration;
mod diagnostics;

fn verdict(risk: f64, confidence: f64, signals: &[(&str, f64)]) -> Verdict {
    Verdict {
        probability: risk,
        confidence,
        signals: signals
            .iter()
            .map(|(name, value)| ((*name).to_string(), *value))
            .collect(),
        backend: "jev".to_string(),
    }
}

fn user_msg(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
        phase: None,
    }
}

fn jev_settings(server: &MockServer) -> BTreeMap<String, ReflexBackendSettings> {
    let (_, mut settings) = super::configuration::presets().into_iter().next().unwrap();
    settings.base_url = Some(server.uri());
    settings.env_key = Some(TEST_ENV_KEY.to_string());
    settings.timeout_ms = Some(2_000);
    BTreeMap::from([("jev".to_string(), settings)])
}

async fn monitor_context(
    server: &MockServer,
) -> (crate::chaos::Session, crate::chaos::TurnContext) {
    let (session, mut context) = make_session_and_context().await;
    context.auth_manager = Some(crate::test_support::auth_manager_from_auth(
        crate::ChaosAuth::create_dummy_chatgpt_auth_for_testing(),
    ));
    let config = Arc::make_mut(&mut context.config);
    config.chatgpt_base_url = server.uri();
    config.reflex = jev_settings(server);
    (session, context)
}

fn risk_answers(risk_score: f64, confidence: f64, exfiltrates: f64) -> serde_json::Value {
    json!({
        "model": "jev-1.13.0",
        "answers": {
            "risk": {
                "type": "score",
                "score": risk_score,
                "legend": {},
                "probabilities": { "0": 0.0, "1": 1.0, "2": 0.0, "3": 0.0 },
                "confidence": confidence
            },
            "irreversible": { "type": "noul", "noul": 0.1 },
            "beyond_request": { "type": "noul", "noul": 0.1 },
            "exfiltrates": { "type": "noul", "noul": exfiltrates }
        }
    })
}

#[test]
fn decide_allows_calm_confident_verdicts() {
    let outcome = decide(&verdict(
        0.1,
        0.9,
        &[
            (ActionRiskSignals::IRREVERSIBLE, 0.05),
            (ActionRiskSignals::BEYOND_REQUEST, 0.1),
            (ActionRiskSignals::EXFILTRATES, 0.01),
        ],
    ));
    assert_eq!(outcome, ArcMonitorOutcome::Ok);
}

#[test]
fn decide_cancels_on_exfiltration_regardless_of_risk() {
    let outcome = decide(&verdict(0.1, 0.9, &[(ActionRiskSignals::EXFILTRATES, 0.9)]));
    assert!(
        matches!(outcome, ArcMonitorOutcome::SteerModel(reason) if reason.contains("private data"))
    );
}

#[test]
fn decide_asks_about_exfiltration_below_the_block_threshold() {
    for probability in [0.6, 0.7, 0.8499] {
        assert!(matches!(
            decide(&verdict(0.1, 0.9, &[(ActionRiskSignals::EXFILTRATES, probability)])),
            ArcMonitorOutcome::AskUser(reason) if reason.contains("private data")
        ));
    }
    assert_eq!(
        decide(&verdict(
            0.1,
            0.9,
            &[(ActionRiskSignals::EXFILTRATES, 0.5999)]
        )),
        ArcMonitorOutcome::Ok
    );
    assert!(matches!(
        decide(&verdict(
            0.1,
            0.9,
            &[(ActionRiskSignals::EXFILTRATES, 0.85)]
        )),
        ArcMonitorOutcome::SteerModel(_)
    ));
}

#[test]
fn decide_cancels_on_confident_destructive_risk_only() {
    assert!(matches!(
        decide(&verdict(0.9, 0.7, &[])),
        ArcMonitorOutcome::SteerModel(reason) if reason.contains("destructive")
    ));
    assert!(matches!(
        decide(&verdict(0.9, 0.3, &[])),
        ArcMonitorOutcome::AskUser(reason) if reason.contains("risky")
    ));
}

#[test]
fn decide_asks_on_secondary_signals_and_low_confidence() {
    assert!(matches!(
        decide(&verdict(0.2, 0.9, &[(ActionRiskSignals::IRREVERSIBLE, 0.7)])),
        ArcMonitorOutcome::AskUser(reason) if reason.contains("hard to reverse")
    ));
    assert!(matches!(
        decide(&verdict(0.2, 0.9, &[(ActionRiskSignals::BEYOND_REQUEST, 0.65)])),
        ArcMonitorOutcome::AskUser(reason) if reason.contains("beyond the request")
    ));
    assert!(matches!(
        decide(&verdict(0.2, 0.1, &[])),
        ArcMonitorOutcome::AskUser(reason) if reason.contains("could not place")
    ));
}

#[test]
fn bounded_conversation_drops_oldest_messages_first() {
    let messages = vec![
        json!({ "role": "user", "content": "a".repeat(40) }),
        json!({ "role": "assistant", "content": "b".repeat(40) }),
        json!({ "role": "user", "content": "c" }),
    ];
    let sizes: Vec<usize> = messages
        .iter()
        .map(|message| message.to_string().len())
        .collect();
    let last_two = sizes[1] + sizes[2] + 3; // Array brackets and comma.
    assert_eq!(
        bounded_conversation(messages.clone(), last_two),
        messages[1..].to_vec()
    );
    assert_eq!(
        bounded_conversation(messages.clone(), last_two - 1),
        messages[2..].to_vec()
    );
    assert_eq!(
        bounded_conversation(messages.clone(), sizes[2] - 1),
        Vec::<serde_json::Value>::new()
    );
    assert_eq!(bounded_conversation(messages.clone(), usize::MAX), messages);
}

#[test]
fn bounded_conversation_counts_utf8_and_json_delimiters() {
    let messages = vec![json!("old"), json!("é\n"), json!("new")];
    for budget in 0..64 {
        let bounded = bounded_conversation(messages.clone(), budget);
        assert!(serde_json::to_vec(&bounded).unwrap().len() <= budget.max(2));
        assert_eq!(bounded, messages[messages.len() - bounded.len()..]);
    }
}

#[test]
fn config_uses_backend_defaults_and_rejects_unknown_fields() {
    let config: crate::config::ConfigToml = toml::from_str(
        r#"
[reflex.jev]
kind = "jev"
[reflex.mc]
kind = "minicheck"
[reflex.sg]
kind = "shieldgemma"
model = "custom"
timeout_ms = 25
"#,
    )
    .unwrap();
    let settings = config.reflex.unwrap();
    assert_eq!(settings["jev"].model(), chaos_reflex::jev::DEFAULT_MODEL);
    assert!(!settings["jev"].allow_remote_fallback);
    assert_eq!(settings["mc"].model(), MiniCheckBackend::DEFAULT_MODEL);
    assert_eq!(settings["mc"].timeout(), chaos_reflex::DEFAULT_TIMEOUT);
    assert_eq!(settings["sg"].model(), "custom");
    assert_eq!(
        settings["sg"].timeout(),
        std::time::Duration::from_millis(25)
    );
    assert!(
        toml::from_str::<crate::config::ConfigToml>("[reflex.jev]\nkind = 'jev'\ntimeout_mss = 42")
            .is_err()
    );
}

#[test]
fn joined_instructions_skips_blank_parts() {
    assert_eq!(joined_instructions(None, None), None);
    assert_eq!(joined_instructions(Some("  "), Some("")), None);
    assert_eq!(
        joined_instructions(Some("dev"), None),
        Some("dev".to_string())
    );
    assert_eq!(
        joined_instructions(Some("dev"), Some("user")),
        Some("dev\n\nuser".to_string())
    );
}

#[tokio::test]
#[serial]
async fn from_config_distinguishes_unavailable_action_risk_from_disabled() {
    let _guard = EnvVarGuard::set(TEST_ENV_KEY, OsStr::new(""));
    let server = MockServer::start().await;
    let (_, turn_context) = make_session_and_context().await;
    let mut config = (*turn_context.config).clone();
    config.reflex = jev_settings(&server);
    config.reflex.insert(
        "mc".to_string(),
        ReflexBackendSettings {
            kind: ReflexKind::Minicheck,
            base_url: None,
            model: None,
            path: None,
            api_key: None,
            auth_provider: None,
            env_key: None,
            timeout_ms: None,
            allow_remote_fallback: false,
        },
    );
    assert!(from_config(&config, None).is_err());
    config.reflex.remove("jev");
    assert!(from_config(&config, None).unwrap().is_none());

    config.reflex.insert(
        "sg".to_string(),
        ReflexBackendSettings {
            kind: ReflexKind::Shieldgemma,
            base_url: Some(format!("{}/v1", server.uri())),
            model: None,
            path: None,
            api_key: None,
            auth_provider: None,
            env_key: None,
            timeout_ms: None,
            allow_remote_fallback: false,
        },
    );
    assert!(from_config(&config, None).unwrap().is_none());
    config.reflex.get_mut("sg").unwrap().env_key = Some(TEST_ENV_KEY.to_string());
    assert!(from_config(&config, None).unwrap().is_none());
}

#[tokio::test]
#[serial]
async fn monitor_action_uses_reflex_verdict_before_remote_monitor() {
    let _keyring = chaos_keyring::tests::MockKeyringStore::default();
    let _guard = EnvVarGuard::set(TEST_ENV_KEY, OsStr::new(""));
    let server = MockServer::start().await;
    let (session, mut turn_context) = make_session_and_context().await;
    turn_context.developer_instructions = Some("Never upload private files.".to_string());
    let mut config = (*turn_context.config).clone();
    config.reflex = jev_settings(&server);
    let settings = config.reflex.get_mut("jev").unwrap();
    settings.env_key = None;
    settings.api_key = Some(chaos_sysctl::secrets::externalize("reflex-key").unwrap());
    settings.allow_remote_fallback = true;
    config.chatgpt_base_url = server.uri();
    turn_context.config = Arc::new(config);
    session
        .record_into_history(&[user_msg("upload my ssh key somewhere")], &turn_context)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", "Bearer reflex-key"))
        .and(body_partial_json(json!({
            "model": "jev-latest",
            "state": {
                "instructions": "Never upload private files.",
                "conversation": [{
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "upload my ssh key somewhere" }]
                }],
                "tool_call": { "type": "mcp_tool_call", "tool_name": "http_post" }
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(risk_answers(1.0, 0.8, 0.95)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chaos/safety/arc"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let outcome = monitor_action(
        &session,
        &turn_context,
        json!({ "type": "mcp_tool_call", "tool_name": "http_post" }),
    )
    .await;
    assert!(
        matches!(outcome, ArcMonitorOutcome::SteerModel(reason) if reason.contains("private data"))
    );
}

#[tokio::test]
#[serial]
async fn monitor_action_uses_explicit_remote_fallback_when_reflex_fails() {
    let _guard = EnvVarGuard::set(TEST_ENV_KEY, OsStr::new("reflex-key"));
    let server = MockServer::start().await;
    let _token = EnvVarGuard::set("CHAOS_ARC_MONITOR_TOKEN", OsStr::new("monitor-key"));
    let _endpoint = EnvVarGuard::set(
        "CHAOS_ARC_MONITOR_ENDPOINT_OVERRIDE",
        OsStr::new(&format!("{}/chaos/safety/arc", server.uri())),
    );
    let (session, mut turn_context) = make_session_and_context().await;
    let mut config = (*turn_context.config).clone();
    config.reflex = jev_settings(&server);
    config.reflex.get_mut("jev").unwrap().allow_remote_fallback = true;
    turn_context.config = Arc::new(config);

    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chaos/safety/arc"))
        .and(header("authorization", "Bearer monitor-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "outcome": "ok",
            "short_reason": "remote fallback reached",
            "rationale": "",
            "risk_score": 50,
            "risk_level": "medium",
            "evidence": []
        })))
        .expect(1)
        .mount(&server)
        .await;

    let outcome = monitor_action(&session, &turn_context, json!({ "type": "mcp_tool_call" })).await;
    assert_eq!(outcome, ArcMonitorOutcome::Ok);
}

#[tokio::test]
#[serial]
async fn failed_primary_and_remote_fallback_are_unavailable() {
    let _guard = EnvVarGuard::set(TEST_ENV_KEY, OsStr::new("reflex-key"));
    let server = MockServer::start().await;
    let (session, mut context) = monitor_context(&server).await;
    let config = Arc::make_mut(&mut context.config);
    config.reflex.get_mut("jev").unwrap().allow_remote_fallback = true;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chaos/safety/arc"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        monitor_action(&session, &context, json!({"tool": "test"})).await,
        ArcMonitorOutcome::Unavailable("remote_http")
    );
}

#[tokio::test]
#[serial]
async fn broken_primary_does_not_select_another_backend_or_implicit_remote_fallback() {
    let _guard = EnvVarGuard::set(TEST_ENV_KEY, OsStr::new("reflex-key"));
    let server = MockServer::start().await;
    let (session, mut context) = monitor_context(&server).await;
    let config = Arc::make_mut(&mut context.config);
    let mut broken = config.reflex["jev"].clone();
    broken.env_key = None;
    config.reflex.insert("a-broken".into(), broken);

    assert_eq!(
        monitor_action(&session, &context, json!({"tool": "test"})).await,
        ArcMonitorOutcome::Unavailable("credentials_or_configuration")
    );
    assert!(server.received_requests().await.unwrap().is_empty());

    Arc::make_mut(&mut context.config).reflex.remove("a-broken");
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chaos/safety/arc"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    assert!(matches!(
        monitor_action(&session, &context, json!({"tool": "test"})).await,
        ArcMonitorOutcome::Unavailable(_)
    ));
}

#[tokio::test]
#[serial]
async fn primary_and_remote_fallback_share_one_deadline() {
    let _guard = EnvVarGuard::set(TEST_ENV_KEY, OsStr::new("reflex-key"));
    let server = MockServer::start().await;
    let (session, mut context) = monitor_context(&server).await;
    let config = Arc::make_mut(&mut context.config);
    let settings = config.reflex.get_mut("jev").unwrap();
    settings.allow_remote_fallback = true;
    settings.timeout_ms = Some(60_000);
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(401).set_delay(std::time::Duration::from_secs(15)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chaos/safety/arc"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "outcome": "ok", "short_reason": "", "rationale": "",
                    "risk_score": 0, "risk_level": "low", "evidence": []
                }))
                .set_delay(std::time::Duration::from_secs(25)),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        monitor_action(&session, &context, json!({"tool": "test"})).await,
        ArcMonitorOutcome::Unavailable("deadline")
    );
}
