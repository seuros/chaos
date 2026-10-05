use super::*;

pub(super) async fn fixture()
-> anyhow::Result<(tempfile::TempDir, RuntimeDbHandle, PlanningActor, Workspace)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDbHandle::Sqlite(StateRuntime::init(home.path().into(), "test".into()).await?);
    let actor = PlanningActor {
        session: "session-one".into(),
        installation: "machine-one".into(),
    };
    let workspace = db.planning_create_workspace("ecosystem").await?;
    Ok((home, db, actor, workspace))
}

pub(super) fn request(
    plan: Option<&str>,
    revision: Option<i16>,
    change: PlanChange,
) -> PlanMutation {
    PlanMutation {
        request_id: Uuid::now_v7().to_string(),
        plan: plan.map(str::to_owned),
        expected_revision: revision,
        change,
    }
}

pub(super) async fn create(
    db: &RuntimeDbHandle,
    actor: &PlanningActor,
    workspace: &str,
) -> anyhow::Result<Plan> {
    Ok(db
        .planning_mutate(
            actor,
            &request(
                None,
                None,
                PlanChange::Create {
                    workspace: workspace.into(),
                    title: "durable plan".into(),
                    body: String::new(),
                },
            ),
        )
        .await?
        .plan)
}

pub(super) async fn add(
    db: &RuntimeDbHandle,
    actor: &PlanningActor,
    plan: &str,
    revision: i16,
) -> anyhow::Result<MutationResult> {
    db.planning_mutate(
        actor,
        &request(
            Some(plan),
            Some(revision),
            PlanChange::AddTask {
                title: "task".into(),
                body: String::new(),
                parent: None,
                position: 0,
            },
        ),
    )
    .await
}

pub(super) async fn content_roundtrip(
    db: &RuntimeDbHandle,
    actor: &PlanningActor,
    workspace: &str,
) -> anyhow::Result<()> {
    let plan = db
        .planning_mutate(
            actor,
            &request(
                None,
                None,
                PlanChange::Create {
                    workspace: workspace.into(),
                    title: "Content".into(),
                    body: "# Design\n\nKeep **Markdown**.".into(),
                },
            ),
        )
        .await?
        .plan;
    let task = db
        .planning_mutate(
            actor,
            &request(
                Some(&plan.id),
                Some(1),
                PlanChange::AddTask {
                    title: "Implement".into(),
                    body: "Accept Unicode: café 🦀".into(),
                    parent: None,
                    position: 0,
                },
            ),
        )
        .await?
        .task
        .expect("created task");
    for index in 0..53 {
        let mutation = request(
            Some(&plan.id),
            None,
            PlanChange::Clarify {
                task: Some("T1".into()),
                text: format!("Clarification {index}"),
            },
        );
        db.planning_mutate(actor, &mutation).await?;
        db.planning_mutate(actor, &mutation).await?;
    }
    db.planning_mutate(
        actor,
        &request(
            Some(&plan.id),
            None,
            PlanChange::Clarify {
                task: None,
                text: "Plan clarification".into(),
            },
        ),
    )
    .await?;
    let detail = db.planning_detail(&plan.id).await?;
    assert_eq!(detail.content.body, "# Design\n\nKeep **Markdown**.");
    assert_eq!(detail.content.clarifications.entries.len(), 1);
    let detail = db.planning_task(&plan.id, "T1", 0).await?;
    assert_eq!(detail.content.body, "Accept Unicode: café 🦀");
    assert_eq!(detail.task.revision, task.revision);
    assert_eq!(detail.content.clarifications.entries.len(), 50);
    let after = detail.content.clarifications.next_after.expect("next page");
    assert_eq!(
        db.planning_clarifications(&plan.id, Some("T1"), after)
            .await?
            .entries
            .len(),
        3
    );
    let snapshot = serde_json::to_value(db.planning_read(&plan.id, 0).await?)?;
    assert!(snapshot["plan"].get("body").is_none());
    assert!(snapshot["tasks"][0].get("body").is_none());
    assert_eq!(db.planning_history(&plan.id, 0).await?.entries.len(), 50);
    let job = db
        .planning_enqueue_consolidation(
            actor,
            &ConsolidationRequest {
                request_id: "all-pending".into(),
                plan: plan.id,
                task: Some("T1".into()),
                provider: "provider".into(),
                model: "model".into(),
            },
            None,
            "all-pending",
        )
        .await?;
    assert_eq!(job.input.clarifications.len(), 53);
    let last = job
        .input
        .clarifications
        .last()
        .expect("pending clarifications");
    assert_eq!(last.text, "Clarification 52");
    assert_eq!(job.input.through_seq, last.seq);
    db.planning_cancel_consolidation(&job.id).await?;
    Ok(())
}

