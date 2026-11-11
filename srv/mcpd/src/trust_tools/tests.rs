use super::*;
use mcp_host::{AUTHENTICATED_PRINCIPAL_KEY, AuthenticatedPrincipal, HTTP_BEARER_PRINCIPAL_SOURCE};
use pretty_assertions::assert_eq;

fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    chaos_kern::test_support::bootstrap_sqlite(home.path()).unwrap();
    home
}

fn session_as(principal: Option<AuthenticatedPrincipal>) -> Session {
    let session = Session::new();
    if let Some(principal) = principal {
        session.set_state(AUTHENTICATED_PRINCIPAL_KEY, principal.into());
    }
    session
}

fn operator() -> Option<AuthenticatedPrincipal> {
    Some(AuthenticatedPrincipal::new(
        HTTP_BEARER_PRINCIPAL_SOURCE,
        OPERATOR_PRINCIPAL,
    ))
}

type Handler = for<'a> fn(&'a TrustOperator, ExecutionContext<'a>) -> Fut<'a>;

async fn call(
    server: &TrustOperator,
    session: &Session,
    handler: Handler,
    params: Value,
) -> Result<Value, String> {
    let (tx, _rx) = NotificationSender::bounded(16);
    let logger = McpLogger::new(tx, "trust-test");
    let ctx = ExecutionContext::new(params, session, &logger);
    match handler(server, ctx).await {
        Ok(output) => Ok(output.into_payload()),
        Err(err) => Err(err.to_string()),
    }
}

fn project_key(dir: &Path) -> String {
    trust_admin::normalize_project_path(dir)
        .unwrap()
        .to_string_lossy()
        .to_string()
}

#[tokio::test]
async fn every_trust_tool_refuses_without_the_operator_principal() {
    let home = home();
    let server = TrustOperator {
        home: home.path().to_path_buf(),
    };
    let project = tempfile::tempdir().unwrap();
    let callers = [
        None,
        // The right subject from another source is not the operator bearer.
        Some(AuthenticatedPrincipal::new("api-key", OPERATOR_PRINCIPAL)),
        Some(AuthenticatedPrincipal::new(
            HTTP_BEARER_PRINCIPAL_SOURCE,
            "someone-else",
        )),
    ];
    let tools: [(Handler, Value); 6] = [
        (list_handler, json!({})),
        (
            show_handler,
            json!({"project_path": project.path().to_string_lossy()}),
        ),
        (
            set_handler,
            json!({"project_path": project.path().to_string_lossy(), "decision": "trusted", "expect": "none"}),
        ),
        (export_handler, json!({})),
        (plan_handler, json!({"manifest": {}, "digest": "x"})),
        (apply_handler, json!({"manifest": {}, "digest": "x"})),
    ];
    for caller in callers {
        let session = session_as(caller.clone());
        for (handler, params) in &tools {
            let err = call(&server, &session, *handler, params.clone())
                .await
                .unwrap_err();
            assert!(
                err.contains("requires the operator bearer"),
                "{caller:?}: {err}"
            );
        }
    }
    let listing = trust_admin::list(home.path()).await.unwrap();
    assert!(listing.entries.is_empty());
}

#[tokio::test]
async fn operator_sets_exports_plans_and_applies() {
    let home = home();
    let server = TrustOperator {
        home: home.path().to_path_buf(),
    };
    let session = session_as(operator());
    let project = tempfile::tempdir().unwrap();
    let key = project_key(project.path());

    let receipt = call(
        &server,
        &session,
        set_handler,
        json!({"project_path": key, "decision": "trusted", "expect": "none"}),
    )
    .await
    .unwrap();
    assert_eq!(receipt["changes"][0]["after"], "trusted");
    let actor = receipt["actor"].as_str().unwrap();
    assert!(
        actor.starts_with("chaos-operator (serve --mcp session "),
        "{actor}"
    );

    let shown = call(
        &server,
        &session,
        show_handler,
        json!({"project_path": key}),
    )
    .await
    .unwrap();
    assert_eq!(shown["key"], key.as_str());
    assert_eq!(shown["stored_at_key"], "trusted");

    let exported = call(&server, &session, export_handler, json!({}))
        .await
        .unwrap();
    let digest = exported["digest"].as_str().unwrap().to_string();
    let plan = call(
        &server,
        &session,
        plan_handler,
        json!({"manifest": exported["manifest"], "digest": digest}),
    )
    .await
    .unwrap();
    assert_eq!(plan["changes"], json!([]));
    assert_eq!(plan["held"], json!([]));
    assert_eq!(plan["unchanged"], json!([key]));

    // A digest that is not the reviewed one is refused.
    let err = call(
        &server,
        &session,
        apply_handler,
        json!({"manifest": exported["manifest"], "digest": "v1:not-it"}),
    )
    .await
    .unwrap_err();
    assert!(err.contains("digest mismatch"), "{err}");
}

#[tokio::test]
async fn apply_refuses_held_grants_unless_partial_is_explicit() {
    let home = home();
    let server = TrustOperator {
        home: home.path().to_path_buf(),
    };
    let session = session_as(operator());
    let present = tempfile::tempdir().unwrap();
    let missing = present.path().join("gone");
    let manifest = trust_admin::Manifest {
        format: trust_admin::MANIFEST_FORMAT.to_string(),
        installation_id: chaos_kern::user_settings::installation_id(home.path()).unwrap(),
        entries: vec![
            trust_admin::ManifestEntry {
                project_path: project_key(present.path()),
                decision: Decision::Trusted,
                expect: None,
            },
            trust_admin::ManifestEntry {
                project_path: missing.to_string_lossy().to_string(),
                decision: Decision::Trusted,
                expect: None,
            },
        ],
    };
    let digest = manifest.digest().unwrap();
    let manifest = serde_json::to_value(&manifest).unwrap();

    let plan = call(
        &server,
        &session,
        plan_handler,
        json!({"manifest": manifest, "digest": digest}),
    )
    .await
    .unwrap();
    assert_eq!(
        plan["held"][0]["project_path"],
        missing.to_string_lossy().as_ref()
    );

    let err = call(
        &server,
        &session,
        apply_handler,
        json!({"manifest": manifest, "digest": digest}),
    )
    .await
    .unwrap_err();
    assert!(err.contains("would be held"), "{err}");
    assert!(
        trust_admin::list(home.path())
            .await
            .unwrap()
            .entries
            .is_empty(),
        "a refused apply writes nothing"
    );

    let receipt = call(
        &server,
        &session,
        apply_handler,
        json!({"manifest": manifest, "digest": digest, "allow_held": true}),
    )
    .await
    .unwrap();
    assert_eq!(receipt["held"].as_array().unwrap().len(), 1);
    assert_eq!(receipt["changes"].as_array().unwrap().len(), 1);
}
