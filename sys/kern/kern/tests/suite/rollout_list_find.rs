#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use chrono::Utc;
use chaos_kern::EventPersistenceMode;
use chaos_kern::RolloutRecorder;
use chaos_kern::RolloutRecorderParams;
use chaos_kern::config::ConfigBuilder;
use chaos_kern::find_archived_process_path_by_id_str;
use chaos_kern::find_process_path_by_id_str;
use chaos_kern::find_process_path_by_name_str;
use chaos_ipc::ProcessId;
use chaos_ipc::models::BaseInstructions;
use chaos_ipc::protocol::SessionSource;
use chaos_proc::StateRuntime;
use chaos_proc::ProcessMetadataBuilder;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use uuid::Uuid;

/// Create <subdir>/YYYY/MM/DD and write a minimal rollout file containing the
/// provided conversation id in the SessionMeta line. Returns the absolute path.
fn write_minimal_rollout_with_id_in_subdir(codex_home: &Path, subdir: &str, id: Uuid) -> PathBuf {
    let sessions = codex_home.join(subdir).join("2024/01/01");
    std::fs::create_dir_all(&sessions).unwrap();

    let file = sessions.join(format!("rollout-2024-01-01T00-00-00-{id}.jsonl"));
    let mut f = std::fs::File::create(&file).unwrap();
    // Minimal first line: session_meta with the id so content search can find it
    writeln!(
        f,
        "{}",
        serde_json::json!({
            "timestamp": "2024-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "id": id,
                "timestamp": "2024-01-01T00:00:00Z",
                "cwd": ".",
                "originator": "test",
                "cli_version": "test",
                "model_provider": "test-provider"
            }
        })
    )
    .unwrap();

    file
}

/// Create sessions/YYYY/MM/DD and write a minimal rollout file containing the
/// provided conversation id in the SessionMeta line. Returns the absolute path.
fn write_minimal_rollout_with_id(codex_home: &Path, id: Uuid) -> PathBuf {
    write_minimal_rollout_with_id_in_subdir(codex_home, "sessions", id)
}

async fn upsert_process_metadata(codex_home: &Path, process_id: ProcessId, rollout_path: PathBuf) {
    let runtime = StateRuntime::init(codex_home.to_path_buf(), "test-provider".to_string())
        .await
        .unwrap();
    runtime.mark_backfill_complete(None).await.unwrap();
    let mut builder = ProcessMetadataBuilder::new(
        process_id,
        rollout_path,
        Utc::now(),
        SessionSource::default(),
    );
    builder.cwd = codex_home.to_path_buf();
    let metadata = builder.build("test-provider");
    runtime.upsert_process(&metadata).await.unwrap();
}

#[tokio::test]
async fn find_locates_rollout_file_by_id() {
    let home = TempDir::new().unwrap();
    let id = Uuid::new_v4();
    let expected = write_minimal_rollout_with_id(home.path(), id);

    let found = find_process_path_by_id_str(home.path(), &id.to_string())
        .await
        .unwrap();

    assert_eq!(found.unwrap(), expected);
}

#[tokio::test]
async fn find_handles_gitignore_covering_codex_home_directory() {
    let repo = TempDir::new().unwrap();
    let codex_home = repo.path().join(".codex");
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::write(repo.path().join(".gitignore"), ".codex/**\n").unwrap();
    let id = Uuid::new_v4();
    let expected = write_minimal_rollout_with_id(&codex_home, id);

    let found = find_process_path_by_id_str(&codex_home, &id.to_string())
        .await
        .unwrap();

    assert_eq!(found, Some(expected));
}

#[tokio::test]
async fn find_prefers_sqlite_path_by_id() {
    let home = TempDir::new().unwrap();
    let id = Uuid::new_v4();
    let process_id = ProcessId::from_string(&id.to_string()).unwrap();
    let db_path = home.path().join(format!(
        "sessions/2030/12/30/rollout-2030-12-30T00-00-00-{id}.jsonl"
    ));
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    std::fs::write(&db_path, "").unwrap();
    write_minimal_rollout_with_id(home.path(), id);
    upsert_process_metadata(home.path(), process_id, db_path.clone()).await;

    let found = find_process_path_by_id_str(home.path(), &id.to_string())
        .await
        .unwrap();

    assert_eq!(found, Some(db_path));
}

#[tokio::test]
async fn find_falls_back_to_filesystem_when_sqlite_has_no_match() {
    let home = TempDir::new().unwrap();
    let id = Uuid::new_v4();
    let expected = write_minimal_rollout_with_id(home.path(), id);
    let unrelated_id = Uuid::new_v4();
    let unrelated_process_id = ProcessId::from_string(&unrelated_id.to_string()).unwrap();
    let unrelated_path = home
        .path()
        .join("sessions/2030/12/30/rollout-2030-12-30T00-00-00-unrelated.jsonl");
    upsert_process_metadata(home.path(), unrelated_process_id, unrelated_path).await;

    let found = find_process_path_by_id_str(home.path(), &id.to_string())
        .await
        .unwrap();

    assert_eq!(found, Some(expected));
}

#[tokio::test]
async fn find_ignores_granular_gitignore_rules() {
    let home = TempDir::new().unwrap();
    let id = Uuid::new_v4();
    let expected = write_minimal_rollout_with_id(home.path(), id);
    std::fs::write(home.path().join("sessions/.gitignore"), "*.jsonl\n").unwrap();

    let found = find_process_path_by_id_str(home.path(), &id.to_string())
        .await
        .unwrap();

    assert_eq!(found, Some(expected));
}

#[tokio::test]
async fn find_locates_rollout_file_written_by_recorder() -> std::io::Result<()> {
    // Ensures the name-based finder locates a rollout produced by the real recorder.
    let home = TempDir::new().unwrap();
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let process_id = ProcessId::new();
    let process_name = "named process";
    let recorder = RolloutRecorder::new(
        &config,
        RolloutRecorderParams::new(
            process_id,
            None,
            SessionSource::Exec,
            BaseInstructions::default(),
            Vec::new(),
            EventPersistenceMode::Limited,
        ),
        None,
        None,
    )
    .await?;
    recorder.persist().await?;
    recorder.flush().await?;

    let index_path = home.path().join("session_index.jsonl");
    std::fs::write(
        &index_path,
        format!(
            "{}\n",
            serde_json::json!({
                "id": process_id,
                "process_name": process_name,
                "updated_at": "2024-01-01T00:00:00Z"
            })
        ),
    )?;

    let found = find_process_path_by_name_str(home.path(), process_name).await?;

    let path = found.expect("expected rollout path to be found");
    assert!(path.exists());
    let contents = std::fs::read_to_string(&path)?;
    assert!(contents.contains(&process_id.to_string()));
    recorder.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn find_archived_locates_rollout_file_by_id() {
    let home = TempDir::new().unwrap();
    let id = Uuid::new_v4();
    let expected = write_minimal_rollout_with_id_in_subdir(home.path(), "archived_sessions", id);

    let found = find_archived_process_path_by_id_str(home.path(), &id.to_string())
        .await
        .unwrap();

    assert_eq!(found, Some(expected));
}