#[tokio::test]
async fn bodies_and_clarifications_are_separate_from_summaries() -> anyhow::Result<()> {
    let (_home, db, actor, workspace) = fixture().await?;
    content_roundtrip(&db, &actor, &workspace.id).await
}

#[tokio::test]
async fn identities_revisions_history_and_retry() -> anyhow::Result<()> {
    let (_home, db, actor, workspace) = fixture().await?;
    let plan = create(&db, &actor, &workspace.id).await?;
    let mutation = request(
        Some(&plan.id),
        Some(1),
        PlanChange::AddTask {
            title: "first".into(),
            body: String::new(),
            parent: None,
            position: 10,
        },
    );
    let first = db.planning_mutate(&actor, &mutation).await?;
    assert_eq!(first, db.planning_mutate(&actor, &mutation).await?);
    let before = db.planning_history(&plan.id, 0).await?;
    assert!(add(&db, &actor, &plan.id, 1).await.is_err());
    assert_eq!(before, db.planning_history(&plan.id, 0).await?);
    let mut reused = mutation.clone();
    reused.change = PlanChange::AddTask {
        title: "different".into(),
        body: String::new(),
        parent: None,
        position: 0,
    };
    assert!(db.planning_mutate(&actor, &reused).await.is_err());
    db.planning_mutate(
        &actor,
        &request(
            Some(&plan.id),
            None,
            PlanChange::Transition {
                task: "T1".into(),
                task_revision: 1,
                event: TaskEvent::Cancel,
                reason: "not needed".into(),
            },
        ),
    )
    .await?;
    add(&db, &actor, &plan.id, 2).await?;
    let read = db.planning_read(&plan.id, 0).await?;
    assert_eq!(read.tasks[0].task.reference, "T2");
    assert_eq!(read.tasks[1].task.reference, "T1");
    assert_eq!(read.tasks[1].task.status, TaskStatus::Cancelled);
    assert_eq!(db.planning_history(&plan.id, 0).await?.entries.len(), 4);
    Ok(())
}

#[tokio::test]
async fn task_details_targets_and_bounded_tree_pages() -> anyhow::Result<()> {
    let (_home, db, actor, workspace) = fixture().await?;
    let plan = create(&db, &actor, &workspace.id).await?;
    for revision in 1..=52 {
        add(&db, &actor, &plan.id, revision).await?;
    }
    db.planning_mutate(
        &actor,
        &request(
            Some(&plan.id),
            Some(53),
            PlanChange::AddTask {
                title: "Nested".into(),
                body: String::new(),
                parent: Some("T1".into()),
                position: 0,
            },
        ),
    )
    .await?;
    let first = db.planning_read(&plan.id, 0).await?;
    assert_eq!(first.tasks.len(), 50);
    assert_eq!(first.next_offset, Some(50));
    assert_eq!(first.tasks[0].task.reference, "T1");
    assert_eq!(first.tasks[1].task.reference, "T53");
    assert_eq!(first.tasks[1].depth, 1);
    let second = db.planning_read(&plan.id, 50).await?;
    assert_eq!(second.tasks.len(), 3);
    assert_eq!(second.next_offset, None);
    assert_eq!(second.tasks[0].task.reference, "T50");
    let project = db.planning_register_project(&workspace.id, "chaos").await?;
    let driver = db
        .planning_register_project(&workspace.id, "skipper")
        .await?;
    db.planning_mutate(
        &actor,
        &request(
            Some(&plan.id),
            None,
            PlanChange::EditTask {
                task: "T1".into(),
                task_revision: 1,
                title: "Across repositories".into(),
                projects: vec![project.id.clone(), driver.id.clone()],
            },
        ),
    )
    .await?;
    let detail = db.planning_task(&plan.id, "T1", 0).await?;
    assert_eq!(detail.task.title, "Across repositories");
    assert_eq!(detail.task.revision, 2);
    assert_eq!(detail.projects.items.len(), 2);
    assert!(detail.projects.items.iter().any(|p| p.id == project.id));
    assert!(detail.projects.items.iter().any(|p| p.id == driver.id));
    let foreign_workspace = db.planning_create_workspace("other").await?;
    let foreign = db
        .planning_register_project(&foreign_workspace.id, "foreign")
        .await?;
    let history = db.planning_history(&plan.id, 0).await?;
    assert!(
        db.planning_mutate(
            &actor,
            &request(
                Some(&plan.id),
                None,
                PlanChange::EditTask {
                    task: "T1".into(),
                    task_revision: 2,
                    title: "Must roll back".into(),
                    projects: vec![foreign.id],
                }
            )
        )
        .await
        .is_err()
    );
    let detail = db.planning_task(&plan.id, "T1", 0).await?;
    assert_eq!(detail.task.revision, 2);
    assert_eq!(detail.task.title, "Across repositories");
    assert_eq!(detail.projects.items.len(), 2);
    assert_eq!(history.entries.len(), 50);
    assert_eq!(history.next_after, Some(50));
    assert_eq!(history, db.planning_history(&plan.id, 0).await?);
    let tail = db.planning_history(&plan.id, 50).await?;
    assert_eq!(tail.entries.len(), 5);
    assert_eq!(tail.next_after, None);
    assert!(db.planning_graph(&plan.id, None, 0).await.is_err());
    assert!(db.planning_validate_graph(&plan.id, 0).await.is_err());
    Ok(())
}

