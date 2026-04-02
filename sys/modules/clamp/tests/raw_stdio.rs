//! Raw test: spawn claude with stream-json, dump everything from stdout/stderr
//! to see what it actually sends.
//!
//! Run with:
//!   CHAOS_CLAMP_SMOKE=1 cargo test -p chaos-clamp --test raw_stdio -- --ignored --nocapture

use std::process::Stdio;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Command;

#[ignore = "requires local Claude Code CLI and authenticated environment"]
#[tokio::test]
async fn dump_claude_stream_json() {
    if std::env::var_os("CHAOS_CLAMP_SMOKE").is_none() {
        eprintln!("skipping clamp raw stdio test; set CHAOS_CLAMP_SMOKE=1 to enable");
        return;
    }

    let mut child = Command::new("claude")
        .args(["--output-format", "stream-json"])
        .args(["--input-format", "stream-json"])
        .arg("--verbose")
        .args(["--system-prompt", ""])
        .args(["--setting-sources", ""])
        .args(["--permission-mode", "default"])
        .env("CLAUDE_CODE_ENTRYPOINT", "sdk-chaos")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn claude");

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();

    // Read stdout in background
    let stdout_task = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut count = 0;
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("[STDOUT:{count}] {line}");
            count += 1;
            if count > 50 {
                eprintln!("[STDOUT] stopping after 50 lines");
                break;
            }
        }
    });

    // Read stderr in background
    let stderr_task = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        let mut count = 0;
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("[STDERR:{count}] {line}");
            count += 1;
            if count > 30 {
                break;
            }
        }
    });

    // Wait a bit for claude to start
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    eprintln!("[TEST] sending initialize control request...");

    // Send initialize (matching the SDK exactly)
    let init_msg = serde_json::json!({
        "type": "control_request",
        "request_id": "req_1",
        "request": {
            "subtype": "initialize",
            "hooks": null
        }
    });
    let line = serde_json::to_string(&init_msg).unwrap() + "\n";
    stdin.write_all(line.as_bytes()).await.unwrap();
    stdin.flush().await.unwrap();
    eprintln!("[TEST] sent: {}", line.trim());

    // Wait for response
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    eprintln!("[TEST] sending user message...");
    let user_msg = serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": "Say: OK"},
        "parent_tool_use_id": null,
        "session_id": "default"
    });
    let line = serde_json::to_string(&user_msg).unwrap() + "\n";
    stdin.write_all(line.as_bytes()).await.unwrap();
    stdin.flush().await.unwrap();
    eprintln!("[TEST] sent user message");

    // Wait for response
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;

    eprintln!("[TEST] closing stdin...");
    drop(stdin);

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let _ = child.kill().await;
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    eprintln!("[TEST] done");
}
