use std::time::Duration;

use chaos_client::ChaosHttpClient;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::body_string_contains;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use crate::ActionRiskSignals;
use crate::JevBackend;
use crate::Judgment;
use crate::JudgmentKind;
use crate::LocalChat;
use crate::MiniCheckBackend;
use crate::Reflex;
use crate::ReflexBackend;
use crate::ReflexError;
use crate::ShieldGemmaBackend;
use crate::Verdict;
use crate::jev::JevClient;
use crate::local_chat::TopLogprob;
use crate::local_chat::yes_no_from_logprobs;
use crate::local_chat::yes_no_from_text;

mod transport;

fn jev(server: &MockServer) -> JevBackend {
    let client = JevClient::new(ChaosHttpClient::default_client(), server.uri(), "key")
        .with_retry(1, Duration::from_millis(1));
    JevBackend::new("jev", client)
}

fn local(server: &MockServer, model: &str) -> LocalChat {
    LocalChat::new(
        ChaosHttpClient::default_client(),
        format!("{}/v1", server.uri()),
        model,
    )
}

fn chat_reply(content: &str) -> serde_json::Value {
    json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] })
}

fn chat_reply_with_logprobs(content: &str, top: &[(&str, f64)]) -> serde_json::Value {
    let top_logprobs: Vec<serde_json::Value> = top
        .iter()
        .map(|(token, logprob)| json!({ "token": token, "logprob": logprob }))
        .collect();
    json!({ "choices": [{
        "message": { "role": "assistant", "content": content },
        "logprobs": { "content": [{ "token": content, "logprob": top[0].1, "top_logprobs": top_logprobs }] }
    }] })
}

fn unwrap<T, E: std::fmt::Debug>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(err) => panic!("unexpected error: {err:?}"),
    }
}

#[test]
fn yes_no_text_parsing_tolerates_decoration_and_rationale() {
    assert_eq!(unwrap(yes_no_from_text("b", "Yes")), 1.0);
    assert_eq!(unwrap(yes_no_from_text("b", "  no.")), 0.0);
    assert_eq!(unwrap(yes_no_from_text("b", "**Yes**\nStep 1: ...")), 1.0);
    assert!(matches!(
        yes_no_from_text("b", "Maybe"),
        Err(ReflexError::Malformed { .. })
    ));
}

#[test]
fn yes_no_logprobs_renormalize_over_both_labels() {
    let candidates = [
        TopLogprob {
            token: "Yes".into(),
            logprob: (0.6f64).ln(),
        },
        TopLogprob {
            token: "No".into(),
            logprob: (0.2f64).ln(),
        },
        TopLogprob {
            token: " yes".into(),
            logprob: (0.1f64).ln(),
        },
        TopLogprob {
            token: "Pot".into(),
            logprob: (0.1f64).ln(),
        },
    ];
    let probability = yes_no_from_logprobs(&candidates).unwrap_or_default();
    assert!((probability - 0.7 / 0.9).abs() < 1e-9);
    let unrelated = [TopLogprob {
        token: "what".into(),
        logprob: -0.1,
    }];
    assert_eq!(yes_no_from_logprobs(&unrelated), None);
    assert_eq!(yes_no_from_logprobs(&[]), None);
}

#[test]
fn yes_no_logprobs_remain_soft_when_probabilities_underflow() {
    let candidates = [
        TopLogprob {
            token: "Yes".into(),
            logprob: -1000.0,
        },
        TopLogprob {
            token: "No".into(),
            logprob: -1001.0,
        },
    ];
    let probability = yes_no_from_logprobs(&candidates).expect("valid labels");
    assert!((probability - 1.0 / (1.0 + (-1.0_f64).exp())).abs() < 1e-9);

    for logprob in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1.0] {
        assert_eq!(
            yes_no_from_logprobs(&[TopLogprob {
                token: "Yes".into(),
                logprob
            }]),
            None,
        );
    }
}

#[test]
fn verdicts_reject_invalid_probabilities_confidence_and_signals() {
    for invalid in [-0.01, 1.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(Verdict::binary("test", invalid).is_err());
        let mut verdict = Verdict::binary("test", 0.5).expect("valid probability");
        verdict.confidence = invalid;
        assert!(verdict.validate().is_err());
        let mut verdict = Verdict::binary("test", 0.5).expect("valid probability");
        verdict.signals.insert("secondary".into(), invalid);
        assert!(verdict.validate().is_err());
    }
    for probability in [0.0, 0.5, 1.0] {
        assert!(Verdict::binary("test", probability).is_ok());
    }
}

