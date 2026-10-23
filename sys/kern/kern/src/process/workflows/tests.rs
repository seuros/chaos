use super::*;
use crate::chaos::make_session_and_context;
use crate::state::{ActiveTurn, RunningTask, TaskKind};
use crate::tasks::RegularTask;
use mcp_host::server::Server;
use mcp_host::server::session::Session as McpSession;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tokio_util::task::AbortOnDropHandle;

mod admission;
mod owner;
mod tasks;

/// Install task metadata without starting a native turn or calling a provider.
async fn active_session() -> (Arc<Session>, CancellationToken, Arc<Notify>) {
    let (session, turn) = make_session_and_context().await;
    let session = Arc::new(session);
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let done = Arc::new(Notify::new());
    let mut active = ActiveTurn::default();
    active.add_task(RunningTask {
        done: done.clone(),
        kind: TaskKind::Regular,
        task: Arc::new(RegularTask::default()),
        cancellation_token: cancellation.clone(),
        handle: AbortOnDropHandle::new(tokio::spawn(async move {
            task_cancellation.cancelled().await;
        })),
        turn_context: Arc::new(turn),
        unrecorded_input: Arc::new(Mutex::new(Vec::new())),
        _timer: None,
    });
    *session.active_turn.lock().await = Some(active);
    (session, cancellation, done)
}

#[tokio::test]
async fn attach_requires_a_task_and_does_not_create_a_turn() {
    let (session, _) = make_session_and_context().await;
    let session = Arc::new(session);

    for empty_turn in [false, true] {
        *session.active_turn.lock().await = empty_turn.then(ActiveTurn::default);
        std::assert_matches!(
            attach(&session, "owner").await.err(),
            Some(ChaosErr::InvalidRequest(message))
                if message == "workflow attachment requires an active turn"
        );
        assert_eq!(session.active_turn.lock().await.is_some(), empty_turn);
    }
}

#[tokio::test]
async fn attach_refuses_already_cancelled_task() {
    let (session, cancellation, _) = active_session().await;
    cancellation.cancel();

    std::assert_matches!(
        attach(&session, "owner").await.err(),
        Some(ChaosErr::InvalidRequest(message)) if message == "the active turn is ending"
    );
    session.active_turn.lock().await.take();
}

#[tokio::test]
async fn attach_refuses_finished_task_even_without_cancellation() {
    let (session, cancellation, _) = active_session().await;
    let mut finished = AbortOnDropHandle::new(tokio::spawn(async {}));
    (&mut finished).await.expect("synthetic task completes");
    {
        let mut active = session.active_turn.lock().await;
        let (_, task) = active
            .as_mut()
            .expect("synthetic turn")
            .tasks
            .first_mut()
            .expect("synthetic task");
        task.handle = finished;
    }

    assert!(!cancellation.is_cancelled());
    std::assert_matches!(
        attach(&session, "owner").await.err(),
        Some(ChaosErr::InvalidRequest(message)) if message == "the active turn is ending"
    );
    session.active_turn.lock().await.take();
}

#[tokio::test]
async fn native_task_cancellation_revokes_attached_scope() {
    let (session, cancellation, _) = active_session().await;
    let scope = attach(&session, "owner").await.expect("active task");
    assert!(!scope.0.lifetime.is_cancelled());
    assert!(Arc::ptr_eq(&scope.0.invocation.session, &session));

    cancellation.cancel();

    tokio::time::timeout(Duration::from_secs(2), scope.0.lifetime.cancelled())
        .await
        .expect("native cancellation must revoke the attachment");
    session.active_turn.lock().await.take();
}

#[tokio::test]
async fn normal_native_completion_immediately_revokes_scope() {
    let (session, cancellation, _) = active_session().await;
    let scope = attach(&session, "owner").await.expect("active task");
    assert!(!scope.0.lifetime.is_cancelled());

    {
        let mut active = session.active_turn.lock().await;
        let active = active.as_mut().expect("synthetic turn");
        let sub_id = active.tasks.first().expect("synthetic task").0.clone();
        assert!(active.remove_completed_task(&sub_id));
        assert!(active.tasks.is_empty());
        assert!(
            scope.0.lifetime.is_cancelled(),
            "completion must revoke authority synchronously"
        );
    }

    assert!(
        cancellation.is_cancelled(),
        "normal completion must cancel the terminal native task token"
    );
    session.active_turn.lock().await.take();
}

#[tokio::test]
async fn scope_rejects_foreign_context_before_parsing_arguments() {
    let (session, _, _) = active_session().await;
    let scope = attach(&session, "owner").await.expect("active task");
    let server = Server::builder("workflow-scope-tests", "0.0.0").build();
    let logger = server.logger_for_session("owner");
    let owner = McpSession::with_id("owner");
    let foreign = McpSession::with_id("foreign");
    let owner_ctx = ExecutionContext::new(Value::Null, &owner, &logger);
    assert!(scope.check_owner(&owner_ctx).is_ok());

    for result in [
        scope
            .run(ExecutionContext::new(Value::Null, &foreign, &logger))
            .await,
        scope.status(ExecutionContext::new(Value::Null, &foreign, &logger)),
        scope.cancel(ExecutionContext::new(Value::Null, &foreign, &logger)),
    ] {
        std::assert_matches!(
            result,
            Err(ToolError::Execution(message))
                if message == "workflow scope belongs to another session"
        );
    }
    std::assert_matches!(scope.status(owner_ctx), Err(ToolError::InvalidArguments(_)));
    scope.close();
    session.active_turn.lock().await.take();
}

#[tokio::test]
async fn close_revokes_all_clones_but_not_the_native_turn() {
    let (session, cancellation, _) = active_session().await;
    let scope = attach(&session, "owner").await.expect("active task");
    let retained = scope.clone();

    scope.close();
    scope.close();
    drop(scope);

    assert!(retained.0.lifetime.is_cancelled());
    assert!(
        !cancellation.is_cancelled(),
        "external session teardown must not cancel the parent native turn"
    );
    session.active_turn.lock().await.take();
}

#[tokio::test]
async fn last_scope_drop_revokes_lifetime() {
    let (session, cancellation, _) = active_session().await;
    let scope = attach(&session, "owner").await.expect("active task");
    let retained = scope.clone();
    let lifetime = scope.0.lifetime.clone();

    drop(scope);
    assert!(
        !lifetime.is_cancelled(),
        "a remaining owner keeps the scope"
    );
    drop(retained);

    assert!(lifetime.is_cancelled());
    assert!(!cancellation.is_cancelled());
    session.active_turn.lock().await.take();
}
