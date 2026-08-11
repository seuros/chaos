use std::fs;
use std::path::Path;

use anyhow::Result;
use serde_json::Value as JsonValue;
use tempfile::TempDir;

mod common;

#[cfg(unix)]
fn write_fake_agy(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::write(
        path,
        r#"#!/bin/sh
set -eu

case "${1:-}" in
  *)
    test -n "${CHAOS_CLAMP_MCP_SOCKET:-}"
    test -n "${CHAOS_CLAMP_MCP_TOKEN:-}"
    test -f "$HOME/.gemini/config/mcp_config.json"
    test -f "$HOME/.gemini/antigravity-cli/settings.json"
    grep -q '"clamp-session-bridge"' "$HOME/.gemini/config/mcp_config.json"
    ! grep -q 'CHAOS_CLAMP_MCP_' "$HOME/.gemini/config/mcp_config.json"
    grep -q 'mcp(chaos/\*)' "$HOME/.gemini/antigravity-cli/settings.json"
    grep -q 'command(\*)' "$HOME/.gemini/antigravity-cli/settings.json"
    case "$*" in
      *"--conversation conversation-e2e"*) response=CHAOS_AGY_RESUMED ;;
      *) response=CHAOS_AGY_FRESH ;;
    esac
    printf '{"event":"init","conversation_id":"conversation-e2e","init":{"model":"gemini-3.1-pro-low","permission_mode":"request-review"}}\n'
    printf '{"event":"result","result":{"conversation_id":"conversation-e2e","status":"SUCCESS","response":"%s","usage":{"input_tokens":10,"output_tokens":2,"thinking_tokens":3,"cache_read_tokens":4,"total_tokens":19}}}\n' "$response"
    ;;
esac
"#,
    )?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn exec_resumes_the_provider_conversation_across_processes() -> Result<()> {
    let fixture = TempDir::new()?;
    let chaos_home = fixture.path().join("chaos");
    let agy_home = fixture.path().join("antigravity");
    let agy_path = fixture.path().join("agy");
    write_fake_agy(&agy_path)?;
    fs::create_dir_all(&chaos_home)?;
    fs::create_dir_all(&agy_home)?;

    let mut fresh = common::chaos_command(&chaos_home)?;
    let fresh = fresh
        .args([
            "exec",
            "--json",
            "--skip-git-repo-check",
            "-c",
            "clamp=true",
            "-c",
            "clamp_backend=antigravity",
            "-m",
            "gemini-3.1-pro-preview",
            "start",
        ])
        .env("CHAOS_AGY_PATH", &agy_path)
        .env("CHAOS_AGY_HOME", &agy_home)
        .output()?;
    assert!(
        fresh.status.success(),
        "fresh exec failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&fresh.stdout),
        String::from_utf8_lossy(&fresh.stderr)
    );
    let fresh_events = parse_jsonl(&fresh.stdout)?;
    let process_id = fresh_events
        .iter()
        .find(|event| event["type"] == "process.started")
        .and_then(|event| event["process_id"].as_str())
        .expect("fresh process id");
    assert_agent_message(&fresh_events, "CHAOS_AGY_FRESH");

    let state_path = agy_home
        .join(".chaos-conversations")
        .join(format!("{process_id}.json"));
    assert!(state_path.is_file());

    let mut resumed = common::chaos_command(&chaos_home)?;
    let resumed = resumed
        .args([
            "exec",
            "--json",
            "--skip-git-repo-check",
            "-c",
            "clamp=true",
            "-c",
            "clamp_backend=antigravity",
            "-m",
            "gemini-3.1-pro-preview",
            "resume",
            process_id,
            "continue",
        ])
        .env("CHAOS_AGY_PATH", &agy_path)
        .env("CHAOS_AGY_HOME", &agy_home)
        .output()?;
    assert!(
        resumed.status.success(),
        "resumed exec failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&resumed.stdout),
        String::from_utf8_lossy(&resumed.stderr)
    );
    let resumed_events = parse_jsonl(&resumed.stdout)?;
    assert_agent_message(&resumed_events, "CHAOS_AGY_RESUMED");
    assert!(
        resumed_events
            .iter()
            .all(|event| event["type"] != "turn.failed")
    );
    Ok(())
}

fn parse_jsonl(output: &[u8]) -> Result<Vec<JsonValue>> {
    String::from_utf8(output.to_vec())?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}

fn assert_agent_message(events: &[JsonValue], expected: &str) {
    assert!(events.iter().any(|event| {
        event["type"] == "item.completed"
            && event["item"]["type"] == "agent_message"
            && event["item"]["text"] == expected
    }));
}