#[test]
fn error_body_is_trimmed_and_bounded_at_a_utf8_boundary() {
    let body = format!("  {}é trailing  ", "x".repeat(2_047));
    assert_eq!(crate::error::bounded_body(&body), "x".repeat(2_047));
    assert_eq!(crate::error::bounded_body("  short  "), "short");
}

#[test]
fn error_categories_do_not_include_provider_content() {
    for (error, category) in [
        (
            ReflexError::Unsupported(JudgmentKind::Grounding),
            "unsupported",
        ),
        (ReflexError::MissingPolicy, "missing_policy"),
        (
            ReflexError::backend("jev", "private provider response"),
            "backend",
        ),
        (
            ReflexError::malformed("jev", "private malformed payload"),
            "malformed",
        ),
    ] {
        assert_eq!(error.category(), category);
    }
}

#[test]
fn debug_output_does_not_expose_backend_credentials() {
    let http = ChaosHttpClient::default_client();
    let jev = JevBackend::new(
        "jev",
        JevClient::new(http.clone(), "http://example.invalid", "secret-jev-key"),
    );
    let chat = LocalChat::new(http, "http://example.invalid", "model")
        .with_api_key(Some("secret-chat-key".into()));
    assert!(!format!("{jev:?}").contains("secret-jev-key"));
    assert!(!format!("{chat:?}").contains("secret-chat-key"));
}

#[test]
fn backends_advertise_their_judgments() {
    let server = MockServer::start();
    let server = unwrap(tokio::runtime::Runtime::new()).block_on(server);
    let jev = jev(&server);
    let minicheck = MiniCheckBackend::new("mc", local(&server, "bespoke-minicheck"));
    let shield = ShieldGemmaBackend::new("sg", local(&server, "shieldgemma"));
    for kind in [
        JudgmentKind::Grounding,
        JudgmentKind::PolicyViolation,
        JudgmentKind::ActionRisk,
    ] {
        assert!(jev.supports(kind));
    }
    assert!(minicheck.supports(JudgmentKind::Grounding));
    assert!(!minicheck.supports(JudgmentKind::PolicyViolation));
    assert!(!minicheck.supports(JudgmentKind::ActionRisk));
    assert!(shield.supports(JudgmentKind::PolicyViolation));
    assert!(!shield.supports(JudgmentKind::Grounding));
    assert!(!shield.supports(JudgmentKind::ActionRisk));
}

#[tokio::test]
async fn router_picks_first_supporting_backend_in_order() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "bespoke-minicheck" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_reply("Yes")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let reflex = Reflex::new(vec![
        Box::new(MiniCheckBackend::new(
            "mc",
            local(&server, "bespoke-minicheck"),
        )),
        Box::new(jev(&server)),
    ]);
    assert_eq!(
        reflex
            .backend_for(JudgmentKind::Grounding)
            .map(ReflexBackend::name),
        Some("mc")
    );
    assert_eq!(
        reflex
            .backend_for(JudgmentKind::ActionRisk)
            .map(ReflexBackend::name),
        Some("jev")
    );

    let verdict = unwrap(
        reflex
            .judge(Judgment::Grounding {
                document: "The sky is blue.".into(),
                claim: "The sky is blue.".into(),
            })
            .await,
    );
    assert_eq!(verdict.backend, "mc");
    assert_eq!(verdict.probability, 1.0);
    assert_eq!(verdict.confidence, 1.0);
}

#[tokio::test]
async fn router_reports_unsupported_judgments() {
    let server = MockServer::start().await;
    let reflex = Reflex::new(vec![Box::new(ShieldGemmaBackend::new(
        "sg",
        local(&server, "shieldgemma"),
    ))]);
    assert!(!reflex.supports(JudgmentKind::ActionRisk));
    let result = reflex
        .judge(Judgment::ActionRisk {
            conversation: json!([]),
            action: json!({}),
            instructions: None,
        })
        .await;
    assert!(matches!(
        result,
        Err(ReflexError::Unsupported(JudgmentKind::ActionRisk))
    ));
    assert!(Reflex::default().is_empty());
}

