use super::*;
use serde_json::json;
use std::process::Stdio;

#[test]
fn audit_messages_preserve_operations_paths_and_process_names() {
    for (message, expected) in [
        (
            concat!(
                "Sandbox: mbx(123) deny(1) file-write-create ",
                "/Users/test/Library/Application Support/mbx"
            ),
            Some((
                123,
                "mbx",
                "file-write-create /Users/test/Library/Application Support/mbx",
            )),
        ),
        (
            "Sandbox: a (helper)(456) deny(1) mach-lookup com.example.service",
            Some((456, "a (helper)", "mach-lookup com.example.service")),
        ),
        ("Sandbox: mbx(123) allow(1) file-write-data /tmp/file", None),
        (
            "Sandbox: mbx(no-pid) deny(1) file-write-data /tmp/file",
            None,
        ),
        ("Sandbox: mbx(0) deny(1) file-write-data /tmp/file", None),
        ("Sandbox: mbx(-1) deny(1) file-write-data /tmp/file", None),
        (
            "Sandbox: mbx(123) deny(invalid) file-write-data /tmp/file",
            None,
        ),
        ("Sandbox: mbx(123) deny(1)", None),
        ("mbx[error]: Operation not permitted (os error 1)", None),
    ] {
        assert_eq!(
            parse_message(message),
            expected.map(|(pid, name, capability)| (pid, name.into(), capability.into())),
            "{message}"
        );
    }
}

#[test]
fn audit_reports_filter_by_pid_deduplicate_and_bound_results() {
    let event = |message: &str| json!({"eventMessage": message}).to_string();
    let logs = [
        "not json".to_string(),
        event("Sandbox: unrelated(999) deny(1) file-read-data /private/unrelated"),
        event("Sandbox: mbx(123) deny(1) file-write-create /cache/mbx"),
        event("Sandbox: mbx(123) deny(1) file-write-create /cache/mbx"),
        event("Sandbox: child(456) deny(1) file-read-data /cache/index"),
        event("Sandbox: mbx(123) allow(1) file-write-create /allowed"),
    ]
    .join("\n");
    assert_eq!(
        collect_denials(&logs, &HashSet::from([123, 456])),
        [
            SandboxDenial {
                name: "mbx".into(),
                capability: "file-write-create /cache/mbx".into(),
            },
            SandboxDenial {
                name: "child".into(),
                capability: "file-read-data /cache/index".into(),
            },
        ]
    );
    assert!(collect_denials(&logs, &HashSet::new()).is_empty());

    let logs = (0..100)
        .map(|index| {
            event(&format!(
                "Sandbox: mbx(123) deny(1) file-read-data /cache/{index}"
            ))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let denials = collect_denials(&logs, &HashSet::from([123]));
    assert_eq!(denials.len(), 16);
    assert_eq!(denials[15].capability, "file-read-data /cache/99");
}

#[tokio::test]
async fn logger_preserves_root_pid_when_descendant_tracking_is_unavailable() {
    let log = json!({
        "eventMessage": "Sandbox: mbx(123) deny(1) file-write-create /cache/mbx"
    })
    .to_string();
    let child = tokio::process::Command::new("/usr/bin/printf")
        .args(["%s\n", &log])
        .stdout(Stdio::piped())
        .spawn()
        .expect("fixture log stream");
    let mut logger = DenialLogger::from_log_stream(child).expect("fixture logger");
    logger.root_pid = Some(123);
    assert_eq!(
        logger.finish().await,
        [SandboxDenial {
            name: "mbx".into(),
            capability: "file-write-create /cache/mbx".into(),
        }]
    );
}

#[test]
fn truncated_capture_does_not_invent_a_shortened_audit_path() {
    let prefix = r#"{"eventMessage":"Sandbox: mbx(123) deny(1) file-read-data /"#;
    let suffix = "\"}\n";
    let log = format!(
        "{prefix}{}OMITTED_PATH_COMPONENT{}{suffix}",
        "x".repeat(AUDIT_LOG_MAX_BYTES / 2 - prefix.len()),
        "z".repeat(AUDIT_LOG_MAX_BYTES / 2 - suffix.len())
    );
    let mut buffer = HeadTailBuffer::new(AUDIT_LOG_MAX_BYTES);
    buffer.push_chunk(log.into_bytes());
    assert!(buffer.omitted_bytes() > 0);
    let pids = HashSet::from([123]);
    assert_eq!(
        collect_denials(&String::from_utf8_lossy(&buffer.to_bytes()), &pids).len(),
        1,
        "plain concatenation would incorrectly report a truncated path"
    );
    assert!(collect_buffered_denials(&buffer, &pids).is_empty());
}
