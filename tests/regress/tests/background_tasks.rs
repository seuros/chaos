use anyhow::Result;
use chaos_ipc::background_tasks::WakePolicy;
use chaos_ipc::protocol::{ApprovalPolicy, EventMsg, Op, SandboxPolicy};
use chaos_ipc::user_input::UserInput;
use chaos_session::background::{BackgroundWait, WaitEvent};
use core_test_support::responses::{
    ev_assistant_message, ev_completed, ev_function_call, ev_response_created, mount_sse_sequence,
    sse, start_mock_server,
};
use core_test_support::test_chaos::{TestChaos, test_chaos};
use core_test_support::wait_for_event;
use std::time::Duration;

fn responses() -> Vec<String> {
    vec![
        sse(vec![
            ev_response_created("launch"),
            ev_function_call(
                "background-exec",
                "exec_command",
                r#"{"cmd":"sleep 2; printf background_result","yield_time_ms":250}"#,
            ),
            ev_completed("launch"),
        ]),
        sse(vec![
            ev_response_created("initial-answer"),
            ev_assistant_message("started", "Work is running."),
            ev_completed("initial-answer"),
        ]),
        sse(vec![
            ev_response_created("completion-answer"),
            ev_assistant_message("done", "Background work completed."),
            ev_completed("completion-answer"),
        ]),
    ]
}

// Unlike TestChaos::submit_turn, leave the event stream to BackgroundWait.
async fn submit(test: &TestChaos, prompt: &str) -> Result<()> {
    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: prompt.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd.path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::RootAccess,
            model: test.session_configured.model.clone(),
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_exit_wakes_the_existing_runner_and_waits_for_quiescence() -> Result<()> {
    shell_completion(true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an isolated journald server via CHAOS_JOURNALD_SOCKET"]
async fn durable_shell_completion_commits_before_continuing() -> Result<()> {
    anyhow::ensure!(
        std::env::var_os("CHAOS_JOURNALD_SOCKET").is_some(),
        "start an isolated journald server before running this test"
    );
    shell_completion(false).await
}

async fn shell_completion(ephemeral: bool) -> Result<()> {
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(&server, responses()).await;
    let test = test_chaos()
        .with_config(move |config| config.ephemeral = ephemeral)
        .build(&server)
        .await?;
    if !ephemeral {
        chaos_kern::runtime_db::mount_vfs_for_startup(&test.config).await?;
    }
    submit(&test, "Start background work.").await?;
    let mut wait = BackgroundWait::new(&test.process, true, Duration::from_secs(20));
    let mut turns = Vec::new();
    loop {
        match wait.next(&test.process).await {
            WaitEvent::Event(event) => {
                if let EventMsg::TurnComplete(turn) = event.msg {
                    turns.push(turn);
                }
            }
            WaitEvent::Complete => break,
            WaitEvent::Stopped(reason) => {
                server.reset().await;
                anyhow::bail!("{reason}");
            }
        }
    }
    assert_eq!(turns.len(), 2);
    assert!(turns[1].turn_id.starts_with("task-completion-"));
    assert!(test.process.subscribe_activity().borrow().is_quiescent());
    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        3,
        "one owner turn and one continuation, not a second runner"
    );
    let prompt = requests[2].body_json();
    let input = prompt["input"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("response request is missing its input array"))?;
    let notifications = input
        .iter()
        .filter(|item| item.to_string().contains("<task_completion>"))
        .collect::<Vec<_>>();
    assert_eq!(notifications.len(), 1);
    // The Responses adapter maps the kernel's canonical system role to developer.
    assert_eq!(notifications[0]["role"], "developer");
    assert!(!notifications[0].to_string().contains("background_result"));
    test.process.shutdown_and_wait().await?;
    if !ephemeral {
        use chaos_ipc::background_tasks::TaskJournalEvent;
        use chaos_ipc::protocol::RolloutItem;
        let history = chaos_kern::RolloutRecorder::get_rollout_history_for_process(
            test.session_configured.session_id,
        )
        .await?
        .get_rollout_items();
        assert_eq!(
            history
                .iter()
                .filter(|item| matches!(
                    item,
                    RolloutItem::BackgroundTask(TaskJournalEvent::Delivered { .. })
                ))
                .count(),
            1,
            "completion delivery must survive in the journal exactly once"
        );
        assert!(history.iter().any(|item| matches!(
            item,
            RolloutItem::BackgroundTask(TaskJournalEvent::ContinuationFinished { .. })
        )));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_retains_shell_completion_until_owner_input() -> Result<()> {
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(&server, responses()).await;
    let test = test_chaos()
        .with_config(|config| config.ephemeral = true)
        .build(&server)
        .await?;
    submit(&test, "Start background work.").await?;
    wait_for_event(&test.process, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    test.process.submit(Op::Interrupt).await?;
    let mut activity = test.process.subscribe_activity();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if activity.borrow().outstanding_tasks == 0
                && activity.borrow().wake_policy == WakePolicy::Interrupted
            {
                break;
            }
            activity.changed().await.expect("live process");
        }
    })
    .await?;
    assert_eq!(
        mock.requests().len(),
        2,
        "interrupt must prevent automatic sampling"
    );
    assert_eq!(activity.borrow().pending_completions, 1);
    submit(&test, "Now continue.").await?;
    wait_for_event(&test.process, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(mock.requests().len(), 3);
    test.process.shutdown_and_wait().await?;
    Ok(())
}