#[tokio::test]
async fn minicheck_sends_system_prompt_document_and_claim() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({
            "model": "bespoke-minicheck",
            "stream": false,
            "max_tokens": 1,
            "logprobs": true,
            "messages": [
                { "role": "system", "content": crate::minicheck::SYSTEM_PROMPT },
                { "role": "user", "content": "Document: Cats purr.\nClaim: Cats bark." }
            ]
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(chat_reply_with_logprobs(
                "No",
                &[("No", (0.9f64).ln()), ("Yes", (0.1f64).ln())],
            )),
        )
        .expect(1)
        .mount(&server)
        .await;
    let backend = MiniCheckBackend::new("mc", local(&server, "bespoke-minicheck"));
    let verdict = unwrap(
        backend
            .judge(Judgment::Grounding {
                document: "Cats purr.".into(),
                claim: "Cats bark.".into(),
            })
            .await,
    );
    assert!((verdict.probability - 0.1).abs() < 1e-9);
}

#[tokio::test]
async fn local_backends_fall_back_to_the_emitted_token_without_logprobs() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_reply("Yes")))
        .expect(1)
        .mount(&server)
        .await;
    let backend = MiniCheckBackend::new("mc", local(&server, "bespoke-minicheck"));
    let verdict = unwrap(
        backend
            .judge(Judgment::Grounding {
                document: "d".into(),
                claim: "c".into(),
            })
            .await,
    );
    assert_eq!(verdict.probability, 1.0);
}

#[test]
fn shieldgemma_prompt_preserves_explicit_policy_in_model_card_format() {
    let prompt_policy = " \n* \"Cooking\": Only discuss cooking.\n";
    let prompt_only = ShieldGemmaBackend::build_prompt("How do I bake bread?", None, prompt_policy);
    assert!(
        prompt_only
            .starts_with("You are a policy expert trying to help determine whether a user prompt")
    );
    assert!(
        prompt_only
            .contains("<start_of_turn>\nHuman Question: How do I bake bread?\n<end_of_turn>")
    );
    assert!(prompt_only.contains(&format!(
        "Our safety principle is defined in the below:\n\n{prompt_policy}\n\n===\n\n"
    )));
    assert!(prompt_only.contains("Does the human question violate the above principle?"));
    assert!(!prompt_only.contains("Chatbot Response"));

    let response_policy = "* \"No Theft\": The chatbot shall not suggest taking property.";
    let with_response = ShieldGemmaBackend::build_prompt(
        "How can I adopt a llama?",
        Some("Steal one from the zoo."),
        response_policy,
    );
    assert!(
        with_response
            .starts_with("You are a policy expert trying to help determine whether an AI response")
    );
    assert!(
        with_response
            .contains("<start_of_turn>\nChatbot Response: Steal one from the zoo.\n<end_of_turn>")
    );
    assert!(with_response.contains(&format!(
        "Our safety principle is defined in the below:\n\n{response_policy}\n\n===\n\n"
    )));
    assert!(with_response.contains("Does the chatbot response violate the above principle?"));
}

#[tokio::test]
async fn shieldgemma_sends_one_user_turn_and_scores_from_logprobs() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer local-key"))
        .and(body_partial_json(
            json!({ "model": "shieldgemma", "max_tokens": 1, "logprobs": true }),
        ))
        .and(body_string_contains(
            "Chatbot Response: Steal one from the zoo.",
        ))
        .and(body_string_contains("No Theft"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(chat_reply_with_logprobs(
                "Yes",
                &[("Yes", -0.59), ("No", -0.81)],
            )),
        )
        .expect(1)
        .mount(&server)
        .await;
    let chat = local(&server, "shieldgemma").with_api_key(Some("local-key".into()));
    let backend = ShieldGemmaBackend::new("sg", chat);
    let verdict = unwrap(
        backend
            .judge(Judgment::PolicyViolation {
                prompt: "How can I adopt a llama?".into(),
                response: Some("Steal one from the zoo.".into()),
                policy: "* \"No Theft\": The chatbot shall not suggest taking property.".into(),
            })
            .await,
    );
    let expected = (-0.59f64).exp() / ((-0.59f64).exp() + (-0.81f64).exp());
    assert!((verdict.probability - expected).abs() < 1e-9);
}

