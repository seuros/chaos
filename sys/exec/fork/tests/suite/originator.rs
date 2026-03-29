#![allow(clippy::expect_used, clippy::unwrap_used)]

use anyhow::Context;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::RolloutLine;
use chaos_ipc::protocol::SessionMetaLine;
use chaos_kern::default_client::CODEX_INTERNAL_ORIGINATOR_OVERRIDE_ENV_VAR;
use core_test_support::responses;
use core_test_support::test_chaos_fork::test_chaos_fork;
use walkdir::WalkDir;

fn load_session_meta(home_path: &std::path::Path) -> anyhow::Result<SessionMetaLine> {
    let rollout_path = WalkDir::new(home_path.join("sessions"))
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| {
            entry.file_type().is_file() && entry.file_name().to_string_lossy().ends_with(".jsonl")
        })
        .map(walkdir::DirEntry::into_path)
        .context("session rollout file not found")?;
    let first_line = std::fs::read_to_string(&rollout_path)?
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .context("session rollout file was empty")?;
    let rollout_line: RolloutLine = serde_json::from_str(&first_line)?;
    let RolloutItem::SessionMeta(meta) = rollout_line.item else {
        anyhow::bail!(
            "expected first rollout item to be session meta in {}",
            rollout_path.display()
        );
    };
    Ok(meta)
}

/// Verify that `chaos exec` persists the default fork originator in session metadata.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_chaos_fork_originator() -> anyhow::Result<()> {
    let test = test_chaos_fork();

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("response_1"),
        responses::ev_assistant_message("response_1", "Hello, world!"),
        responses::ev_completed("response_1"),
    ]);
    let response = responses::mount_sse_once(&server, body).await;

    test.cmd_with_server(&server)
        .env_remove(CODEX_INTERNAL_ORIGINATOR_OVERRIDE_ENV_VAR)
        .arg("--skip-git-repo-check")
        .arg("tell me something")
        .assert()
        .code(0);
    let _ = response.single_request();
    let meta = load_session_meta(test.home_path())?;
    assert_eq!(meta.meta.originator, "chaos_fork");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supports_originator_override() -> anyhow::Result<()> {
    let test = test_chaos_fork();

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("response_1"),
        responses::ev_assistant_message("response_1", "Hello, world!"),
        responses::ev_completed("response_1"),
    ]);
    let response = responses::mount_sse_once(&server, body).await;

    test.cmd_with_server(&server)
        .env("CODEX_INTERNAL_ORIGINATOR_OVERRIDE", "chaos_fork_override")
        .arg("--skip-git-repo-check")
        .arg("tell me something")
        .assert()
        .code(0);
    let _ = response.single_request();
    let meta = load_session_meta(test.home_path())?;
    assert_eq!(meta.meta.originator, "chaos_fork_override");

    Ok(())
}
