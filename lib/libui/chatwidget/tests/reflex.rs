use super::*;
use pretty_assertions::assert_eq;

pub(super) async fn run() {
    let (mut chat, mut rx, mut ops) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.config.reflex.clear();
    while rx.try_recv().is_ok() {}
    while ops.try_recv().is_ok() {}

    for command in [SlashCommand::Accounts, SlashCommand::Reflex] {
        chat.dispatch_command(command);
        let event = rx.try_recv().unwrap();
        assert!(matches!(
            (command, event),
            (SlashCommand::Accounts, AppEvent::OpenAccountsPopup)
                | (SlashCommand::Reflex, AppEvent::OpenReflexPopup)
        ));
        assert!(chat.bottom_pane.composer_text().is_empty());
        assert!(ops.try_recv().is_err());
    }

    for args in ["sk-do-not-record", "test sk-do-not-record", "unknown"] {
        chat.bottom_pane
            .set_composer_text(format!("/reflex {args}"), vec![], vec![]);
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert!(chat.bottom_pane.composer_text().is_empty());
        let lines = drain_insert_history(&mut rx);
        let text = lines
            .iter()
            .flatten()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Usage: /reflex"));
        assert!(!text.contains("sk-do-not-record"));
        assert!(ops.try_recv().is_err());
    }

    chat.handle_key_event(KeyEvent::from(KeyCode::Up));
    assert!(chat.bottom_pane.composer_text().is_empty());
    chat.bottom_pane
        .set_composer_text("/reflex test".into(), vec![], vec![]);
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert!(chat.bottom_pane.composer_text().is_empty());
    let (process_id, result) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match rx.recv().await.unwrap() {
                AppEvent::ReflexTestFinished { process_id, result } => break (process_id, result),
                AppEvent::InsertHistoryCell(_) => {}
                event => panic!("unexpected event: {event:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(process_id, chat.process_id);
    assert!(result.as_ref().unwrap_err().contains("No action-risk"));
    chat.reflex_test_finished(process_id, result);
    assert!(!drain_insert_history(&mut rx).is_empty());
    assert!(ops.try_recv().is_err());

    chat.reflex_test_finished(Some(ProcessId::new()), Err("stale result".into()));
    assert!(rx.try_recv().is_err());
    chat.bottom_pane.set_task_running(true);
    chat.dispatch_command_with_args(SlashCommand::Reflex, "test".into(), vec![]);
    let text = drain_insert_history(&mut rx)
        .into_iter()
        .flatten()
        .map(|line| line.to_string())
        .collect::<String>();
    assert!(text.contains("disabled"));
    assert!(ops.try_recv().is_err());
}
