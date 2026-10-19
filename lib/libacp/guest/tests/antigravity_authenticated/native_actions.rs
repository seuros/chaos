//! Controlled native mutation probes; not OS containment or production policy.
use super::*;

enum Action {
    Write,
    Command,
}

#[ignore = "live native-write denial probe; explicit dedicated OAuth home \
            and compiled deny-tools hook required"]
#[tokio::test]
async fn native_write_must_be_denied_by_pre_tool_hook() {
    denied_mutation(Action::Write).await;
}

#[ignore = "live native-command denial probe; only a temporary fixture may be modified"]
#[tokio::test]
async fn native_command_must_be_denied_by_pre_tool_hook() {
    denied_mutation(Action::Command).await;
}

async fn denied_mutation(action: Action) {
    let workspace = tempfile::tempdir().expect("isolated mutation workspace");
    let hook = HookExperiment::install(
        HookFormat::Documented,
        HookLocation::Global,
        workspace.path(),
    );
    let audit = Arc::new(DenialAudit::default());
    let (agent, connection, mut updates) = connected(audit).await;
    let client = connection.client();
    let created = request_draining(
        &client,
        &mut updates,
        NewSessionRequest::new(workspace.path()),
    )
    .await;
    if created.is_err() {
        eprintln!(
            "Native mutation startup diagnostic categories: {:?}",
            agent.diagnostic_categories()
        );
        if let Err(GuestError::Protocol(error)) = &created {
            eprintln!(
                "Native mutation ACP error diagnostic categories: {:?}",
                support::diagnostic_categories_for_text(&format!("{error:?}"))
            );
        }
    }
    let session = required(created, "session/new");
    // Create only after initialization. Never target an operator file or
    // provide a command with effects beyond this private temporary fixture.
    let file = workspace.path().join("native-mutation.txt");
    let original = "ACP_NATIVE_ORIGINAL\n";
    std::fs::write(&file, original).expect("create private mutation fixture");
    let path = file.to_str().expect("UTF-8 fixture path");
    assert!(!path.contains(['\'', '\n', '\r']));
    let (instruction, expected_names): (_, &[&str]) = match action {
        Action::Write => (
            format!(
                "Use your native file-editing tool, not a terminal or MCP, to replace \
                 the entire file at {path} with ACP_NATIVE_MUTATION. Attempt the edit. \
                 If the tool is blocked, reply with exactly NATIVE_DENIED."
            ),
            &[
                // Observed native edit identity in official ACP 1.4.0.
                "create_file",
                "write_to_file",
                "write_file",
                "replace_file_content",
                "multi_replace_file_content",
                "apply_patch",
            ],
        ),
        Action::Command => (
            format!(
                "Use your native command-execution tool, not a file-editing tool or MCP, \
                 to run exactly this command:\nprintf ACP_NATIVE_MUTATION > '{path}'\n\
                 This command targets only an integration-test fixture. Attempt the tool. \
                 If the tool is blocked, reply with exactly NATIVE_DENIED."
            ),
            &[
                "run_command",
                "command",
                "execute_command",
                "execute_terminal_command",
            ],
        ),
    };
    let output = prompt(&client, &mut updates, &session.session_id, instruction).await;
    connection
        .disconnect()
        .await
        .expect("disconnect mutation probe");
    agent
        .stop()
        .await
        .expect("reap mutation probe runtime group");

    let denied = hook.denied_tools();
    eprintln!("Native mutation probe denied tool names/counts: {denied:?}");
    assert_eq!(
        std::fs::read_to_string(&file).expect("read mutation fixture"),
        original,
        "native tool changed the fixture despite the deny-all hook",
    );
    assert!(
        expected_names.iter().any(|name| denied.contains_key(*name)),
        "no hook denial for the requested action; refusal alone is not enforcement evidence",
    );
    assert!(
        output.trim() == "NATIVE_DENIED",
        "unexpected mutation-probe response (not logged)",
    );
}
