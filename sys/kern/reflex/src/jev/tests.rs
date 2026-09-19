use std::time::Duration;

use chaos_client::ChaosHttpClient;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_json;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::Answer;
use super::JevBackend;
use super::JevClient;
use super::JevError;
use super::Question;
use super::Questions;
use super::SystemOneResponse;
use crate::Judgment;

fn client(server: &MockServer) -> JevClient {
    JevClient::new(ChaosHttpClient::default_client(), server.uri(), "test-key")
        .with_retry(3, Duration::from_millis(1))
}

fn ticket_questions() -> Questions {
    let mut questions = Questions::new();
    questions.insert(
        "department".to_string(),
        Question::choice(
            "Which team should handle this ticket?",
            [
                ("billing", Some("Charges, refunds, invoices")),
                ("technical", Some("Bugs and outages")),
                ("sales", None),
            ],
        ),
    );
    questions.insert(
        "frustration".to_string(),
        Question::score(
            "How frustrated does the customer seem?",
            ["Calm", "Frustrated but civil", "Very angry"],
        ),
    );
    questions.insert(
        "is_urgent".to_string(),
        Question::noul("The message conveys urgency or time-sensitivity"),
    );
    questions
}

fn ticket_answers() -> serde_json::Value {
    json!({
        "model": "jev-1.13.0",
        "answers": {
            "department": {
                "type": "choice",
                "choice": "billing",
                "probabilities": { "billing": 0.84, "technical": 0.159, "sales": 0.001 },
                "confidence": 0.596
            },
            "frustration": {
                "type": "score",
                "score": 1.035,
                "legend": { "0": "Calm", "1": "Frustrated but civil", "2": "Very angry" },
                "probabilities": { "0": 0.1, "1": 0.765, "2": 0.135 },
                "confidence": 0.842
            },
            "is_urgent": { "type": "noul", "noul": 0.999 }
        },
        "usage": { "input_tokens": 312, "output_tokens": 48 }
    })
}

#[test]
fn policy_state_contains_only_caller_rules() {
    let policy = " \nOnly discuss this repository.\n";
    for response in [None, Some("src/main.rs".to_string())] {
        let judgment = Judgment::PolicyViolation {
            prompt: "What files are in src/?".into(),
            response: response.clone(),
            policy: policy.into(),
        };
        judgment.validate().expect("non-empty policy");
        assert_eq!(
            JevBackend::state_for(&judgment),
            json!({
                "policy": policy,
                "prompt": "What files are in src/?",
                "response": response,
            })
        );
    }
}

#[test]
fn questions_serialize_to_documented_wire_shape() {
    let value = serde_json::to_value(ticket_questions()).unwrap_or_default();
    assert_eq!(
        value,
        json!({
            "department": {
                "type": "choice",
                "instructions": "Which team should handle this ticket?",
                "criteria": {
                    "billing": "Charges, refunds, invoices",
                    "sales": null,
                    "technical": "Bugs and outages"
                }
            },
            "frustration": {
                "type": "score",
                "instructions": "How frustrated does the customer seem?",
                "criteria": ["Calm", "Frustrated but civil", "Very angry"]
            },
            "is_urgent": {
                "type": "noul",
                "instructions": "The message conveys urgency or time-sensitivity"
            }
        })
    );
}

#[test]
fn noul_criteria_serialize_with_boolean_keys() {
    let question = Question::noul_with_criteria(
        "Repeat contact?",
        "Mentions a prior ticket",
        "No sign of prior contact",
    );
    let value = serde_json::to_value(question).unwrap_or_default();
    assert_eq!(
        value,
        json!({
            "type": "noul",
            "instructions": "Repeat contact?",
            "criteria": { "true": "Mentions a prior ticket", "false": "No sign of prior contact" }
        })
    );
}

#[test]
fn response_deserializes_documented_wire_shape() {
    let response: SystemOneResponse = serde_json::from_value(ticket_answers()).expect_or_panic();
    response.validate(&ticket_questions()).expect_or_panic();
    assert_eq!(response.model, "jev-1.13.0");
    assert_eq!(response.usage.input_tokens, 312);
    assert_eq!(response.choice("department").ok(), Some(("billing", 0.596)));
    assert_eq!(response.score("frustration").ok(), Some((1.035, 0.842)));
    assert_eq!(response.noul("is_urgent").ok(), Some(0.999));
    assert_eq!(
        response
            .answer("department")
            .ok()
            .and_then(|answer| answer.probability_of("technical")),
        Some(0.159)
    );
    assert!(matches!(
        response.answer("frustration"),
        Ok(Answer::Score { legend, .. }) if legend.len() == 3
    ));
}

