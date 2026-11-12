use super::*;
use crate::error::{ChaosErr, SandboxErr};
use crate::exec::StreamOutput;
use crate::tools::sandboxing::ToolError;

#[test]
fn sandbox_errors_survive_orchestration_as_denials_not_spawn_failures() {
    let evidence = "macOS sandbox denials:\n  (mbx) file-write-create /cache/mbx\n";
    let output = ExecToolCallOutput {
        exit_code: 1,
        stderr: StreamOutput::new(format!("{evidence}Operation not permitted")),
        aggregated_output: StreamOutput::new(format!("{evidence}Operation not permitted")),
        ..Default::default()
    };
    let error =
        ExecError::from_tool_error(ToolError::Chaos(ChaosErr::Sandbox(SandboxErr::Denied {
            output: Box::new(output),
            network_policy_decision: None,
        })));
    assert!(error.to_string().contains("file-write-create /cache/mbx"));
    let ExecError::SandboxDenied { message, output } = error else {
        panic!("expected a sandbox denial, got {error:?}");
    };
    assert!(message.starts_with(evidence));
    assert_eq!(output.exit_code, 1);
}

#[test]
fn unrelated_tool_errors_keep_their_existing_spawn_error_format() {
    let error = ExecError::from_tool_error(ToolError::Rejected("missing executable".into()));
    let ExecError::CreateProcess { message } = error else {
        panic!("expected a spawn error: {error:?}");
    };
    assert_eq!(message, "Rejected(\"missing executable\")");
}

#[test]
fn network_denials_keep_their_policy_details() {
    let policy = serde_json::from_value(serde_json::json!({
        "decision": "deny",
        "source": "decider",
        "protocol": "http",
        "host": "blocked.example",
        "reason": "not_allowed",
        "port": 443
    }))
    .expect("network policy fixture");
    let error = ToolError::Chaos(ChaosErr::Sandbox(SandboxErr::Denied {
        output: Box::new(ExecToolCallOutput::default()),
        network_policy_decision: Some(policy),
    }));
    let expected = format!("{error:?}");
    let ExecError::SandboxDenied { message, .. } = ExecError::from_tool_error(error) else {
        panic!("expected a network sandbox denial");
    };
    assert_eq!(message, expected);
    assert!(message.contains("blocked.example"));
    assert!(message.contains("not_allowed"));
}
