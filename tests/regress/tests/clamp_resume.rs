//! Cross-process clamp wiring regression. The default test uses no provider or
//! credentials. The opt-in test exercises the same assertions against Claude.
#![cfg(unix)]

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

#[tokio::test]
async fn claude_resume_preserves_tools_across_exec_processes() -> Result<()> {
    tool_continuity(false).await
}

/// Requires an authenticated Claude CLI; never part of the default CI gate.
/// CHAOS_CLAMP_SMOKE=1 cargo test -p chaos-regress --test clamp_resume -- --ignored
#[tokio::test]
#[ignore = "requires local Claude Code CLI and authenticated environment"]
async fn live_claude_resume_preserves_tools_across_exec_processes() -> Result<()> {
    ensure!(
        std::env::var_os("CHAOS_CLAMP_SMOKE").is_some(),
        "set CHAOS_CLAMP_SMOKE=1"
    );
    tool_continuity(true).await
}

async fn tool_continuity(live: bool) -> Result<()> {
    // Short paths also fit macOS's Unix socket path limit under nextest's TMPDIR.
    let root = tempfile::Builder::new()
        .prefix("clamp-")
        .tempdir_in("/tmp")?;
    let home = root.path().join("home");
    let work = root.path().join("work");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&work)?;
    // An authoritative catalog prevents even background provider discovery in CI.
    let catalog_path = home.join("models.json");
    let mut model = chaos_kern::test_support::test_model_info("claude-haiku-4-5");
    model.shell_type = chaos_ipc::openai_models::ConfigShellToolType::UnifiedExec;
    model.experimental_supported_tools = vec!["read_file".to_string()];
    std::fs::write(
        &catalog_path,
        serde_json::to_vec(&serde_json::json!({"models":[model]}))?,
    )?;
    let peer_dir = root.path().join("bin");
    std::fs::create_dir(&peer_dir)?;
    std::os::unix::fs::symlink(
        env!("CARGO_BIN_EXE_clamp-test-peer"),
        peer_dir.join("claude"),
    )?;
    let path = std::env::join_paths([peer_dir].into_iter().chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))?;
    let binary = chaos_which::cargo_bin("chaos")?;
    let socket = home.join("run/journald.sock");
    let mut daemon = Command::new(chaos_which::cargo_bin("chaos_journald")?)
        .args(["--socket"])
        .arg(&socket)
        .args(["--db"])
        .arg(home.join("journal.sqlite"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let result = async {
        // Probe protocol readiness, not a fixed startup sleep. Own and reap this daemon.
        let client = chaos_journald::JournalRpcClient::new(socket);
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            loop {
                tick.tick().await;
                if client.hello("clamp-regression").await.is_ok() {
                    return Ok::<_, anyhow::Error>(());
                }
                ensure!(daemon.try_wait()?.is_none(), "journald exited before readiness");
            }
        }).await.context("journald readiness timed out")??;
        let first = marker(root.path(), "first-")?;
        let second = marker(root.path(), "second-")?;
        let changed = marker(root.path(), "changed-")?;
        std::fs::write(work.join("first.txt"), &first)?;
        let mut process = None;
        let mut native = None;
        for turn in 1..=3 {
            if turn == 2 {
                std::fs::remove_file(work.join("first.txt"))?;
                std::fs::write(work.join("second.txt"), &second)?;
            } else if turn == 3 {
                std::fs::write(work.join("second.txt"), &changed)?;
            }
            let file = work.join(if turn == 1 { "first.txt" } else { "second.txt" });
            let finish = if turn == 1 {
                "Remember the exact file marker, but do not print it. Finish with STAGE_ONE_OK."
            } else {
                "first.txt was deleted. Recall its exact marker from your prior tool result, without re-reading it. Print that marker and the newly read second.txt marker."
            };
            let prompt = format!(
                "STAGE_{turn}: Scoped tool continuity test. Use only Chaos MCP tools. \
                 Read {} with read_file (fresh read, it may have changed). \
                 Use exec_command with shell /bin/sh and login=false in {} \
                 to run exactly: printf 'turn{turn}\\n' >> audit.txt\n\
                 Then read {}/audit.txt with read_file. {finish} \
                 Do not access or modify anything outside this work directory.",
                file.display(), work.display(), work.display()
            );
            let mut command = Command::new(&binary);
            command.current_dir(&work)
                .env("CHAOS_HOME", &home)
                .env_remove("CHAOS_STORAGE_URL").env_remove("CHAOS_SQLITE_HOME")
                .env_remove("CHAOS_JOURNALD_SOCKET").env_remove("CLAUDECODE")
                .args(["exec", "--json", if live { "--full-auto" } else { "--headless" }, "--skip-git-repo-check",
                    "-c", "clamp=true", "-c", "machine_warnings.enabled=false",
                    "-c", "disable_user_scripts=true", "-m", "claude-haiku-4-5"])
                .kill_on_drop(true);
            if !live {
                command.env("PATH", &path).env("CLAMP_TEST_ROOT", root.path())
                    .args(["-c", &format!("model_catalog_json={}", catalog_path.display())]);
            }
            if let Some(id) = &process {
                command.arg("resume").arg(id);
            }
            command.arg(prompt);
            let output = tokio::time::timeout(
                Duration::from_secs(if live { 180 } else { 30 }), command.output()
            ).await.context("exec timed out")??;
            ensure!(output.status.success(), "turn {turn} failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            let events: Vec<Value> = String::from_utf8(output.stdout)?.lines()
                .map(serde_json::from_str).collect::<std::result::Result<_, _>>()?;
            if process.is_none() {
                process = Some(events.iter().find(|e| e["type"] == "process.started")
                    .and_then(|e| e["process_id"].as_str()).context("process ID")?.to_string());
            }
            let id = process.as_deref().context("process ID")?;
            let checkpoint: Value = serde_json::from_slice(&std::fs::read(
                home.join("clamp/claude").join(format!("{id}.json"))
            )?)?;
            let current = checkpoint["session_id"].as_str().context("native ID")?.to_string();
            if let Some(expected) = &native {
                ensure!(expected == &current, "native session changed across tool-use restart");
            }
            native = Some(current);
            let answer = assistant_text(&events);
            if turn == 1 {
                ensure!(answer.contains("STAGE_ONE_OK") && !answer.contains(&first),
                    "first marker must remain tool-only: {answer}");
            } else {
                ensure!(answer.contains(&first), "lost prior tool-only result: {answer}");
                ensure!(answer.contains(if turn == 2 { &second } else { &changed }),
                    "missing fresh read: {answer}");
            }
            let expected: String = (1..=turn).map(|i| format!("turn{i}\n")).collect();
            ensure!(std::fs::read_to_string(work.join("audit.txt"))? == expected,
                "missing or replayed side effect on turn {turn}");
            if live {
                verify_native_tools(native.as_deref().context("native ID")?, turn)?;
            }
        }
        Ok(())
    }.await;
    daemon.kill().await?;
    daemon.wait().await?;
    result
}

