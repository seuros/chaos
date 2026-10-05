use super::*;
use crate::chaos::{Session, TurnContext};
use crate::tools::context::ToolPayload;
use crate::turn_diff_tracker::TurnDiffTracker;
use chaos_proc::planning::Plan;
use chaos_proc::{RuntimeDbHandle, StateRuntime};
use std::sync::Arc;
use tokio::sync::Mutex;

async fn create_plan(db: &RuntimeDbHandle, workspace: &str, title: &str) -> anyhow::Result<Plan> {
    Ok(db
        .planning_mutate(
            &PlanningActor {
                session: "operator".into(),
                installation: "test".into(),
            },
            &PlanMutation {
                request_id: title.into(),
                plan: None,
                expected_revision: None,
                change: PlanChange::Create {
                    workspace: workspace.into(),
                    title: title.into(),
                    body: String::new(),
                },
            },
        )
        .await?
        .plan)
}

async fn switch_mode(
    session: &Session,
    turn: &Arc<TurnContext>,
    mode: &str,
) -> anyhow::Result<Arc<TurnContext>> {
    session
        .switch_mode(mode, turn)
        .await
        .map_err(anyhow::Error::msg)?;
    Ok(session.effective_turn_context(turn).await)
}

async fn fixture() -> anyhow::Result<(
    tempfile::TempDir,
    Arc<Session>,
    Arc<TurnContext>,
    async_channel::Receiver<chaos_ipc::protocol::Event>,
    Plan,
)> {
    let home = tempfile::tempdir()?;
    let (mut session, turn) = crate::chaos::make_session_and_context_with_home(home.path()).await;
    let db = RuntimeDbHandle::Sqlite(StateRuntime::init(home.path().into(), "test".into()).await?);
    let workspace = db.planning_create_workspace("tools").await?;
    let plan = create_plan(&db, &workspace.id, "Plan").await?;
    db.planning_attach(&session.conversation_id.to_string(), Some(&plan.id))
        .await?;
    session.services.runtime_db = Some(db);
    let (tx, rx) = async_channel::unbounded();
    session.tx_event = tx;
    let session = Arc::new(session);
    let turn = session.effective_turn_context(&Arc::new(turn)).await;
    Ok((home, session, turn, rx, plan))
}

async fn call(
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    name: &str,
    args: serde_json::Value,
) -> Result<FunctionToolOutput, FunctionCallError> {
    PlanHandler
        .handle(ToolInvocation {
            session: Arc::clone(session),
            turn: Arc::clone(turn),
            tracker: Arc::new(Mutex::new(TurnDiffTracker::default())),
            call_id: "test".into(),
            tool_name: name.into(),
            tool_namespace: None,
            payload: ToolPayload::Function {
                arguments: args.to_string(),
            },
        })
        .await
}