#[test]
fn response_validation_rejects_incomplete_or_incompatible_answers() {
    for (pointer, value) in [
        ("/model", json!(" \t")),
        (
            "/answers/is_urgent",
            json!({ "type": "score", "score": 0.0, "confidence": 1.0, "probabilities": {} }),
        ),
        ("/answers/department/choice", json!("unrequested")),
        (
            "/answers/department/probabilities",
            json!({ "billing": 1.0 }),
        ),
        (
            "/answers/department/probabilities",
            json!({ "billing": 0.8, "technical": 0.1, "unrequested": 0.1 }),
        ),
        ("/answers/frustration/probabilities", json!({ "0": 1.0 })),
        (
            "/answers/frustration/probabilities",
            json!({ "0": 0.5, "1": 0.5, "3": 0.0 }),
        ),
    ] {
        let mut payload = ticket_answers();
        *payload.pointer_mut(pointer).expect("fixture field") = value;
        let response: SystemOneResponse = serde_json::from_value(payload).expect_or_panic();
        assert!(
            matches!(
                response.validate(&ticket_questions()),
                Err(JevError::Decode(_))
            ),
            "{pointer}"
        );
    }
    let mut response: SystemOneResponse =
        serde_json::from_value(ticket_answers()).expect_or_panic();
    response.answers.remove("is_urgent");
    assert!(matches!(
        response.validate(&ticket_questions()),
        Err(JevError::MissingAnswer(key)) if key == "is_urgent"
    ));
}

#[test]
fn response_validation_rejects_invalid_numbers_without_clamping() {
    let questions = ticket_questions();
    let original: SystemOneResponse = serde_json::from_value(ticket_answers()).expect_or_panic();
    for invalid in [-0.1, 1.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        for field in [
            "noul",
            "choice confidence",
            "choice probability",
            "score confidence",
            "score probability",
        ] {
            let mut response = original.clone();
            match field {
                "noul" => {
                    response
                        .answers
                        .insert("is_urgent".into(), Answer::Noul { noul: invalid });
                }
                "choice confidence" | "choice probability" => {
                    let Answer::Choice {
                        confidence,
                        probabilities,
                        ..
                    } = response.answers.get_mut("department").expect("choice")
                    else {
                        panic!("choice");
                    };
                    if field == "choice confidence" {
                        *confidence = invalid;
                    } else {
                        probabilities.insert("billing".into(), invalid);
                    }
                }
                _ => {
                    let Answer::Score {
                        confidence,
                        probabilities,
                        ..
                    } = response.answers.get_mut("frustration").expect("score")
                    else {
                        panic!("score");
                    };
                    if field == "score confidence" {
                        *confidence = invalid;
                    } else {
                        probabilities.insert("0".into(), invalid);
                    }
                }
            }
            assert!(response.validate(&questions).is_err(), "{field}: {invalid}");
        }
    }
    for invalid in [-0.1, 2.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut response = original.clone();
        let Answer::Score { score, .. } = response.answers.get_mut("frustration").expect("score")
        else {
            panic!("score");
        };
        *score = invalid;
        assert!(response.validate(&questions).is_err(), "score: {invalid}");
    }
}

#[test]
fn typed_accessors_reject_mismatched_answer_types() {
    let response: SystemOneResponse = serde_json::from_value(ticket_answers()).expect_or_panic();
    assert!(matches!(
        response.noul("department"),
        Err(JevError::Decode(_))
    ));
    assert!(matches!(
        response.score("is_urgent"),
        Err(JevError::Decode(_))
    ));
    assert!(matches!(
        response.choice("missing"),
        Err(JevError::MissingAnswer(key)) if key == "missing"
    ));
}

