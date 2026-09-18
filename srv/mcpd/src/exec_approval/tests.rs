use pretty_assertions::assert_eq;

use super::*;

#[test]
fn exec_approval_params_preserve_command_context() -> anyhow::Result<()> {
    let process_id = ProcessId::new();
    let command = vec!["touch".to_string(), "created.txt".to_string()];
    let cwd = PathBuf::from("/tmp/chaos-mcpd-test");
    let parsed = chaos_sh::parse_command::parse_command(&command);

    let params = exec_approval_elicitation_params(
        command.clone(),
        cwd.clone(),
        "tool-call".to_string(),
        "event-id".to_string(),
        "exec-call".to_string(),
        parsed.clone(),
        process_id,
    );

    assert_eq!(
        serde_json::to_value(params)?,
        json!({
            "message": "Allow Chaos to run `touch created.txt` in `/tmp/chaos-mcpd-test`?",
            "requestedSchema": {
                "type": "object",
                "properties": {}
            },
            "_meta": {
                "processId": process_id,
                "codex_elicitation": "exec-approval",
                "codex_mcp_tool_call_id": "tool-call",
                "chaos_event_id": "event-id",
                "codex_call_id": "exec-call",
                "codex_command": command,
                "codex_cwd": cwd,
                "codex_parsed_cmd": parsed
            }
        })
    );
    Ok(())
}