#[tokio::test]
async fn planning_replanning_round_trip_and_stale_calls() -> anyhow::Result<()> {
    use serde_json::json;
    let (_home, session, execution, rx, plan) = fixture().await?;
    let authoring = switch_mode(&session, &execution, "plan").await?;
    call(
        &session,
        &authoring,
        "plan",
        json!({
            "action": "change", "request_id": "add", "expected_revision": 1,
            "change": {"action":"add_task", "title":"Inspect", "position":0}
        }),
    )
    .await?;
    let execution = switch_mode(&session, &authoring, "default").await?;
    assert_eq!(
        execution.tools_config.attached_plan.as_deref(),
        Some(plan.id.as_str())
    );
    assert!(!execution.tools_config.planning_authoring);
    assert!(
        call(&session, &authoring, "plan", json!({"action":"read"}))
            .await
            .is_err()
    );
    while rx.try_recv().is_ok() {}

    // A stale revision cannot append history or emit a successful-looking UI update.
    let db = session
        .runtime_db()
        .ok_or_else(|| anyhow::anyhow!("database"))?;
    let before = db.planning_history(&plan.id, 0).await?;
    let error = call(&session, &execution, "plan_progress", json!({
        "action":"change", "request_id":"stale",
        "change":{"action":"transition","task":"T1","task_revision":9,"event":"start","reason":"inspect"}
    })).await.err().ok_or_else(|| anyhow::anyhow!("expected revision conflict"))?;
    assert!(error.to_string().contains("revision_conflict"));
    assert_eq!(db.planning_history(&plan.id, 0).await?, before);
    assert!(rx.try_recv().is_err());

    call(&session, &execution, "plan_progress", json!({
        "action":"change","request_id":"start",
        "change":{"action":"transition","task":"T1","task_revision":1,"event":"start","reason":"inspect"}
    })).await?;
    let event = rx.recv().await?;
    let EventMsg::PlanUpdate(update) = event.msg else {
        anyhow::bail!("expected committed view")
    };
    assert_eq!(
        update.tasks[0].status,
        chaos_proc::planning::TaskStatus::InProgress
    );
    assert_eq!(update.tasks[0].reference, "T1");

    db.planning_attach(&session.conversation_id.to_string(), None)
        .await?;
    assert!(
        call(
            &session,
            &execution,
            "plan_progress",
            json!({"action":"read"})
        )
        .await
        .is_err()
    );
    assert!(
        session
            .effective_turn_context(&execution)
            .await
            .tools_config
            .attached_plan
            .is_none()
    );
    assert!(rx.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn planning_storage_failure_does_not_publish_ui_update() -> anyhow::Result<()> {
    use serde_json::json;
    let (_home, session, execution, rx, plan) = fixture().await?;
    let authoring = switch_mode(&session, &execution, "plan").await?;
    while rx.try_recv().is_ok() {}
    // A nonexistent task is a failed database read inside the write transaction.
    assert!(
        call(
            &session,
            &authoring,
            "plan",
            json!({
                "action":"change", "request_id":"missing", "expected_revision":1,
                "change":{"action":"add_task", "title":"Child", "parent":"T999", "position":0}
            })
        )
        .await
        .is_err()
    );
    assert!(rx.try_recv().is_err());
    let db = session
        .runtime_db()
        .ok_or_else(|| anyhow::anyhow!("database"))?;
    let snapshot = db.planning_read(&plan.id, 0).await?;
    assert!(snapshot.tasks.is_empty());
    assert_eq!(snapshot.plan.revision, 1);
    assert_eq!(db.planning_history(&plan.id, 0).await?.entries.len(), 1);
    Ok(())
}

#[tokio::test]
async fn planning_authoring_rejects_rebound_implicit_plan() -> anyhow::Result<()> {
    use serde_json::json;
    let (_home, session, execution, rx, plan) = fixture().await?;
    let authoring = switch_mode(&session, &execution, "plan").await?;
    while rx.try_recv().is_ok() {}
    let db = session
        .runtime_db()
        .ok_or_else(|| anyhow::anyhow!("database"))?;
    let other = create_plan(&db, &plan.workspace_id, "Another plan").await?;
    db.planning_attach(&session.conversation_id.to_string(), Some(&other.id))
        .await?;
    assert!(
        call(
            &session,
            &authoring,
            "plan",
            json!({
                "action":"change", "request_id":"stale", "expected_revision":1,
                "change":{"action":"add_task", "title":"Wrong destination", "position":0}
            })
        )
        .await
        .is_err()
    );
    assert!(
        db.planning_detach(&session.conversation_id.to_string(), &plan.id)
            .await
            .is_err()
    );
    assert_eq!(
        db.planning_attachment(&session.conversation_id.to_string())
            .await?,
        Some(other.id.clone())
    );
    assert!(db.planning_read(&other.id, 0).await?.tasks.is_empty());
    assert!(rx.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn planning_bodies_and_intent_changes_are_gated_without_bloating_updates()
-> anyhow::Result<()> {
    use serde_json::json;
    let (_home, session, execution, rx, plan) = fixture().await?;
    let authoring = switch_mode(&session, &execution, "plan").await?;
    call(&session, &authoring, "plan", json!({
        "action":"change", "request_id":"body", "expected_revision":1,
        "change":{"action":"add_task", "title":"Inspect", "position":0, "body":"# Specification"}
    })).await?;
    call(
        &session,
        &authoring,
        "plan",
        json!({
            "action":"change", "request_id":"clarify",
            "change":{"action":"clarify", "task":"T1", "text":"Test restart recovery"}
        }),
    )
    .await?;
    while let Ok(event) = rx.try_recv() {
        if let EventMsg::PlanUpdate(update) = event.msg {
            let text = serde_json::to_string(&update)?;
            assert!(!text.contains("Specification"));
            assert!(!text.contains("restart recovery"));
        }
    }
    let execution = switch_mode(&session, &authoring, "default").await?;
    let detail: serde_json::Value = serde_json::from_str(
        &call(
            &session,
            &execution,
            "plan_progress",
            json!({"action":"task","task":"T1"}),
        )
        .await?
        .into_text(),
    )?;
    assert_eq!(detail["body"], "# Specification");
    assert_eq!(
        detail["clarifications"]["entries"][0]["text"],
        "Test restart recovery"
    );
    let db = session.runtime_db().expect("database");
    let before = db.planning_history(&plan.id, 0).await?;
    for args in [
        json!({
            "action":"change", "request_id":"denied",
            "change":{"action":"clarify", "task":"T1", "text":"Change intent"}
        }),
        json!({
            "action":"consolidate", "request_id":"denied",
            "provider":"provider", "model":"model"
        }),
    ] {
        assert!(
            call(&session, &execution, "plan_progress", args)
                .await
                .is_err()
        );
    }
    assert_eq!(db.planning_history(&plan.id, 0).await?, before);
    assert!(
        db.planning_pending_consolidations(&session.conversation_id.to_string(), "")
            .await?
            .is_empty()
    );
    Ok(())
}
