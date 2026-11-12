use super::*;
use crate::exec::{ExecError, NoopSpawnLifecycle, StreamOutput};
use std::collections::HashMap;
use std::time::Duration;

fn captured_diagnostics() -> SandboxDiagnostics {
    SandboxDiagnostics {
        enabled: true,
        logger: Mutex::new(None),
        report: OnceCell::new_with(Some(format_report(&[SandboxDenial {
            name: "mbx".into(),
            capability: "file-write-create /cache/mbx index".into(),
        }]))),
    }
}

#[tokio::test]
async fn reports_prepend_evidence_preserve_output_and_explain_missing_audit_events() {
    for diagnostics in [
        captured_diagnostics(),
        SandboxDiagnostics {
            enabled: true,
            logger: Mutex::new(None),
            report: OnceCell::new(),
        },
    ] {
        let mut output = ExecToolCallOutput {
            exit_code: 1,
            stdout: StreamOutput::new("stdout stays intact".into()),
            stderr: StreamOutput::new("mbx: Operation not permitted".into()),
            aggregated_output: StreamOutput::new("combined output".into()),
            ..Default::default()
        };
        diagnostics.enrich_denial(&mut output).await;
        assert!(output.stderr.text.starts_with("macOS sandbox"));
        assert!(output.stderr.text.ends_with("mbx: Operation not permitted"));
        assert!(output.aggregated_output.text.ends_with("combined output"));
        assert_eq!(output.stdout.text, "stdout stays intact");
        assert_eq!(output.exit_code, 1);
        let report = diagnostics.report.get().expect("cached report");
        if report.contains("unavailable") {
            assert!(report.contains("EPERM alone does not identify the denied operation or path"));
            assert!(!report.contains("/cache/mbx"));
        } else {
            assert!(report.contains("file-write-create /cache/mbx index"));
        }
    }
    let mut output = ExecToolCallOutput::default();
    SandboxDiagnostics::start(SandboxType::None)
        .enrich_denial(&mut output)
        .await;
    assert!(output.stderr.text.is_empty());
}

#[test]
fn reports_escape_control_characters_and_limit_size() {
    let report = format_report(&[SandboxDenial {
        name: "mbx\nspoof".into(),
        capability: format!("file-read-data /cache/\u{1b}{}", "x".repeat(20_000)),
    }]);
    assert!(report.len() <= 8 * 1024);
    assert!(report.contains("mbx\\nspoof"));
    assert!(!report.contains('\u{1b}'));
}

#[tokio::test]
async fn managed_pipe_and_pty_denials_include_audit_operation_and_path() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let args = vec![
        "-c".to_string(),
        "printf 'mbx: Operation not permitted' >&2; exit 1".to_string(),
    ];
    for tty in [false, true] {
        let spawned = if tty {
            chaos_pty::spawn_pty_process(
                "/bin/sh",
                &args,
                &cwd,
                &HashMap::new(),
                &None,
                chaos_pty::TerminalSize::default(),
            )
            .await?
        } else {
            chaos_pty::spawn_pipe_process_no_stdin("/bin/sh", &args, &cwd, &HashMap::new(), &None)
                .await?
        };
        let result = crate::exec::ExecProcess::from_spawned(
            spawned,
            SandboxType::MacosSeatbelt,
            Box::new(NoopSpawnLifecycle),
            captured_diagnostics(),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(process) => {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !process.has_exited() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await?;
                process
                    .check_for_sandbox_denial()
                    .await
                    .expect_err("sandbox denial")
            }
        };
        let ExecError::SandboxDenied { message, output } = error else {
            panic!("expected a typed sandbox denial: {error:?}");
        };
        assert!(message.contains("file-write-create /cache/mbx index"));
        assert!(output.stderr.text.contains("mbx: Operation not permitted"));
    }
    Ok(())
}
