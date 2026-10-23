use super::*;
use pretty_assertions::assert_eq;

#[test]
fn id_token_claims_preserve_email_and_normalize_optional_plans() {
    for (payload, email, plan) in [
        (
            serde_json::json!({
                "email": "user@example.com",
                "https://api.openai.com/auth": { "chatgpt_plan_type": "pro" }
            }),
            Some("user@example.com"),
            Some("Pro"),
        ),
        (
            serde_json::json!({
                "email": "user@example.com",
                "https://api.openai.com/auth": { "chatgpt_plan_type": "go" }
            }),
            Some("user@example.com"),
            Some("Go"),
        ),
        (serde_json::json!({ "sub": "123" }), None, None),
    ] {
        let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let fake_jwt = format!(
            "{}.{}.{}",
            encode(br#"{"alg":"none","typ":"JWT"}"#),
            encode(&serde_json::to_vec(&payload).unwrap()),
            encode(b"sig"),
        );
        let info = parse_chatgpt_jwt_claims(&fake_jwt).expect("should parse");
        assert_eq!(info.email.as_deref(), email, "payload: {payload}");
        assert_eq!(
            info.get_chatgpt_plan_type().as_deref(),
            plan,
            "payload: {payload}"
        );
    }
}

#[test]
fn workspace_account_detection_matches_workspace_plans() {
    let workspace = IdTokenInfo {
        chatgpt_plan_type: Some(PlanType::Known(KnownPlan::Business)),
        ..IdTokenInfo::default()
    };
    assert!(workspace.is_workspace_account());

    let personal = IdTokenInfo {
        chatgpt_plan_type: Some(PlanType::Known(KnownPlan::Pro)),
        ..IdTokenInfo::default()
    };
    assert!(!personal.is_workspace_account());
}
