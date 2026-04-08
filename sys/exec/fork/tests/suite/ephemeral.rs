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
        .env("CODEX_RS_SSE_FIXTURE", &fixture)
        .env("OPENAI_BASE_URL", "http://unused.local")
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

#[test]
fn does_not_persist_rollout_file_in_ephemeral_mode() -> anyhow::Result<()> {
    let test = test_chaos_fork();
    let fixture = find_resource!("tests/fixtures/cli_responses_fixture.sse")?;

    test.cmd()
        .env("CODEX_RS_SSE_FIXTURE", &fixture)
        .env("OPENAI_BASE_URL", "http://unused.local")
        .arg("--skip-git-repo-check")
        .arg("--ephemeral")
        .arg("ephemeral behavior")
        .assert()
        .code(0);

    assert!(
        !runtime_db_exists(test.home_path()),
        "expected no runtime DB for ephemeral session"
    );
    Ok(())
}
