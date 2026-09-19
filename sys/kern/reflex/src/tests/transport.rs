use super::*;
use crate::jev::JevError;
use crate::jev::Question;
use pretty_assertions::assert_eq;
use rama::Service;
use rama::error::BoxError;
use rama::error::extra::OpaqueError;
use rama::http::Body;
use rama::service::service_fn;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

fn scripted_http(statuses: Vec<u16>, body: String) -> (ChaosHttpClient, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let service = service_fn(move |request: rama::http::Request| {
        let index = count.fetch_add(1, Ordering::SeqCst);
        let status = statuses[index.min(statuses.len() - 1)];
        let body = body.clone();
        async move {
            assert_eq!(request.headers()["authorization"], "Bearer test-key");
            Ok::<_, OpaqueError>(
                rama::http::Response::builder()
                    .status(status)
                    .body(Body::from(body))
                    .expect("valid response"),
            )
        }
    })
    .boxed();
    (ChaosHttpClient::new(service), calls)
}

fn client(http: ChaosHttpClient, attempts: u32) -> JevClient {
    JevClient::new(http, "http://example.invalid", "test-key").with_retry(attempts, Duration::ZERO)
}

#[tokio::test]
async fn policy_validation_precedes_routing_and_http() {
    let (http, calls) = scripted_http(vec![200], String::new());
    let backends: [Box<dyn ReflexBackend>; 2] = [
        Box::new(JevBackend::new("jev", client(http.clone(), 1))),
        Box::new(ShieldGemmaBackend::new(
            "shieldgemma",
            LocalChat::new(http, "http://example.invalid", "shieldgemma")
                .with_api_key(Some("test-key".into())),
        )),
    ];
    let router = Reflex::default();
    for policy in ["", " \t\r\n", "\u{2003}"] {
        for response in [None, Some("reply".to_string())] {
            let judgment = Judgment::PolicyViolation {
                prompt: "question".into(),
                response,
                policy: policy.into(),
            };
            for backend in &backends {
                assert!(matches!(
                    backend.judge(judgment.clone()).await,
                    Err(ReflexError::MissingPolicy)
                ));
            }
            assert!(matches!(
                router.judge(judgment).await,
                Err(ReflexError::MissingPolicy)
            ));
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn jev_reuses_http_retry_classification_and_attempt_limits() {
    let reply = json!({
        "model": "jev",
        "answers": { "q": { "type": "noul", "noul": 0.8 } }
    })
    .to_string();
    for (statuses, attempts, expected_calls, ok) in [
        (vec![429, 200], 3, 2, true),
        (vec![503, 200], 3, 2, true),
        (vec![529], 3, 3, false),
        (vec![401], 3, 1, false),
        (vec![422], 3, 1, false),
        (vec![429], 0, 1, false),
    ] {
        let (http, calls) = scripted_http(statuses, reply.clone());
        let result = client(http, attempts)
            .evaluate(
                json!({}),
                [("q".into(), Question::noul("supported?"))].into(),
            )
            .await;
        assert_eq!(result.is_ok(), ok);
        assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
    }
}

#[tokio::test]
async fn malformed_json_is_not_retried() {
    let (http, calls) = scripted_http(vec![200], "not json".into());
    let result = client(http, 3).list_models().await;
    assert!(matches!(result, Err(JevError::Decode(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_action_risk_values_are_malformed_not_clamped() {
    for (score, confidence, signal) in [
        (-1.0, 0.9, 0.1),
        (4.0, 0.9, 0.1),
        (1.0, 1.5, 0.1),
        (1.0, 0.9, -0.1),
    ] {
        let reply = json!({
            "model": "jev",
            "answers": {
                "risk": {
                    "type": "score",
                    "score": score,
                    "confidence": confidence,
                    "probabilities": { "0": 0.0, "1": 1.0, "2": 0.0, "3": 0.0 }
                },
                "irreversible": { "type": "noul", "noul": signal },
                "beyond_request": { "type": "noul", "noul": 0.1 },
                "exfiltrates": { "type": "noul", "noul": 0.1 }
            }
        });
        let (http, calls) = scripted_http(vec![200], reply.to_string());
        let reflex = Reflex::new(vec![Box::new(JevBackend::new("jev", client(http, 3)))]);
        let result = reflex
            .judge(Judgment::ActionRisk {
                conversation: json!([]),
                action: json!({}),
                instructions: None,
            })
            .await;
        assert!(matches!(result, Err(ReflexError::Malformed { .. })));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn incomplete_successful_responses_are_rejected_without_retry() {
    let (http, calls) = scripted_http(
        vec![200],
        json!({
            "model": "jev",
            "answers": {}
        })
        .to_string(),
    );
    let result = client(http, 3)
        .evaluate(
            json!({}),
            [("q".into(), Question::noul("supported?"))].into(),
        )
        .await;
    assert!(matches!(result, Err(JevError::MissingAnswer(key)) if key == "q"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

fn stalled_body_http() -> ChaosHttpClient {
    ChaosHttpClient::new(
        service_fn(|_: rama::http::Request| async {
            let body = Body::from_stream(rama::futures::stream::pending::<
                Result<rama::bytes::Bytes, BoxError>,
            >());
            Ok::<_, OpaqueError>(rama::http::Response::new(body))
        })
        .boxed(),
    )
}

#[tokio::test(start_paused = true)]
async fn both_clients_enforce_the_timeout_after_headers_arrive() {
    let timeout = Duration::from_millis(10);
    let result = client(stalled_body_http(), 1)
        .with_timeout(timeout)
        .list_models()
        .await;
    assert!(matches!(result, Err(JevError::Timeout(value)) if value == timeout));

    let chat = LocalChat::new(stalled_body_http(), "http://example.invalid", "model")
        .with_timeout(timeout);
    assert!(matches!(
        chat.yes_no("local", &[]).await,
        Err(ReflexError::Backend { message, .. }) if message.contains("timeout")
    ));
}
