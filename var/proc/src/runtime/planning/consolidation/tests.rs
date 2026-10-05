use super::super::tests::{add, create, fixture, request};
use super::*;

fn consolidation_request(plan: &str, task: Option<&str>) -> ConsolidationRequest {
    ConsolidationRequest {
        request_id: Uuid::now_v7().to_string(),
        plan: plan.into(),
        task: task.map(str::to_owned),
        provider: "chosen-provider".into(),
        model: "chosen-model".into(),
    }
}

async fn pending_request(
    db: &RuntimeDbHandle,
    actor: &PlanningActor,
    workspace: &str,
) -> anyhow::Result<ConsolidationRequest> {
    let plan = create(db, actor, workspace).await?;
    db.planning_mutate(
        actor,
        &request(
            Some(&plan.id),
            None,
            PlanChange::Clarify {
                task: None,
                text: "Apply once".into(),
            },
        ),
    )
    .await?;
    Ok(consolidation_request(&plan.id, None))
}

async fn claim(db: &RuntimeDbHandle, job: &ConsolidationJob) -> anyhow::Result<String> {
    Ok(db
        .planning_claim_consolidation(&job.id, &job.actor, None)
        .await?
        .expect("claim queued consolidation"))
}

pub(in crate::runtime::planning) async fn roundtrip(
    db: &RuntimeDbHandle,
    actor: &PlanningActor,
    workspace: &str,
) -> anyhow::Result<()> {
    for task in [None, Some("T1")] {
        let plan = create(db, actor, workspace).await?;
        add(db, actor, &plan.id, 1).await?;
        let clarify = |text: &str| {
            request(
                Some(&plan.id),
                None,
                PlanChange::Clarify {
                    task: task.map(str::to_owned),
                    text: text.into(),
                },
            )
        };
        db.planning_mutate(actor, &clarify("Use Markdown")).await?;
        assert!(
            db.planning_pending_consolidations(&actor.session, "")
                .await?
                .is_empty()
        );
        let first = consolidation_request(&plan.id, task);
        let job = db
            .planning_enqueue_consolidation(actor, &first, None, "call-one")
            .await?;
        let replay = db
            .planning_enqueue_consolidation(actor, &first, None, "retry-call")
            .await?;
        assert_eq!(job.id, replay.id);
        assert_eq!(replay.origin_call_id, "call-one");
        let mut mismatched = first.clone();
        mismatched.model = "another-model".into();
        assert!(
            db.planning_enqueue_consolidation(actor, &mismatched, None, "bad-retry")
                .await
                .is_err()
        );

        let competing = consolidation_request(&plan.id, task);
        let other = db
            .planning_enqueue_consolidation(actor, &competing, None, "call-two")
            .await?;
        db.planning_mutate(actor, &clarify("Also add acceptance criteria"))
            .await?;
        assert_eq!(job.input.clarifications.len(), 1);
        assert_eq!(db.planning_consolidation(&job.id).await?.input, job.input);
        assert!(serde_json::to_value(&job)?.get("input").is_none());
        assert!(
            db.planning_claim_consolidation(&job.id, "someone-else", None)
                .await?
                .is_none()
        );
        let execution = claim(db, &job).await?;
        assert!(
            db.planning_claim_consolidation(&job.id, &actor.session, None)
                .await?
                .is_none()
        );
        let recovered = db
            .planning_claim_consolidation(&job.id, &actor.session, Some(&execution))
            .await?
            .expect("recovery");
        let stale_attempt = db
            .planning_publish_consolidation(&job.id, &execution, "must not publish")
            .await?;
        assert_eq!(stale_attempt.state, ConsolidationState::Running);
        let before = db.planning_read(&plan.id, 0).await?;
        let published = db
            .planning_publish_consolidation(&job.id, &recovered, "# Body\n\nUse Markdown.")
            .await?;
        assert_eq!(published.state, ConsolidationState::Completed);
        let history = db.planning_history(&plan.id, 0).await?;
        assert_eq!(
            db.planning_publish_consolidation(&job.id, &recovered, "must not replace")
                .await?
                .state,
            ConsolidationState::Completed
        );
        assert_eq!(db.planning_history(&plan.id, 0).await?, history);
        let content = match task {
            Some(task) => db.planning_task(&plan.id, task, 0).await?.content,
            None => db.planning_detail(&plan.id).await?.content,
        };
        assert_eq!(content.body, "# Body\n\nUse Markdown.");
        assert_eq!(content.body_revision, 2);
        assert_eq!(content.clarifications.entries.len(), 1);
        assert_eq!(
            content.clarifications.entries[0].text,
            "Also add acceptance criteria"
        );
        assert_eq!(
            db.planning_clarifications(&plan.id, task, 0)
                .await?
                .entries
                .len(),
            2
        );
        let after = db.planning_read(&plan.id, 0).await?;
        assert_eq!(before.plan.status, after.plan.status);
        assert_eq!(before.tasks[0].task.status, after.tasks[0].task.status);
        assert_eq!(before.tasks[0].task.position, after.tasks[0].task.position);
        assert_eq!(
            before.tasks[0].task.parent_id,
            after.tasks[0].task.parent_id
        );

        let competing_execution = claim(db, &other).await?;
        assert_eq!(
            db.planning_publish_consolidation(&other.id, &competing_execution, "outdated body")
                .await?
                .state,
            ConsolidationState::Stale
        );
        for cancel in [false, true] {
            let attempt = consolidation_request(&plan.id, task);
            let job = db
                .planning_enqueue_consolidation(actor, &attempt, None, "call-stop")
                .await?;
            let execution = claim(db, &job).await?;
            let expected = if cancel {
                db.planning_cancel_consolidation(&job.id).await?;
                ConsolidationState::Cancelled
            } else {
                db.planning_fail_consolidation(&job.id, &execution, "model unavailable")
                    .await?;
                ConsolidationState::Failed
            };
            assert_eq!(
                db.planning_publish_consolidation(&job.id, &execution, "must not publish")
                    .await?
                    .state,
                expected
            );
            assert_eq!(
                db.planning_enqueue_consolidation(actor, &attempt, None, "retry-stop")
                    .await?
                    .state,
                expected
            );
            assert!(
                db.planning_claim_consolidation(&job.id, &actor.session, Some(&execution))
                    .await?
                    .is_none()
            );
        }
        for close_plan in [false, true] {
            let attempt = consolidation_request(&plan.id, task);
            let job = db
                .planning_enqueue_consolidation(actor, &attempt, None, "stale")
                .await?;
            let execution = claim(db, &job).await?;
            let revision = db.planning_read(&plan.id, 0).await?.plan.revision;
            let change = if close_plan {
                PlanChange::Cancel {
                    reason: "No longer needed".into(),
                }
            } else if let Some(task) = task {
                PlanChange::EditTask {
                    task: task.into(),
                    task_revision: db.planning_task(&plan.id, task, 0).await?.task.revision,
                    title: "Revised intent".into(),
                    projects: Vec::new(),
                }
            } else {
                PlanChange::Edit {
                    title: "Revised intent".into(),
                }
            };
            db.planning_mutate(actor, &request(Some(&plan.id), Some(revision), change))
                .await?;
            assert_eq!(
                db.planning_publish_consolidation(&job.id, &execution, "must not publish")
                    .await?
                    .state,
                ConsolidationState::Stale
            );
        }
        assert!(
            db.planning_pending_consolidations(&actor.session, "")
                .await?
                .is_empty()
        );
    }
    Ok(())
}

