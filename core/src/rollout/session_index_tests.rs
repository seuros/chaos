use super::*;
use codex_protocol::ThreadId;
use tempfile::TempDir;

#[tokio::test]
async fn find_thread_path_by_name_uses_index_lookup() -> std::io::Result<()> {
    let temp = TempDir::new()?;
    let thread_id = ThreadId::new();
    let rollout_path = temp.path().join(format!(
        "sessions/2025/01/03/rollout-2025-01-03T00-00-00-{thread_id}.jsonl"
    ));
    std::fs::create_dir_all(rollout_path.parent().expect("rollout path has parent"))?;
    std::fs::write(&rollout_path, "")?;
    append_thread_name(temp.path(), thread_id, "named-thread").await?;

    let found = find_thread_path_by_name_str(temp.path(), "named-thread").await?;
    assert_eq!(found, Some(rollout_path));
    Ok(())
}
