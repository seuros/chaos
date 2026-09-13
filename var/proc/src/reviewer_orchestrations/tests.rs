use super::*;
use crate::StateRuntime;
use serde_json::json;

async fn database() -> RuntimeDbHandle {
    let home = std::env::temp_dir().join(format!(
        "chaos-reviewer-orchestration-{}",
        uuid::Uuid::new_v4()
    ));
    RuntimeDbHandle::Sqlite(
        StateRuntime::init(home, "test".to_string())
            .await
            .expect("runtime database"),
    )
}

fn attempt(ordinal: i64, account: char, family: char) -> ReviewerAttemptCreateParams {
    ReviewerAttemptCreateParams {
        id: format!("attempt-{ordinal}"),
        ordinal,
        provider_id: format!("provider-{ordinal}"),
        model: format!("model-{ordinal}"),
        account_subject: format!("credential:v1:{}", account.to_string().repeat(64)),
        model_family_subject: format!("review-subject:v1:{}", family.to_string().repeat(64)),
        reviewer_attempt_subject: format!(
            "reviewer-attempt:v1:{}",
            (ordinal + 1).to_string().repeat(64)
        )
        .chars()
        .take("reviewer-attempt:v1:".len() + 64)
        .collect(),
        idempotency_key: format!("review-{ordinal}"),
        prompt: "Return strict review JSON".to_string(),
        mcp_server: "review-service".to_string(),
        mcp_tool: "submit_review".to_string(),
    }
}

fn run() -> ReviewRunCreateParams {
    ReviewRunCreateParams {
        id: "run-1".to_string(),
        review_run_subject: format!("review-run:v1:{}", "a".repeat(64)),
        attestation_subject: format!("review-run:v1:{}", "b".repeat(64)),
        owner_process_id: "owner-process-1".to_string(),
    }
}

#[tokio::test]
async fn persists_immutable_selection_and_declared_state_progression() {
    let db = database().await;
    let store = db.reviewer_orchestrations();
    let persisted = store
        .create_run(&run(), &[attempt(0, 'a', 'b'), attempt(1, 'b', 'c')])
        .await
        .unwrap();
    assert_eq!(persisted.owner_process_id, "owner-process-1");
    assert_eq!(
        store
            .get_run("run-1")
            .await
            .unwrap()
            .unwrap()
            .owner_process_id,
        "owner-process-1"
    );

    let first = &store.list_attempts("run-1").await.unwrap()[0];
    assert_eq!(first.state, ReviewAttemptState::Selection);
    assert!(
        store
            .transition_attempt(
                "attempt-0",
                ReviewAttemptState::Selection,
                ReviewAttemptState::Spawn,
                &ReviewAttemptTransitionData::default(),
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .transition_attempt(
                "attempt-0",
                ReviewAttemptState::Spawn,
                ReviewAttemptState::ModelExecution,
                &ReviewAttemptTransitionData {
                    process_id: Some("process-1".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .transition_attempt(
                "attempt-0",
                ReviewAttemptState::ModelExecution,
                ReviewAttemptState::OutputParse,
                &ReviewAttemptTransitionData {
                    raw_output: Some("{}".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .transition_attempt(
                "attempt-0",
                ReviewAttemptState::OutputParse,
                ReviewAttemptState::SubmissionUnknown,
                &ReviewAttemptTransitionData {
                    submission: Some(json!({"findings": []})),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
    );

    let reloaded = store.get_attempt("attempt-0").await.unwrap().unwrap();
    assert_eq!(reloaded.state, ReviewAttemptState::SubmissionUnknown);
    assert_eq!(reloaded.process_id.as_deref(), Some("process-1"));
    assert_eq!(reloaded.raw_output.as_deref(), Some("{}"));
    assert_eq!(reloaded.submission, Some(json!({"findings": []})));
}

#[tokio::test]
async fn rejects_duplicate_credential_or_family_before_database_write() {
    let db = database().await;
    let store = db.reviewer_orchestrations();
    let error = store
        .create_run(&run(), &[attempt(0, 'a', 'b'), attempt(1, 'a', 'c')])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("duplicate credential subject"));
    assert!(store.get_run("run-1").await.unwrap().is_none());

    let error = store
        .create_run(&run(), &[attempt(0, 'a', 'b'), attempt(1, 'c', 'b')])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("duplicate model family subject"));
    assert!(store.get_run("run-1").await.unwrap().is_none());
}

#[tokio::test]
async fn rejects_empty_owner_process_id_before_database_write() {
    let db = database().await;
    let store = db.reviewer_orchestrations();
    let mut invalid = run();
    invalid.owner_process_id = "  ".to_string();

    let error = store
        .create_run(&invalid, &[attempt(0, 'a', 'b')])
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("review run owner process id cannot be empty")
    );
    assert!(store.get_run("run-1").await.unwrap().is_none());
}

#[tokio::test]
async fn database_trigger_rejects_binding_mutation() {
    let db = database().await;
    let store = db.reviewer_orchestrations();
    store
        .create_run(&run(), &[attempt(0, 'a', 'b')])
        .await
        .unwrap();
    let pool = db.sqlite_pool_cloned().unwrap();
    let error = sqlx::query("UPDATE reviewer_attempts SET model = ? WHERE id = ?")
        .bind("forged")
        .bind("attempt-0")
        .execute(&pool)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("immutable"));

    let error = sqlx::query("UPDATE review_runs SET owner_process_id = ? WHERE id = ?")
        .bind("another-owner")
        .bind("run-1")
        .execute(&pool)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("immutable"));
}
