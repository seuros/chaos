use std::collections::HashMap;
use std::collections::HashSet;
use std::io::ErrorKind;

use chaos_ipc::ProcessId;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteRow;

use super::ProcessNameRecord;
use super::ProcessNameSource;
use super::ProcessNameStore;
use super::decode_optional_process_id;
use super::decode_process_name;
use super::decode_process_names;

#[test]
fn legacy_or_unknown_process_name_sources_are_user_pinned() {
    assert_eq!(ProcessNameSource::from_db(None), ProcessNameSource::User);
    assert_eq!(
        ProcessNameSource::from_db(Some("unknown")),
        ProcessNameSource::User
    );
    assert_eq!(
        ProcessNameSource::from_db(Some("agent")),
        ProcessNameSource::Agent
    );
}

#[tokio::test]
async fn decode_process_name_preserves_names_and_defaults_sources() -> std::io::Result<()> {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    assert_eq!(decode_process_name::<SqliteRow>(None)?, None);

    for (source, expected) in [
        (None, ProcessNameSource::User),
        (Some("unknown"), ProcessNameSource::User),
        (Some("user"), ProcessNameSource::User),
        (Some("agent"), ProcessNameSource::Agent),
    ] {
        let row = sqlx::query("SELECT ? AS process_name, ? AS process_name_source")
            .bind("  Keep original spacing  ")
            .bind(source)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            decode_process_name(Some(row))?,
            Some(ProcessNameRecord {
                name: "  Keep original spacing  ".to_owned(),
                source: expected,
            })
        );
    }
    Ok(())
}

#[tokio::test]
async fn decode_process_name_preserves_sqlx_errors() {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    let row = sqlx::query("SELECT 'name' AS process_name")
        .fetch_one(&pool)
        .await
        .unwrap();
    let error = decode_process_name(Some(row)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(error.get_ref().unwrap().is::<sqlx::Error>());
}

#[tokio::test]
async fn decode_process_names_builds_map_and_last_duplicate_wins() -> std::io::Result<()> {
    assert!(decode_process_names::<SqliteRow>(Vec::new())?.is_empty());
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    let first = ProcessId::new();
    let second = ProcessId::new();
    let mut rows = Vec::new();
    for (id, name) in [
        (first, "original"),
        (second, "  second  "),
        (first, "replacement"),
    ] {
        rows.push(
            sqlx::query("SELECT ? AS id, ? AS process_name")
                .bind(id.to_string())
                .bind(name)
                .fetch_one(&pool)
                .await
                .unwrap(),
        );
    }
    assert_eq!(
        decode_process_names(rows)?,
        HashMap::from([
            (first, "replacement".to_owned()),
            (second, "  second  ".to_owned()),
        ])
    );
    Ok(())
}

#[tokio::test]
async fn decode_process_names_preserves_error_precedence() {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    let invalid_id = sqlx::query("SELECT 'invalid-id' AS id, 'name' AS process_name")
        .fetch_one(&pool)
        .await
        .unwrap();
    let missing_name = sqlx::query("SELECT 'invalid-id' AS id")
        .fetch_one(&pool)
        .await
        .unwrap();

    // Parse an earlier row's ID before decoding any later row.
    let error = decode_process_names(vec![invalid_id, missing_name]).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(error.get_ref().unwrap().is::<uuid::Error>());

    // Within one row, decode the name before trying to parse the ID.
    let missing_name = sqlx::query("SELECT 'invalid-id' AS id")
        .fetch_one(&pool)
        .await
        .unwrap();
    let error = decode_process_names(vec![missing_name]).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(error.get_ref().unwrap().is::<sqlx::Error>());
}

#[tokio::test]
async fn decode_optional_process_id_handles_absent_valid_and_invalid_rows() -> std::io::Result<()> {
    assert_eq!(decode_optional_process_id::<SqliteRow>(None)?, None);
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    let id = ProcessId::new();
    let row = sqlx::query("SELECT ? AS id")
        .bind(id.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(decode_optional_process_id(Some(row))?, Some(id));

    let row = sqlx::query("SELECT 'invalid-id' AS id")
        .fetch_one(&pool)
        .await
        .unwrap();
    let error = decode_optional_process_id(Some(row)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(error.get_ref().unwrap().is::<uuid::Error>());

    let row = sqlx::query("SELECT 'missing-id' AS other_column")
        .fetch_one(&pool)
        .await
        .unwrap();
    let error = decode_optional_process_id(Some(row)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(error.get_ref().unwrap().is::<sqlx::Error>());
    Ok(())
}

#[tokio::test]
async fn sqlite_empty_inputs_keep_existing_short_circuits() -> std::io::Result<()> {
    // No processes table: these calls must return before querying it.
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    assert!(pool.get_process_names(&HashSet::new()).await?.is_empty());
    assert_eq!(pool.find_process_id_by_name(" \t ").await?, None);
    // Unlike the general lookup, the unarchived lookup has no empty-name guard.
    assert!(pool.find_unarchived_process_id_by_name("").await.is_err());
    Ok(())
}

#[tokio::test]
async fn sqlite_store_keeps_name_filters_and_lookup_ordering() -> std::io::Result<()> {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE processes (
            id TEXT PRIMARY KEY,
            process_name TEXT,
            process_name_source TEXT,
            updated_at INTEGER,
            created_at INTEGER,
            archived_at INTEGER
        )",
    )
    .execute(&pool)
    .await
    .unwrap();

    let older = ProcessId::new();
    let newer = ProcessId::new();
    let latest_created = ProcessId::new();
    let archived = ProcessId::new();
    let blank = ProcessId::new();
    let unnamed = ProcessId::new();
    for (id, name, source, updated, created, archived_at) in [
        (older, Some("same"), None, 10, 1, None),
        (newer, Some("same"), Some("user"), 20, 1, None),
        (latest_created, Some("same"), Some("agent"), 20, 2, None),
        (archived, Some("same"), None, 30, 1, Some(1)),
        (blank, Some("   "), None, 40, 1, None),
        (unnamed, None, None, 40, 1, None),
    ] {
        sqlx::query("INSERT INTO processes VALUES (?, ?, ?, ?, ?, ?)")
            .bind(id.to_string())
            .bind(name)
            .bind(source)
            .bind(updated)
            .bind(created)
            .bind(archived_at)
            .execute(&pool)
            .await
            .unwrap();
    }

    assert_eq!(
        pool.get_process_name(older).await?,
        Some(ProcessNameRecord {
            name: "same".to_owned(),
            source: ProcessNameSource::User,
        })
    );
    assert_eq!(
        pool.get_process_name(latest_created).await?,
        Some(ProcessNameRecord {
            name: "same".to_owned(),
            source: ProcessNameSource::Agent,
        })
    );
    for id in [blank, unnamed, ProcessId::new()] {
        assert_eq!(pool.get_process_name(id).await?, None);
    }
    assert_eq!(
        pool.get_process_names(&HashSet::from([older, blank, unnamed]))
            .await?,
        HashMap::from([(older, "same".to_owned())])
    );
    assert_eq!(pool.find_process_id_by_name("same").await?, Some(archived));
    assert_eq!(
        pool.find_unarchived_process_id_by_name("same").await?,
        Some(latest_created)
    );
    assert_eq!(pool.find_process_id_by_name("missing").await?, None);
    assert_eq!(
        pool.find_unarchived_process_id_by_name("missing").await?,
        None
    );
    Ok(())
}
