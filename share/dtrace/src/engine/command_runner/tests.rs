use super::*;

#[tokio::test]
async fn bounds_output_and_times_out_blocked_stdin() {
    let shell = CommandShell {
        program: "/bin/sh".into(),
        args: vec!["-c".into()],
        builder: None,
        output_limit_bytes: DEFAULT_OUTPUT_LIMIT_BYTES,
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
    assert!(result.error.unwrap().contains("128 KiB"));
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

#[tokio::test]
async fn output_limit_is_taken_from_the_shell() {
    let handler = ConfiguredHandler {
        event_name: chaos_ipc::protocol::HookEventName::BeforeTurn,
        command: "printf '%s' 0123456789abcdef".into(),
        matcher: None,
        timeout_sec: 5,
        status_message: None,
        source_path: "chaos://hooks/test".into(),
        display_order: 0,
    };
    let mut shell = CommandShell {
        program: "/bin/sh".into(),
        args: vec!["-c".into()],
        builder: None,
        output_limit_bytes: 8,
    };
    let result = run_command(&shell, &handler, "{}", &std::env::temp_dir()).await;
    assert_eq!(result.error.as_deref(), Some("hook output exceeds 8 bytes"));
    assert_eq!(result.stdout, "");

    shell.output_limit_bytes = 16;
    let result = run_command(&shell, &handler, "{}", &std::env::temp_dir()).await;
    assert_eq!(result.error, None);
    assert_eq!(result.stdout, "0123456789abcdef");
}

#[test]
fn output_limit_label_prefers_whole_kib() {
    assert_eq!(output_limit_label(128 * 1024), "128 KiB");
    assert_eq!(output_limit_label(1024 * 1024), "1024 KiB");
    assert_eq!(output_limit_label(8), "8 bytes");
    assert_eq!(output_limit_label(0), "0 bytes");
}
