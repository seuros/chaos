use super::*;
use chaos_ipc::protocol::ExecCommandEndEvent;
use chaos_ipc::protocol::ExecCommandSource;
use pretty_assertions::assert_eq;
use std::time::Duration;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(ToString::to_string).collect()
}

fn unknown(cmd: &str) -> ParsedCommand {
    ParsedCommand::Unknown {
        cmd: cmd.to_string(),
    }
}

#[test]
fn commands_are_named_by_program_and_placed_by_what_they_reach() {
    let cases = [
        (
            argv(&["/usr/bin/curl", "-H", "Authorization: x"]),
            vec![],
            Station::Network,
            "curl",
        ),
        (
            argv(&["bash", "-lc", "git push origin main"]),
            vec![unknown("git push origin main")],
            Station::Network,
            "git",
        ),
        (
            argv(&["git", "status"]),
            vec![unknown("git status")],
            Station::Shell,
            "git",
        ),
        (
            argv(&["zsh", "-c", "cargo test -p chaos-kern"]),
            vec![unknown("cargo test -p chaos-kern")],
            Station::Shell,
            "cargo",
        ),
        (
            argv(&["bash", "-lc", "cat src/secret.rs"]),
            vec![ParsedCommand::Read {
                cmd: "cat src/secret.rs".to_string(),
                name: "secret.rs".to_string(),
                path: "src/secret.rs".into(),
            }],
            Station::Files,
            "cat",
        ),
    ];
    for (argv, parsed, station, label) in cases {
        assert_eq!(
            command(&argv, &parsed),
            (station, label.to_string()),
            "{argv:?}"
        );
    }
}

#[test]
fn a_finished_command_carries_no_arguments_or_output() {
    let msg = EventMsg::ExecCommandEnd(ExecCommandEndEvent {
        call_id: "call-1".to_string(),
        process_id: None,
        turn_id: "turn-1".to_string(),
        command: argv(&["bash", "-lc", "curl -H 'token: hunter2' https://x"]),
        cwd: "/home/someone/private".into(),
        parsed_cmd: vec![unknown("curl -H 'token: hunter2' https://x")],
        source: ExecCommandSource::Agent,
        interaction_input: Some("hunter2".to_string()),
        stdout: "hunter2".to_string(),
        stderr: "hunter2".to_string(),
        aggregated_output: "hunter2".to_string(),
        exit_code: 1,
        duration: Duration::from_millis(42),
        formatted_output: "hunter2".to_string(),
        status: ExecCommandStatus::Failed,
    });
    let activity = Activity::from_event(&msg).unwrap();
    assert_eq!(
        activity,
        Activity::CallEnded {
            call_id: "call-1".to_string(),
            station: Station::Network,
            label: "curl".to_string(),
            outcome: CallOutcome::Failed,
            duration_ms: Some(42),
        }
    );
    let json = serde_json::to_string(&activity).unwrap();
    assert!(
        !json.contains("hunter2") && !json.contains("private"),
        "{json}"
    );
}

#[tokio::test]
async fn subscribers_receive_what_sessions_deliver() {
    let mut rx = subscribe();
    let (session, turn_context) = crate::chaos::make_session_and_context().await;
    session
        .send_event(
            &turn_context,
            EventMsg::Error(chaos_ipc::protocol::ErrorEvent {
                message: "provider said: hunter2".to_string(),
                chaos_error_info: None,
            }),
        )
        .await;
    // The bus is shared by every test in this binary; find ours.
    let ours = async {
        loop {
            let event = rx.recv().await.unwrap();
            if event.process_id == session.conversation_id {
                return event.activity;
            }
        }
    };
    let activity = tokio::time::timeout(Duration::from_secs(5), ours)
        .await
        .expect("the session's event never reached the bus");
    assert_eq!(activity, Activity::Failed);
}
