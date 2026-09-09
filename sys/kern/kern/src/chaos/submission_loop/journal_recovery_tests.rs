use super::*;
use crate::tasks::{SessionTask, SessionTaskContext};
use chaos_ipc::models::{ContentItem, ResponseInputItem};
use chaos_ipc::user_input::UserInput;
use std::future::Future;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

struct PausedTask(bool);

impl SessionTask for PausedTask {
    fn resumes_after_journal_recovery(&self) -> bool {
        self.0
    }

    fn kind(&self) -> crate::state::TaskKind {
        crate::state::TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "test.journal_recovery"
    }

    fn run(
        self: Arc<Self>,
        _session: Arc<SessionTaskContext>,
        _context: Arc<TurnContext>,
        _input: Vec<UserInput>,
        cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> {
        Box::pin(async move {
            cancellation.cancelled().await;
            None
        })
    }
}

#[tokio::test]
async fn journal_recovery_does_not_resume_idle_sessions() {
    let (session, _) = crate::chaos::make_session_and_context().await;
    assert!(
        pause_for_journal_recovery(&Arc::new(session))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn journal_recovery_does_not_replay_standalone_tasks() {
    let (session, context) = crate::chaos::make_session_and_context().await;
    let session = Arc::new(session);
    session
        .spawn_task(Arc::new(context), Vec::new(), PausedTask(false))
        .await;
    assert!(pause_for_journal_recovery(&session).await.is_none());
    assert!(session.active_turn.lock().await.is_none());
}

#[tokio::test]
async fn journal_recovery_preserves_unrecorded_and_steered_input() {
    let (session, context) = crate::chaos::make_session_and_context().await;
    let session = Arc::new(session);
    let context = Arc::new(context);
    let input = vec![UserInput::Text {
        text: "original request".into(),
        text_elements: Vec::new(),
    }];
    session
        .spawn_task(context.clone(), input.clone(), PausedTask(true))
        .await;
    let pending = ResponseInputItem::Message {
        role: "user".into(),
        content: vec![ContentItem::InputText {
            text: "steering".into(),
        }],
    };
    {
        let active = session.active_turn.lock().await;
        active
            .as_ref()
            .unwrap()
            .turn_state
            .lock()
            .await
            .push_pending_input(pending.clone());
    }

    let continuation = pause_for_journal_recovery(&session).await.unwrap();
    assert_eq!(continuation.context.sub_id, context.sub_id);
    assert_eq!(continuation.input, input);
    assert_eq!(continuation.pending, vec![pending]);
    assert!(session.active_turn.lock().await.is_none());
    assert!(session.clone_history().await.raw_items().is_empty());

    resume_after_journal_recovery(&session, continuation).await;
    {
        let active = session.active_turn.lock().await;
        let task = active.as_ref().unwrap().tasks.values().next().unwrap();
        assert_ne!(task.turn_context.sub_id, context.sub_id);
        assert_eq!(task.turn_context.model_info.slug, context.model_info.slug);
        assert!(task.task.resumes_after_journal_recovery());
    }
    session
        .abort_all_tasks(chaos_ipc::protocol::TurnAbortReason::Replaced)
        .await;
    let history = session.clone_history().await;
    let texts: Vec<_> = history
        .raw_items()
        .iter()
        .filter_map(|item| {
            let chaos_ipc::models::ResponseItem::Message { role, content, .. } = item else {
                return None;
            };
            if role != "user" {
                return None;
            }
            let ContentItem::InputText { text } = &content[0] else {
                return None;
            };
            Some(text.as_str())
        })
        .collect();
    assert_eq!(&texts[..2], &["original request", "steering"]);
    for expected in ["original request", "steering"] {
        assert_eq!(texts.iter().filter(|text| **text == expected).count(), 1);
    }
    assert!(texts.iter().all(|text| !text.is_empty()));
}

#[tokio::test]
async fn journal_recovery_does_not_duplicate_recorded_input() {
    let (session, context) = crate::chaos::make_session_and_context().await;
    let session = Arc::new(session);
    let context = Arc::new(context);
    let input = vec![UserInput::Text {
        text: "already recorded".into(),
        text_elements: Vec::new(),
    }];
    session
        .spawn_task(context.clone(), input.clone(), PausedTask(true))
        .await;
    let unrecorded = {
        let active = session.active_turn.lock().await;
        active
            .as_ref()
            .unwrap()
            .tasks
            .values()
            .next()
            .unwrap()
            .unrecorded_input
            .clone()
    };
    session
        .record_initial_user_prompt(
            &context,
            &input,
            crate::prompt_images::response_input_item_from_user_input(input.clone()).into(),
            &unrecorded,
        )
        .await;
    let continuation = pause_for_journal_recovery(&session).await.unwrap();
    assert!(continuation.input.is_empty());
    assert!(continuation.pending.is_empty());
    assert_eq!(session.clone_history().await.raw_items().len(), 1);
}

#[tokio::test]
async fn journal_recovery_does_not_restart_finished_turns() {
    let (session, context) = crate::chaos::make_session_and_context().await;
    let session = Arc::new(session);
    let context = Arc::new(context);
    session
        .spawn_task(context.clone(), Vec::new(), PausedTask(true))
        .await;
    session
        .completions
        .finished
        .lock()
        .await
        .push((context, Some("done".into())));
    assert!(pause_for_journal_recovery(&session).await.is_none());
    assert!(session.active_turn.lock().await.is_none());
}
