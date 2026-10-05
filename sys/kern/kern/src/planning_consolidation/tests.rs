use super::*;
use crate::models_manager::CollaborationModesConfig;
use crate::models_manager::manager::ModelsManager;
use chaos_ipc::background_tasks::{TaskJournalEvent, WakePolicy};
use chaos_ipc::openai_models::ModelsResponse;
use chaos_ipc::protocol::RolloutItem;
use chaos_proc::planning::{PlanChange, PlanMutation};
use chaos_proc::{RuntimeDbHandle, StateRuntime};
use core_test_support::responses::{ev_assistant_message, ev_completed, mount_sse_once, sse};
use serde_json::json;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Fixture {
    _home: tempfile::TempDir,
    server: MockServer,
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    db: RuntimeDbHandle,
    actor: PlanningActor,
    request: ConsolidationRequest,
}

impl Fixture {
    async fn new() -> anyhow::Result<Self> {
        let home = tempfile::tempdir()?;
        let server = MockServer::start().await;
        let db =
            RuntimeDbHandle::Sqlite(StateRuntime::init(home.path().into(), "test".into()).await?);
        db.commit_settings(
            db.settings_snapshot().await?.revision,
            &json!({
                "model_providers": {"chosen": {
                    "name": "Consolidation test",
                    "base_url": server.uri(),
                    "wire_api": "responses",
                    "request_max_retries": 0
                }}
            }),
            None,
        )
        .await?;
        let (mut session, turn) =
            crate::chaos::make_session_and_context_with_home(home.path()).await;
        let config = &turn.config;
        let provider = config.model_providers["chosen"].clone();
        let mut info =
            ModelsManager::construct_model_info_offline_for_tests("chosen-model", config);
        info.supported_in_api = true;
        info.native_server_side_tools = vec!["web_search".into()];
        session.services.models_manager = Arc::new(ModelsManager::new_with_provider_binding(
            home.path().into(),
            session.services.auth_manager.clone(),
            Some(ModelsResponse { models: vec![info] }),
            CollaborationModesConfig::default(),
            "chosen".into(),
            provider,
        ));
        session.services.runtime_db = Some(db.clone());
        let actor = PlanningActor {
            session: session.conversation_id.to_string(),
            installation: "test".into(),
        };
        let workspace = db.planning_create_workspace("test").await?;
        let plan = db
            .planning_mutate(
                &actor,
                &PlanMutation {
                    request_id: "create".into(),
                    plan: None,
                    expected_revision: None,
                    change: PlanChange::Create {
                        workspace: workspace.id,
                        title: "Durable specification".into(),
                        body: "# Original\n\nPreserve me.".into(),
                    },
                },
            )
            .await?
            .plan;
        let fixture = Self {
            _home: home,
            server,
            session: Arc::new(session),
            turn: Arc::new(turn),
            db,
            actor,
            request: ConsolidationRequest {
                request_id: "consolidate".into(),
                plan: plan.id,
                task: None,
                provider: "chosen".into(),
                model: "chosen-model".into(),
            },
        };
        fixture.clarify("first", "Use Markdown").await?;
        Ok(fixture)
    }

    async fn clarify(&self, id: &str, text: &str) -> anyhow::Result<()> {
        self.db
            .planning_mutate(
                &self.actor,
                &PlanMutation {
                    request_id: id.into(),
                    plan: Some(self.request.plan.clone()),
                    expected_revision: None,
                    change: PlanChange::Clarify {
                        task: None,
                        text: text.into(),
                    },
                },
            )
            .await?;
        Ok(())
    }

    async fn start(&self) -> anyhow::Result<ConsolidationJob> {
        self.session
            .request_consolidation(
                self.turn.clone(),
                &self.actor,
                &self.request,
                None,
                "rewrite",
            )
            .await
    }

    async fn finished(&self, job: &ConsolidationJob) -> anyhow::Result<ConsolidationJob> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let current = self.db.planning_consolidation(&job.id).await?;
                let task = self
                    .session
                    .services
                    .internal_task_store
                    .get(&TaskRegistry::submission_id(&job.origin_call_id))
                    .await;
                if current.state.is_terminal() && task.is_some_and(|t| t.state.is_terminal()) {
                    return Ok(current);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?
    }
}

#[tokio::test]
async fn consolidation_uses_selected_model_without_tools_or_transcript() -> anyhow::Result<()> {
    let f = Fixture::new().await?;
    f.session
        .record_into_history(
            &[ResponseInputItem::Message {
                role: "user".into(),
                content: vec![ContentItem::InputText {
                    text: "Unrelated private conversation".into(),
                }],
            }
            .into()],
            &f.turn,
        )
        .await;
    let mock = mount_sse_once(
        &f.server,
        sse(vec![
            ev_assistant_message("body", "# Consolidated\n\nPreserve me. Use Markdown."),
            ev_completed("rewrite"),
        ]),
    )
    .await;
    let job = f.start().await?;
    let result = f.finished(&job).await?;
    assert_eq!(
        result.state,
        ConsolidationState::Completed,
        "{:?}",
        result.error
    );
    let request = mock.single_request().body_json();
    assert_eq!(request["model"], "chosen-model");
    assert_eq!(request["tools"], json!([]));
    let input = request["input"].as_array().expect("input");
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["role"], "user");
    let supplied: serde_json::Value =
        serde_json::from_str(input[0]["content"][0]["text"].as_str().expect("text"))?;
    assert_eq!(
        supplied,
        json!({
            "title": "Durable specification",
            "body": "# Original\n\nPreserve me.",
            "clarifications": ["Use Markdown"],
        })
    );
    let detail = f.db.planning_detail(&f.request.plan).await?;
    assert_eq!(
        detail.content.body,
        "# Consolidated\n\nPreserve me. Use Markdown."
    );
    assert!(detail.content.clarifications.entries.is_empty());

    let repeated = f.start().await?;
    assert_eq!(repeated.id, job.id);
    assert_eq!(repeated.state, ConsolidationState::Completed);
    assert_eq!(mock.requests().len(), 1);
    Ok(())
}

