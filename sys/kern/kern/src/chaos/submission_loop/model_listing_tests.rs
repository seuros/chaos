use super::*;
use crate::models_manager::manager::ModelsManager;
use chaos_ipc::openai_models::{ModelInfo, ModelVisibility, ModelsResponse};
use std::time::Duration;

#[tokio::test]
async fn list_models_submission_returns_a_correlated_catalog_without_starting_a_turn() {
    let model = crate::test_support::test_remote_model("catalog-model", ModelVisibility::List, 1);
    assert_catalog_submission(vec![model]).await;
}

#[tokio::test]
async fn list_models_submission_returns_an_empty_catalog_when_none_are_available() {
    assert_catalog_submission(Vec::new()).await;
}

async fn assert_catalog_submission(models: Vec<ModelInfo>) {
    let model_count = models.len();
    let (mut session, context, events) = crate::chaos::make_session_and_context_with_rx().await;
    let manager = ModelsManager::new(
        context.config.chaos_home.clone(),
        session.services.auth_manager.clone(),
        Some(ModelsResponse { models }),
        Default::default(),
    );
    Arc::get_mut(&mut session)
        .expect("test session has one owner")
        .services
        .models_manager = Arc::new(manager);
    let expected = session.session_controls().await.model_catalog.into_models();
    assert_eq!(expected.len(), model_count);

    let (submissions, receiver) = async_channel::unbounded();
    let task = tokio::spawn(submission_loop(
        session.clone(),
        context.config.clone(),
        receiver,
    ));
    submissions
        .send(Submission {
            id: "catalog-request".into(),
            op: Op::ListModels,
            trace: None,
        })
        .await
        .expect("submit model listing");

    let event = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.expect("session event");
            assert!(
                !matches!(event.msg, EventMsg::TurnStarted(_)),
                "model listing must not start a turn"
            );
            if let EventMsg::ListModelsResponse(response) = &event.msg {
                assert_eq!(response.models, expected);
                break event;
            }
        }
    })
    .await
    .expect("model-list response must not be silently ignored");

    assert_eq!(event.id, "catalog-request");
    assert!(session.active_turn.lock().await.is_none());
    task.abort();
    assert!(
        task.await
            .expect_err("submission loop aborted")
            .is_cancelled()
    );
}