fn assistant_text(events: &[Value]) -> String {
    events
        .iter()
        .filter(|e| e["type"] == "item.completed" && e["item"]["type"] == "agent_message")
        .filter_map(|e| e["item"]["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn marker(root: &Path, prefix: &str) -> Result<String> {
    let file = tempfile::Builder::new().prefix(prefix).tempfile_in(root)?;
    Ok(file
        .path()
        .file_name()
        .context("marker filename")?
        .to_string_lossy()
        .to_string())
}

/// Read only this test's exact native session, never neighboring conversations.
fn verify_native_tools(session: &str, turn: u64) -> Result<()> {
    let home = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|path| path.join(".claude")))
        .context("Claude home")?;
    let mut calls = HashSet::new();
    let mut results = HashSet::new();
    let mut reads = 0;
    let mut writes = 0;
    let mut found = false;
    for project in std::fs::read_dir(home.join("projects"))? {
        let path = project?.path().join(format!("{session}.jsonl"));
        if !path.is_file() {
            continue;
        }
        found = true;
        for line in std::fs::read_to_string(path)?.lines() {
            let row: Value = serde_json::from_str(line)?;
            let Some(blocks) = row["message"]["content"].as_array() else {
                continue;
            };
            for block in blocks {
                match block["type"].as_str() {
                    Some("tool_use") => {
                        let name = block["name"].as_str().context("tool name")?;
                        ensure!(name.starts_with("mcp__chaos__"), "non-Chaos tool used");
                        reads += u64::from(name == "mcp__chaos__read_file");
                        writes += u64::from(name == "mcp__chaos__exec_command");
                        calls.insert(block["id"].as_str().context("tool ID")?.to_string());
                    }
                    Some("tool_result") => {
                        ensure!(block["is_error"] != true, "native tool result failed");
                        results.insert(
                            block["tool_use_id"]
                                .as_str()
                                .context("result ID")?
                                .to_string(),
                        );
                    }
                    _ => {}
                }
            }
        }
    }
    ensure!(
        found && reads >= 2 * turn && writes >= turn,
        "missing native tool calls"
    );
    ensure!(calls.is_subset(&results), "unmatched native tool results");
    Ok(())
}
