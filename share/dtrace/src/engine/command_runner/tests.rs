use super::*;

#[tokio::test]
async fn bounds_output_and_times_out_blocked_stdin() {
    let shell = CommandShell {
        program: "/bin/sh".into(),
        args: vec!["-c".into()],
        builder: None,
    };
    let mut handler = ConfiguredHandler {
        event_name: chaos_ipc::protocol::HookEventName::BeforeTurn,
        command: "cat >/dev/null; yes output".into(),
        matcher: None,
        timeout_sec: 1,
        status_message: None,
        source_path: "chaos://hooks/test".into(),
        display_order: 0,
    };
    let result = run_command(&shell, &handler, "{}", &std::env::temp_dir()).await;
    assert!(result.error.unwrap().contains("1 MiB"));
    handler.command = "sleep 30".into();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        run_command(
            &shell,
            &handler,
            &"x".repeat(1024 * 1024),
            &std::env::temp_dir(),
        ),
    )
    .await
    .unwrap();
    assert!(result.error.unwrap().contains("timed out"));
}
