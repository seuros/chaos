use super::*;
use chaos_ipc::ProcessId;
use tempfile::TempDir;

#[tokio::test]
async fn find_process_path_by_name_uses_index_lookup() -> std::io::Result<()> {
    let temp = TempDir::new()?;
    let process_id = ProcessId::new();
    let rollout_path = temp.path().join(format!(
        "sessions/2025/01/03/rollout-2025-01-03T00-00-00-{process_id}.jsonl"
    ));
    std::fs::create_dir_all(rollout_path.parent().expect("rollout path has parent"))?;
    std::fs::write(&rollout_path, "")?;
    append_process_name(temp.path(), process_id, "named-thread").await?;

    let found = find_process_path_by_name_str(temp.path(), "named-thread").await?;
    assert_eq!(found, Some(rollout_path));
    Ok(())
}
