use super::*;
use crate::reflex::diagnostics::{TestReport, test};
use pretty_assertions::assert_eq;
use std::time::Duration;

#[tokio::test]
#[serial]
async fn live_check_uses_saved_key_and_only_sends_the_synthetic_action() {
    let _keyring = chaos_keyring::tests::MockKeyringStore::default();
    let server = MockServer::start().await;
    let (_, context) = make_session_and_context().await;
    let mut config = (*context.config).clone();
    config.reflex = jev_settings(&server);
    let settings = config.reflex.get_mut("jev").unwrap();
    settings.env_key = None;
    settings.api_key = Some(chaos_sysctl::secrets::externalize("saved-test-key").unwrap());
    settings.model = Some("test-route".into());
    settings.path = Some("/custom/decisions".into());
    Mock::given(method("POST"))
        .and(path("/custom/decisions"))
        .and(header("authorization", "Bearer saved-test-key"))
        .and(body_partial_json(json!({"model": "test-route"})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(25))
                .set_body_json(risk_answers(0.3, 0.9, 0.01)),
        )
        .expect(1)
        .mount(&server)
        .await;

    let report = test(&config, None).await.unwrap();
    assert_eq!(report.verdict.backend, "jev");
    assert!((report.verdict.probability - 0.1).abs() < 1e-9);
    assert_eq!(report.model, "test-route");
    assert!(report.elapsed >= Duration::from_millis(25));
    assert!(report.to_string().contains("Verdict: allow"));
    assert!(!format!("{report:?}").contains("saved-test-key"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = requests[0].body_json().unwrap();
    assert_eq!(
        body["state"],
        json!({
            "conversation": [{
                "role": "user",
                "content": "Read the local README.md file. Do not change anything."
            }],
            "instructions": null,
            "tool_call": {
                "type": "mcp_tool_call",
                "tool_name": "read_file",
                "arguments": { "file_path": "README.md" }
            }
        })
    );
}

#[tokio::test]
#[serial]
async fn failed_live_checks_do_not_fallback_or_expose_upstream_error_bodies() {
    let _guard = EnvVarGuard::set(TEST_ENV_KEY, OsStr::new("reflex-key"));
    let server = MockServer::start().await;
    let (_, context) = make_session_and_context().await;
    let mut config = (*context.config).clone();
    config.reflex = jev_settings(&server);
    let mut later = config.reflex["jev"].clone();
    later.path = Some("/must-not-run".into());
    config.reflex.insert("z-later".into(), later);
    for response in [
        ResponseTemplate::new(401).set_body_string("upstream echoed Bearer reflex-key"),
        ResponseTemplate::new(200).set_body_json(json!({"answers": "reflex-key"})),
    ] {
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let err = test(&config, None).await.unwrap_err();
        assert!(err.to_string().contains("no fallback was attempted"));
        assert!(!format!("{err:?}").contains("reflex-key"));
        assert!(err.source().is_none());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        server.reset().await;
    }
}

#[tokio::test]
#[serial]
async fn missing_saved_key_is_an_error_not_a_skipped_backend() {
    let _keyring = chaos_keyring::tests::MockKeyringStore::default();
    let server = MockServer::start().await;
    let (_, context) = make_session_and_context().await;
    let mut config = (*context.config).clone();
    config.reflex = jev_settings(&server);
    let settings = config.reflex.get_mut("jev").unwrap();
    settings.env_key = None;
    settings.api_key = Some("keyring:chaos-settings/7b795f3b-2586-45d4-ac42-dbd17e8f1ad7".into());
    let mut later = settings.clone();
    later.api_key = Some(chaos_sysctl::secrets::externalize("later-key").unwrap());
    config.reflex.insert("z-later".into(), later);
    let err = test(&config, None).await.unwrap_err();
    assert!(format!("{err:#}").contains("saved reflex credential is unavailable"));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn live_check_requires_an_action_risk_backend() {
    let (_, context) = make_session_and_context().await;
    let mut config = (*context.config).clone();
    config.reflex.clear();
    assert!(
        test(&config, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("No action-risk")
    );
    config.reflex = crate::reflex::configuration::presets()
        .into_iter()
        .filter(|(_, settings)| settings.kind != ReflexKind::Jev)
        .map(|(name, settings)| (name.to_string(), settings))
        .collect();
    assert!(
        test(&config, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("No action-risk")
    );
}

#[test]
fn report_uses_the_kernel_decision_thresholds() {
    for (risk, confidence, expected) in [
        (0.1, 0.9, "allow"),
        (0.1, 0.1, "ask user"),
        (0.9, 0.9, "block"),
    ] {
        let report = TestReport {
            verdict: verdict(risk, confidence, &[]),
            model: "jev-latest".into(),
            elapsed: Duration::from_millis(42),
        };
        let text = report.to_string();
        assert!(text.contains(&format!("Verdict: {expected}")));
        assert!(text.contains("Latency: 42 ms"));
    }
}