#[test]
fn validation_rejects_malformed_questions() {
    let empty_instructions = Question::noul("   ");
    assert!(matches!(
        empty_instructions.validate("q"),
        Err(JevError::InvalidQuestion { key, .. }) if key == "q"
    ));

    let too_many = Question::choice(
        "pick",
        (0..256).map(|index| (format!("option_{index}"), None::<String>)),
    );
    assert!(too_many.validate("q").is_err());

    let no_options = Question::choice("pick", Vec::<(String, Option<String>)>::new());
    assert!(no_options.validate("q").is_err());

    let one_level = Question::score("rate", ["only"]);
    assert!(one_level.validate("q").is_err());

    let eleven_levels = Question::score("rate", (0..11).map(|index| format!("level {index}")));
    assert!(eleven_levels.validate("q").is_err());

    assert!(
        Question::score("rate", ["low", "high"])
            .validate("q")
            .is_ok()
    );
}

#[tokio::test]
async fn evaluate_rejects_empty_question_set_without_a_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let result = client(&server)
        .evaluate(json!("state"), Questions::new())
        .await;
    assert!(matches!(result, Err(JevError::NoQuestions)));
}

#[tokio::test]
async fn evaluate_posts_bearer_auth_and_body() {
    let server = MockServer::start().await;
    let mut expected = serde_json::to_value(ticket_questions()).unwrap_or_default();
    let expected_body = json!({
        "state": "My card was charged twice and I need this fixed today.",
        "model": "jev-latest",
        "questions": expected.take(),
    });
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("content-type", "application/json"))
        .and(body_json(expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(ticket_answers()))
        .expect(1)
        .mount(&server)
        .await;

    let response = client(&server)
        .evaluate(
            json!("My card was charged twice and I need this fixed today."),
            ticket_questions(),
        )
        .await
        .expect_or_panic();
    assert_eq!(response.choice("department").ok(), Some(("billing", 0.596)));
}

#[tokio::test]
async fn evaluate_honors_a_custom_decisions_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/alpha/decisions"))
        .and(wiremock::matchers::body_partial_json(
            json!({ "model": "typesafe/jev-1.13" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ticket_answers()))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server)
        .with_path("alpha/decisions/")
        .with_model("typesafe/jev-1.13");
    assert_eq!(client.path(), "/alpha/decisions");
    let response = client
        .evaluate(json!("state"), ticket_questions())
        .await
        .expect_or_panic();
    assert_eq!(response.noul("is_urgent").ok(), Some(0.999));
}

#[tokio::test]
async fn evaluate_does_not_retry_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
        .expect(1)
        .mount(&server)
        .await;
    let result = client(&server)
        .evaluate(json!("state"), ticket_questions())
        .await;
    assert!(matches!(result, Err(JevError::Unauthorized)));
}

#[tokio::test]
async fn evaluate_surfaces_validation_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(
            ResponseTemplate::new(422).set_body_json(json!({ "field": "questions.department" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let result = client(&server)
        .evaluate(json!("state"), ticket_questions())
        .await;
    assert!(matches!(
        result,
        Err(JevError::Validation(body)) if body.contains("questions.department")
    ));
}

#[tokio::test]
async fn evaluate_retries_rate_limits_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ticket_answers()))
        .expect(1)
        .mount(&server)
        .await;
    let response = client(&server)
        .evaluate(json!("state"), ticket_questions())
        .await
        .expect_or_panic();
    assert_eq!(response.noul("is_urgent").ok(), Some(0.999));
}

#[tokio::test]
async fn evaluate_gives_up_after_max_attempts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(529))
        .expect(3)
        .mount(&server)
        .await;
    let result = client(&server)
        .evaluate(json!("state"), ticket_questions())
        .await;
    assert!(matches!(result, Err(JevError::Overloaded)));
}

#[tokio::test]
async fn list_models_decodes_cards() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [
                { "name": "jev-latest", "description": "Alias of jev-1.13.0", "release_date": "2026-09-15" },
                { "name": "jev-preview" }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let models = client(&server).list_models().await.expect_or_panic();
    let names: Vec<&str> = models
        .models
        .iter()
        .map(|card| card.name.as_str())
        .collect();
    assert_eq!(names, vec!["jev-latest", "jev-preview"]);
}

trait ExpectOrPanic<T> {
    fn expect_or_panic(self) -> T;
}

impl<T, E: std::fmt::Debug> ExpectOrPanic<T> for Result<T, E> {
    fn expect_or_panic(self) -> T {
        match self {
            Ok(value) => value,
            Err(err) => panic!("unexpected error: {err:?}"),
        }
    }
}