#[tokio::test]
async fn consolidation_publication_fencing_failures_and_pending_clarifications()
-> anyhow::Result<()> {
    let (_home, db, actor, workspace) = fixture().await?;
    roundtrip(&db, &actor, &workspace.id).await
}

#[tokio::test]
async fn consolidation_survives_reopening_sqlite() -> anyhow::Result<()> {
    let (home, db, actor, workspace) = fixture().await?;
    let request = pending_request(&db, &actor, &workspace.id).await?;
    let job = db
        .planning_enqueue_consolidation(&actor, &request, None, "call")
        .await?;
    let execution = claim(&db, &job).await?;
    drop(db);
    let reopened =
        RuntimeDbHandle::Sqlite(StateRuntime::init(home.path().into(), "test".into()).await?);
    let recovered = reopened
        .planning_pending_consolidations(&actor.session, "")
        .await?;
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].request, request);
    assert_eq!(
        recovered[0].execution_id.as_deref(),
        Some(execution.as_str())
    );
    Ok(())
}

pub(in crate::runtime::planning) async fn publication_races(
    db: &RuntimeDbHandle,
    actor: &PlanningActor,
    workspace: &str,
) -> anyhow::Result<()> {
    for competing in [false, true] {
        let request = pending_request(db, actor, workspace).await?;
        let (a, replay) = tokio::join!(
            db.planning_enqueue_consolidation(actor, &request, None, "first"),
            db.planning_enqueue_consolidation(actor, &request, None, "retry")
        );
        let a = a?;
        assert_eq!(a.id, replay?.id);
        let a_execution = claim(db, &a).await?;
        let (b, b_execution) = if competing {
            let b = db
                .planning_enqueue_consolidation(
                    actor,
                    &consolidation_request(&request.plan, None),
                    None,
                    "second",
                )
                .await?;
            let execution = claim(db, &b).await?;
            (b, execution)
        } else {
            (a.clone(), a_execution.clone())
        };
        let (a, b) = tokio::join!(
            db.planning_publish_consolidation(&a.id, &a_execution, "First output"),
            db.planning_publish_consolidation(&b.id, &b_execution, "Second output")
        );
        let states = [a?.state, b?.state];
        if competing {
            assert!(states.contains(&ConsolidationState::Completed));
            assert!(states.contains(&ConsolidationState::Stale));
        } else {
            assert_eq!(states, [ConsolidationState::Completed; 2]);
        }
        let detail = db.planning_detail(&request.plan).await?;
        assert_eq!(detail.content.body_revision, 2);
        assert!(["First output", "Second output"].contains(&detail.content.body.as_str()));
        assert!(detail.content.clarifications.entries.is_empty());
        assert_eq!(
            db.planning_history(&request.plan, 0).await?.entries.len(),
            3
        );
    }
    Ok(())
}

#[tokio::test]
async fn consolidation_concurrent_publication_is_idempotent_in_sqlite() -> anyhow::Result<()> {
    let (_home, db, actor, workspace) = fixture().await?;
    publication_races(&db, &actor, &workspace.id).await
}
