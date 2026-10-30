use super::*;
use chaos_ipc::protocol::{Op, Submission};
use rama::futures::FutureExt;
use serde_json::json;

async fn runner(
    session: &Arc<Session>,
) -> (async_channel::Sender<Submission>, AbortOnDropHandle<()>) {
    let (tx, rx) = async_channel::bounded(8);
    let task = AbortOnDropHandle::new(tokio::spawn(
        crate::chaos::submission_loop::submission_loop(
            session.clone(),
            session.get_config().await,
            rx,
        ),
    ));
    (tx, task)
}

async fn shutdown(tx: async_channel::Sender<Submission>, task: AbortOnDropHandle<()>) {
    tx.send(Submission {
        id: "shutdown".into(),
        op: Op::Shutdown,
        trace: None,
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}

async fn idle(session: &Arc<Session>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while session.active_turn.lock().await.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn idle_admission_registers_real_policy_context_without_model_turn() -> anyhow::Result<()> {
    let (session, base) = make_session_and_context().await;
    let session = Arc::new(session);
    session.services.tool_group_catalog.set_groups_enabled(
        &session.services.tool_group_state,
        [crate::tools::groups::WORKFLOWS],
        true,
    )?;
    let (tx, task) = runner(&session).await;
    let scope = session.services.workflow_control.open("owner").await?;
    let turn = &scope.0.invocation.turn;
    assert!(uuid::Uuid::parse_str(&turn.sub_id).is_ok());
    assert_ne!(turn.sub_id, base.sub_id);
    let permissions = session
        .permission_actor
        .snapshot(turn.sub_id.clone())
        .await?;
    assert_eq!(permissions.approval_policy, base.approval_policy.value());
    assert_eq!(permissions.effective_vfs_policy(), base.vfs_policy);
    assert_eq!(turn.mode_id, base.mode_id);
    assert_eq!(turn.model_info.slug, base.model_info.slug);
    assert!(session.clone_history().await.raw_items().is_empty());
    assert!(session.total_token_usage().await.is_none());
    let owner = McpSession::with_id("owner");
    let server = Server::builder("test", "1").build();
    let logger = server.logger_for_session("owner");
    for _ in 0..2 {
        let result = scope
            .run(ExecutionContext::new(
                json!({"tree":{
                    "type":"condition","id":"check","pointer":"/input","predicate":{"op":"exists"}
                }}),
                &owner,
                &logger,
            ))
            .await?
            .into_payload();
        assert_eq!(result["status"]["outcome"], "success");
        let retained = scope
            .status(ExecutionContext::new(
                json!({"run_id":result["run_id"]}),
                &owner,
                &logger,
            ))?
            .into_payload();
        assert_eq!(retained["run_id"], result["run_id"]);
    }
    let turn_id = turn.sub_id.clone();
    scope.close();
    idle(&session).await;
    assert!(session.permission_actor.snapshot(turn_id).await.is_err());
    assert!(session.total_token_usage().await.is_none());
    shutdown(tx, task).await;
    Ok(())
}

#[tokio::test]
async fn busy_admission_never_interrupts_existing_work_or_inherits_its_turn_grants() {
    let (session, cancellation, _) = active_session().await;
    let original = session
        .active_turn
        .lock()
        .await
        .as_ref()
        .unwrap()
        .tasks
        .first()
        .unwrap()
        .0
        .clone();
    let (tx, task) = runner(&session).await;
    let error = session
        .services
        .workflow_control
        .open("owner")
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("idle process"), "{error}");
    assert!(!cancellation.is_cancelled());
    assert_eq!(
        session
            .active_turn
            .lock()
            .await
            .as_ref()
            .unwrap()
            .tasks
            .first()
            .unwrap()
            .0,
        &original
    );
    shutdown(tx, task).await;
}

#[tokio::test]
async fn abandoned_queued_admission_creates_no_turn() {
    let (session, _) = make_session_and_context().await;
    let session = Arc::new(session);
    let control = &session.services.workflow_control;
    let mut open = Box::pin(control.open("owner"));
    assert!(open.as_mut().now_or_never().is_none());
    drop(open);
    control.recv().await.unwrap().admit(&session, false).await;
    assert!(session.active_turn.lock().await.is_none());
}

#[tokio::test]
async fn admission_queue_is_bounded_and_shutdown_releases_waiters() {
    let (session, _) = make_session_and_context().await;
    let session = Arc::new(session);
    let control = &session.services.workflow_control;
    let mut pending = (0..8)
        .map(|_| Box::pin(control.open("owner")))
        .collect::<Vec<_>>();
    for future in &mut pending {
        assert!(future.as_mut().now_or_never().is_none());
    }
    assert!(
        control
            .open("owner")
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("queue")
    );
    control.shutdown();
    for future in pending {
        assert!(future.await.is_err());
    }
    assert!(session.active_turn.lock().await.is_none());
}

#[tokio::test]
async fn journal_pause_refuses_admission_and_cannot_start_automatic_model_work() {
    let (session, _) = make_session_and_context().await;
    let session = Arc::new(session);
    let control = &session.services.workflow_control;
    let mut open = Box::pin(control.open("owner"));
    assert!(open.as_mut().now_or_never().is_none());
    control.recv().await.unwrap().admit(&session, true).await;
    let error = open.await.err().unwrap();
    assert!(error.to_string().contains("journal recovery"));
    assert!(session.active_turn.lock().await.is_none());
}

#[tokio::test]
async fn shutdown_revokes_idle_operation_and_steering_is_not_silently_consumed()
-> anyhow::Result<()> {
    let (session, _) = make_session_and_context().await;
    let session = Arc::new(session);
    let (tx, task) = runner(&session).await;
    let scope = session.services.workflow_control.open("owner").await?;
    let input = vec![chaos_ipc::user_input::UserInput::Text {
        text: "explicit owner input".into(),
        text_elements: Vec::new(),
    }];
    assert!(matches!(
        session.steer_input(input, None).await,
        Err(crate::chaos::SteerInputError::NoActiveTurn(_))
    ));
    assert!(session.get_pending_input().await.is_empty());
    shutdown(tx, task).await;
    assert!(scope.0.lifetime.is_cancelled());
    assert!(
        session
            .services
            .workflow_control
            .open("owner")
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn model_free_operation_uses_real_approval_mailbox_and_close_aborts_waiters()
-> anyhow::Result<()> {
    use chaos_ipc::protocol::{EventMsg, ReviewDecision};
    let (session, _, events) = crate::chaos::make_session_and_context_with_rx().await;
    let (tx, task) = runner(&session).await;
    let scope = session.services.workflow_control.open("owner").await?;
    let turn = scope.0.invocation.turn.clone();
    let approval = |call_id: String| {
        let session = session.clone();
        let turn = turn.clone();
        async move {
            session
                .request_command_approval(
                    &turn,
                    call_id,
                    None,
                    vec!["not-executed".into()],
                    turn.cwd.clone(),
                    None,
                    None,
                    None,
                    None,
                    Some(vec![ReviewDecision::Denied]),
                )
                .await
        }
    };
    let mut pending = Box::pin(approval("first".into()));
    assert!(pending.as_mut().now_or_never().is_none());
    let event = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let EventMsg::ExecApprovalRequest(request) = events.recv().await.unwrap().msg {
                break request;
            }
        }
    })
    .await?;
    assert_eq!(event.turn_id, turn.sub_id);
    assert_eq!(event.call_id, "first");
    tx.send(Submission {
        id: "decision".into(),
        op: Op::ExecApproval {
            id: event.call_id,
            turn_id: Some(event.turn_id),
            decision: ReviewDecision::Denied,
        },
        trace: None,
    })
    .await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), pending).await?,
        ReviewDecision::Denied
    );

    let mut abandoned = Box::pin(approval("second".into()));
    assert!(abandoned.as_mut().now_or_never().is_none());
    scope.close();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), abandoned).await?,
        ReviewDecision::Abort
    );
    idle(&session).await;
    shutdown(tx, task).await;
    Ok(())
}