#[tokio::test]
async fn local_chat_surfaces_http_failures_and_empty_choices() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "broken" })))
        .respond_with(ResponseTemplate::new(503).set_body_string("model not loaded"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "empty" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "choices": [] })))
        .mount(&server)
        .await;
    let judgment = Judgment::Grounding {
        document: "d".into(),
        claim: "c".into(),
    };
    let broken = MiniCheckBackend::new("broken", local(&server, "broken"));
    assert!(matches!(
        broken.judge(judgment.clone()).await,
        Err(ReflexError::Backend { message, .. }) if message.contains("503") && message.contains("model not loaded")
    ));
    let empty = MiniCheckBackend::new("empty", local(&server, "empty"));
    assert!(matches!(
        empty.judge(judgment).await,
        Err(ReflexError::Malformed { .. })
    ));
}

#[tokio::test]
async fn jev_action_risk_asks_four_questions_and_normalizes_score() {
    let server = MockServer::start().await;
    let expected_questions =
        serde_json::to_value(JevBackend::action_risk_questions()).unwrap_or_default();
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(body_partial_json(json!({
            "model": "jev-latest",
            "state": {
                "instructions": "Only touch files under src/.",
                "conversation": [{ "role": "user", "content": "clean up" }],
                "tool_call": { "tool_name": "shell", "command": "rm -rf /" }
            },
            "questions": expected_questions
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": {
                "risk": {
                    "type": "score",
                    "score": 2.7,
                    "legend": {},
                    "probabilities": { "0": 0.0, "1": 0.05, "2": 0.2, "3": 0.75 },
                    "confidence": 0.71
                },
                "irreversible": { "type": "noul", "noul": 0.97 },
                "beyond_request": { "type": "noul", "noul": 0.9 },
                "exfiltrates": { "type": "noul", "noul": 0.02 }
            },
            "usage": { "input_tokens": 200, "output_tokens": 0 }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let verdict = unwrap(
        jev(&server)
            .judge(Judgment::ActionRisk {
                conversation: json!([{ "role": "user", "content": "clean up" }]),
                action: json!({ "tool_name": "shell", "command": "rm -rf /" }),
                instructions: Some("Only touch files under src/.".into()),
            })
            .await,
    );
    assert_eq!(verdict.backend, "jev");
    assert!((verdict.probability - 0.9).abs() < 1e-9);
    assert_eq!(verdict.confidence, 0.71);
    assert_eq!(verdict.signal(ActionRiskSignals::IRREVERSIBLE), Some(0.97));
    assert_eq!(verdict.signal(ActionRiskSignals::BEYOND_REQUEST), Some(0.9));
    assert_eq!(verdict.signal(ActionRiskSignals::EXFILTRATES), Some(0.02));
}

#[tokio::test]
async fn jev_grounding_and_policy_use_single_nouls() {
    let server = MockServer::start().await;
    let rules = "Only discuss this repository.";
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(body_partial_json(
            json!({ "state": { "document": "Cats purr.", "claim": "Cats purr." } }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": { "supported": { "type": "noul", "noul": 0.98 } }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(body_partial_json(
            json!({ "state": { "prompt": "hi", "response": null, "policy": rules } }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": { "violates": { "type": "noul", "noul": 0.01 } }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let backend = jev(&server);
    let grounded = unwrap(
        backend
            .judge(Judgment::Grounding {
                document: "Cats purr.".into(),
                claim: "Cats purr.".into(),
            })
            .await,
    );
    assert_eq!(grounded.probability, 0.98);
    let policy = unwrap(
        backend
            .judge(Judgment::PolicyViolation {
                prompt: "hi".into(),
                response: None,
                policy: rules.into(),
            })
            .await,
    );
    assert_eq!(policy.probability, 0.01);
}

#[tokio::test]
async fn jev_missing_answer_is_malformed_and_http_failure_is_backend_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(body_partial_json(
            json!({ "state": { "claim": "partial" } }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": {}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(body_partial_json(json!({ "state": { "claim": "denied" } })))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let backend = jev(&server);
    assert!(matches!(
        backend
            .judge(Judgment::Grounding {
                document: "d".into(),
                claim: "partial".into()
            })
            .await,
        Err(ReflexError::Malformed { .. })
    ));
    assert!(matches!(
        backend
            .judge(Judgment::Grounding { document: "d".into(), claim: "denied".into() })
            .await,
        Err(ReflexError::Backend { message, .. }) if message.contains("unauthorized")
    ));
}
