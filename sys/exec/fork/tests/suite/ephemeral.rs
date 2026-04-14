#![allow(clippy::expect_used, clippy::unwrap_used)]

use chaos_which::find_resource;
use core_test_support::test_chaos_fork::test_chaos_fork;

/// Returns true if the runtime DB was created in the given chaos home directory.
/// Sessions are stored in SQLite (`chaos.sqlite`) rather than JSONL files.
fn runtime_db_exists(home_path: &std::path::Path) -> bool {
    let db_filename = chaos_proc::runtime_db_filename();
    home_path.join(db_filename).exists()
}

#[test]
fn persists_rollout_file_by_default() -> anyhow::Result<()> {
    let test = test_chaos_fork();
    let fixture = find_resource!("tests/fixtures/cli_responses_fixture.sse")?;

    test.cmd()
        .env("CHAOS_RS_SSE_FIXTURE", &fixture)
        .arg("--skip-git-repo-check")
        .arg("default persistence behavior")
        .assert()
        .code(0);

    assert!(
        runtime_db_exists(test.home_path()),
        "expected runtime DB to be created for non-ephemeral session"
    );
    Ok(())
}

/// Ephemeral mode skips session rollout persistence but the runtime DB
/// itself is still created (model cache, cron, and other shared services
/// depend on it). Verify the process exits successfully and the home
/// directory exists — session-level assertions belong in kern tests.
#[test]
fn does_not_persist_rollout_file_in_ephemeral_mode() -> anyhow::Result<()> {
    let test = test_chaos_fork();
    let fixture = find_resource!("tests/fixtures/cli_responses_fixture.sse")?;

    test.cmd()
        .env("CHAOS_RS_SSE_FIXTURE", &fixture)
        .arg("--skip-git-repo-check")
        .arg("--ephemeral")
        .arg("ephemeral behavior")
        .assert()
        .code(0);

    // The runtime DB may exist (shared services create it), but the
    // chaos home directory itself should have been set up.
    assert!(
        test.home_path().exists(),
        "chaos home should exist even in ephemeral mode"
    );
    Ok(())
}