#[tokio::test]
async fn transitions_notes_and_terminal_plan() -> anyhow::Result<()> {
    let (_home, db, actor, workspace) = fixture().await?;
    let plan = create(&db, &actor, &workspace.id).await?;
    add(&db, &actor, &plan.id, 1).await?;
    for (event, reason) in [(TaskEvent::Complete, "invalid"), (TaskEvent::Start, " ")] {
        assert!(
            db.planning_mutate(
                &actor,
                &request(
                    Some(&plan.id),
                    None,
                    PlanChange::Transition {
                        task: "T1".into(),
                        task_revision: 1,
                        event,
                        reason: reason.into(),
                    }
                )
            )
            .await
            .is_err()
        );
    }
    for _ in 0..2 {
        db.planning_mutate(
            &actor,
            &request(
                Some(&plan.id),
                None,
                PlanChange::Note {
                    task: Some("T1".into()),
                    note: "progress".into(),
                },
            ),
        )
        .await?;
    }
    assert_eq!(
        db.planning_read(&plan.id, 0).await?.tasks[0].task.revision,
        1
    );
    assert!(
        db.planning_mutate(
            &actor,
            &request(
                Some(&plan.id),
                Some(2),
                PlanChange::Complete {
                    reason: "done".into()
                }
            )
        )
        .await
        .is_err()
    );
    db.planning_mutate(
        &actor,
        &request(
            Some(&plan.id),
            Some(2),
            PlanChange::Cancel {
                reason: "pause project".into(),
            },
        ),
    )
    .await?;
    assert_eq!(
        db.planning_read(&plan.id, 0).await?.tasks[0].task.status,
        TaskStatus::Pending
    );
    assert!(
        db.planning_mutate(
            &actor,
            &request(
                Some(&plan.id),
                None,
                PlanChange::Note {
                    task: None,
                    note: "late".into()
                }
            )
        )
        .await
        .is_err()
    );
    db.planning_mutate(
        &actor,
        &request(
            Some(&plan.id),
            Some(3),
            PlanChange::Reopen {
                reason: "resume".into(),
            },
        ),
    )
    .await?;
    db.planning_mutate(
        &actor,
        &request(
            Some(&plan.id),
            None,
            PlanChange::Transition {
                task: "T1".into(),
                task_revision: 1,
                event: TaskEvent::Start,
                reason: "working".into(),
            },
        ),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn containment_and_backend_boundaries() -> anyhow::Result<()> {
    let (_home, db, actor, workspace) = fixture().await?;
    let plan = create(&db, &actor, &workspace.id).await?;
    add(&db, &actor, &plan.id, 1).await?;
    db.planning_mutate(
        &actor,
        &request(
            Some(&plan.id),
            Some(2),
            PlanChange::AddTask {
                title: "child".into(),
                body: String::new(),
                parent: Some("T1".into()),
                position: 0,
            },
        ),
    )
    .await?;
    assert!(
        db.planning_mutate(
            &actor,
            &request(
                Some(&plan.id),
                Some(3),
                PlanChange::MoveTask {
                    task: "T1".into(),
                    task_revision: 1,
                    parent: Some("T2".into()),
                    position: 0,
                }
            )
        )
        .await
        .is_err()
    );
    assert!(
        db.planning_mutate(
            &actor,
            &request(
                Some(&plan.id),
                Some(3),
                PlanChange::Link {
                    parent: "T1".into(),
                    child: "T2".into()
                }
            )
        )
        .await
        .is_err()
    );
    let other = create(&db, &actor, &workspace.id).await?;
    add(&db, &actor, &other.id, 1).await?;
    let foreign = db.planning_read(&other.id, 0).await?.tasks.remove(0).task;
    assert!(
        db.planning_mutate(
            &actor,
            &request(
                Some(&plan.id),
                Some(3),
                PlanChange::MoveTask {
                    task: "T1".into(),
                    task_revision: 1,
                    parent: Some(foreign.id),
                    position: 0,
                }
            )
        )
        .await
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn concurrent_revision_winner_and_independent_tasks() -> anyhow::Result<()> {
    let (_home, db, actor, workspace) = fixture().await?;
    let plan = create(&db, &actor, &workspace.id).await?;
    add(&db, &actor, &plan.id, 1).await?;
    add(&db, &actor, &plan.id, 2).await?;
    let one = request(
        Some(&plan.id),
        None,
        PlanChange::Transition {
            task: "T1".into(),
            task_revision: 1,
            event: TaskEvent::Start,
            reason: "one".into(),
        },
    );
    let two = request(
        Some(&plan.id),
        None,
        PlanChange::Transition {
            task: "T1".into(),
            task_revision: 1,
            event: TaskEvent::Cancel,
            reason: "two".into(),
        },
    );
    let (a, b) = tokio::join!(
        db.planning_mutate(&actor, &one),
        db.planning_mutate(&actor, &two)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    db.planning_mutate(
        &actor,
        &request(
            Some(&plan.id),
            None,
            PlanChange::Transition {
                task: "T2".into(),
                task_revision: 1,
                event: TaskEvent::Start,
                reason: "independent".into(),
            },
        ),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn attachment_and_installation_scoped_checkouts() -> anyhow::Result<()> {
    let (home, db, actor, workspace) = fixture().await?;
    let plan = create(&db, &actor, &workspace.id).await?;
    let project = db.planning_register_project(&workspace.id, "chaos").await?;
    db.planning_bind("one", &project.id, home.path()).await?;
    assert!(db.planning_checkout("one", home.path()).await?.is_some());
    assert!(db.planning_checkout("two", home.path()).await?.is_none());
    db.planning_attach(&actor.session, Some(&plan.id)).await?;
    assert_eq!(
        db.planning_attachment(&actor.session).await?,
        Some(plan.id.clone())
    );
    assert_eq!(db.planning_attachment("child").await?, None);
    let change = request(
        Some(&plan.id),
        Some(1),
        PlanChange::AddTask {
            title: "new".into(),
            body: String::new(),
            parent: None,
            position: 0,
        },
    );
    db.planning_attach(&actor.session, None).await?;
    assert!(
        db.planning_mutate_attached(&actor, &change, &plan.id)
            .await
            .is_err()
    );
    assert!(db.planning_read(&plan.id, 0).await?.tasks.is_empty());
    Ok(())
}

#[test]
fn mutation_wire_shape_and_lifecycle() -> anyhow::Result<()> {
    let m: PlanMutation = serde_json::from_str(
        r#"{"request_id":"r","action":"create","workspace":"w","title":"t"}"#,
    )?;
    assert!(matches!(m.change, PlanChange::Create { .. }));
    assert_eq!(
        transition(TaskStatus::Pending, TaskEvent::Start)?,
        TaskStatus::InProgress
    );
    assert_eq!(
        transition(TaskStatus::InProgress, TaskEvent::Block)?,
        TaskStatus::Blocked
    );
    assert_eq!(
        transition(TaskStatus::Blocked, TaskEvent::Unblock)?,
        TaskStatus::Pending
    );
    assert_eq!(
        transition(TaskStatus::Cancelled, TaskEvent::Reopen)?,
        TaskStatus::Pending
    );
    assert!(transition(TaskStatus::Completed, TaskEvent::Start).is_err());
    Ok(())
}