#[tokio::test]
async fn consolidation_cancellation_preserves_body_and_pending_changes() -> anyhow::Result<()> {
    let f = Fixture::new().await?;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![
                    ev_assistant_message("body", "Must not publish"),
                    ev_completed("rewrite"),
                ]))
                .set_delay(Duration::from_secs(30)),
        )
        .mount(&f.server)
        .await;
    let job = f.start().await?;
    f.clarify("later", "Keep this pending").await?;
    f.session
        .cancel_internal_task(&TaskRegistry::submission_id("rewrite"))
        .await?;
    assert_eq!(f.finished(&job).await?.state, ConsolidationState::Cancelled);
    let detail = f.db.planning_detail(&f.request.plan).await?;
    assert_eq!(detail.content.body, "# Original\n\nPreserve me.");
    assert_eq!(detail.content.clarifications.entries.len(), 2);
    Ok(())
}

#[tokio::test]
async fn consolidation_rejects_unknown_binding_without_enqueuing() -> anyhow::Result<()> {
    let mut f = Fixture::new().await?;
    for (provider, model) in [("unknown", "chosen-model"), ("chosen", "unknown")] {
        f.request.provider = provider.into();
        f.request.model = model.into();
        assert!(f.start().await.is_err());
    }
    assert!(
        f.db.planning_pending_consolidations(&f.actor.session, "")
            .await?
            .is_empty()
    );
    assert!(
        f.session
            .services
            .internal_task_store
            .list()
            .await
            .is_empty()
    );
    assert!(
        f.server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn consolidation_failure_requires_an_explicit_new_request() -> anyhow::Result<()> {
    let f = Fixture::new().await?;
    let mock = mount_sse_once(&f.server, sse(vec![ev_completed("empty")])).await;
    let job = f.start().await?;
    let failed = f.finished(&job).await?;
    assert_eq!(failed.state, ConsolidationState::Failed);
    assert!(failed.error.expect("error").contains("empty body"));
    let retried = f.start().await?;
    assert_eq!(retried.state, ConsolidationState::Failed);
    f.session.recover_consolidations().await?;
    assert_eq!(mock.requests().len(), 1);
    let detail = f.db.planning_detail(&f.request.plan).await?;
    assert_eq!(detail.content.body_revision, 1);
    assert_eq!(detail.content.clarifications.entries.len(), 1);
    Ok(())
}

#[tokio::test]
async fn consolidation_recovery_reclaims_queued_and_running_jobs() -> anyhow::Result<()> {
    for running in [false, true] {
        let f = Fixture::new().await?;
        let mock = mount_sse_once(
            &f.server,
            sse(vec![
                ev_assistant_message("body", "# Recovered"),
                ev_completed("recovery"),
            ]),
        )
        .await;
        let job =
            f.db.planning_enqueue_consolidation(&f.actor, &f.request, None, "recover")
                .await?;
        if running {
            f.db.planning_claim_consolidation(&job.id, &f.actor.session, None)
                .await?
                .expect("old execution");
            f.session.register_consolidation(&job, false).await?;
        } else {
            f.session.begin_background_submission("recover").await?;
        }
        let registry = &f.session.services.internal_task_store;
        let mut saved: Vec<_> = registry
            .list()
            .await
            .into_iter()
            .map(|task| {
                RolloutItem::BackgroundTask(TaskJournalEvent::Upsert {
                    task: Box::new(task),
                })
            })
            .collect();
        saved.push(RolloutItem::BackgroundTask(TaskJournalEvent::WakePolicy {
            policy: WakePolicy::Enabled,
        }));
        f.session.recover_background_tasks(&saved).await;
        assert_eq!(f.finished(&job).await?.state, ConsolidationState::Completed);
        assert_eq!(
            f.db.planning_detail(&f.request.plan).await?.content.body,
            "# Recovered"
        );
        assert_eq!(mock.requests().len(), 1);
    }
    Ok(())
}

#[tokio::test]
async fn consolidation_recovery_reconciles_publication_without_rewriting() -> anyhow::Result<()> {
    let f = Fixture::new().await?;
    let job =
        f.db.planning_enqueue_consolidation(&f.actor, &f.request, None, "published")
            .await?;
    f.session.register_consolidation(&job, true).await?;
    let execution =
        f.db.planning_claim_consolidation(&job.id, &f.actor.session, None)
            .await?
            .expect("claim");
    f.db.planning_publish_consolidation(&job.id, &execution, "# Already published")
        .await?;
    let history = f.db.planning_history(&f.request.plan, 0).await?;
    f.session.recover_consolidations().await?;
    assert_eq!(f.finished(&job).await?.state, ConsolidationState::Completed);
    assert!(
        f.server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
    assert_eq!(f.db.planning_history(&f.request.plan, 0).await?, history);
    Ok(())
}
