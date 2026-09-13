use std::path::PathBuf;

use chaos_ipc::ProcessId;
use chaos_ipc::models::SandboxPermissions;
use jiff::Timestamp;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::HookEvent;
use super::HookEventAfterAgent;
use super::HookEventAfterToolUse;
use super::HookPayload;
use super::HookToolInput;
use super::HookToolInputLocalShell;
use super::HookToolKind;

#[test]
fn hook_payload_serializes_stable_wire_shape() {
    let session_id = ProcessId::new();
    let process_id = ProcessId::new();
    let payload = HookPayload {
        session_id,
        cwd: PathBuf::from("tmp"),
        client: None,
        triggered_at: "2025-01-01T00:00:00Z".parse::<Timestamp>().unwrap(),
        hook_event: HookEvent::AfterAgent {
            event: HookEventAfterAgent {
                process_id,
                turn_id: "turn-1".to_string(),
                input_messages: vec!["hello".to_string()],
                last_assistant_message: Some("hi".to_string()),
            },
        },
    };

    let actual = serde_json::to_value(payload).expect("serialize hook payload");
    let expected = json!({
        "session_id": session_id.to_string(),
        "cwd": "tmp",
        "triggered_at": "2025-01-01T00:00:00Z",
        "hook_event": {
            "event_type": "after_agent",
            "process_id": process_id.to_string(),
            "turn_id": "turn-1",
            "input_messages": ["hello"],
            "last_assistant_message": "hi",
        },
    });

    assert_eq!(actual, expected);
}

#[test]
fn after_tool_use_payload_serializes_stable_wire_shape() {
    let session_id = ProcessId::new();
    let payload = HookPayload {
        session_id,
        cwd: PathBuf::from("tmp"),
        client: None,
        triggered_at: "2025-01-01T00:00:00Z".parse::<Timestamp>().unwrap(),
        hook_event: HookEvent::AfterToolUse {
            event: HookEventAfterToolUse {
                turn_id: "turn-2".to_string(),
                call_id: "call-1".to_string(),
                tool_name: "local_shell".to_string(),
                tool_kind: HookToolKind::LocalShell,
                tool_input: HookToolInput::LocalShell {
                    params: HookToolInputLocalShell {
                        command: vec!["cargo".to_string(), "fmt".to_string()],
                        workdir: Some("chaos".to_string()),
                        timeout_ms: Some(60_000),
                        sandbox_permissions: Some(SandboxPermissions::UseDefault),
                        justification: None,
                        prefix_rule: None,
                    },
                },
                executed: true,
                success: true,
                duration_ms: 42,
                mutating: true,
                sandbox: "none".to_string(),
                sandbox_policy: "root-access".to_string(),
                output_preview: "ok".to_string(),
            },
        },
    };

    let actual = serde_json::to_value(payload).expect("serialize hook payload");
    let expected = json!({
        "session_id": session_id.to_string(),
        "cwd": "tmp",
        "triggered_at": "2025-01-01T00:00:00Z",
        "hook_event": {
            "event_type": "after_tool_use",
            "turn_id": "turn-2",
            "call_id": "call-1",
            "tool_name": "local_shell",
            "tool_kind": "local_shell",
            "tool_input": {
                "input_type": "local_shell",
                "params": {
                    "command": ["cargo", "fmt"],
                    "workdir": "chaos",
                    "timeout_ms": 60000,
                    "sandbox_permissions": "use_default",
                    "justification": null,
                    "prefix_rule": null,
                },
            },
            "executed": true,
            "success": true,
            "duration_ms": 42,
            "mutating": true,
            "sandbox": "none",
            "sandbox_policy": "root-access",
            "output_preview": "ok",
        },
    });

    assert_eq!(actual, expected);
}
