use super::DIM;
use super::PgRecallStore;
use crate::store::RecallDoc;
use crate::store::RecallStore;
use crate::store::SearchRequest;
use chaos_vfs::ChaosVfs;
use chaos_vfs::MountConfig;

const TEST_DATABASE_URL_ENV: &str = "TEST_DATABASE_URL";

fn unit_vector(seed: f32) -> Vec<f32> {
    let mut embedding = vec![0.0_f32; DIM as usize];
    embedding[0] = seed;
    embedding
}

#[tokio::test]
async fn from_vfs_rejects_a_sqlite_mount() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let vfs = ChaosVfs::from_config(MountConfig::sqlite_home(temp_dir.path()))
        .await
        .expect("open sqlite home");

    let err = PgRecallStore::from_pool(vfs.pool()).expect_err("sqlite has no pgvector to offer");
    assert!(
        err.to_string().contains("postgres"),
        "a sqlite mount should say recall needs postgres, got {err}"
    );
}

#[tokio::test]
async fn postgres_mount_indexes_and_searches_a_document() {
    let Ok(database_url) = std::env::var(TEST_DATABASE_URL_ENV) else {
        eprintln!("skipping recall validation; {TEST_DATABASE_URL_ENV} is not set");
        return;
    };

    let config = MountConfig::postgres_url(database_url);
    let vfs = ChaosVfs::from_config(config.clone())
        .await
        .expect("open postgres mount");
    chaos_vfs::set_root(chaos_vfs::mount(config, vfs));

    let store = PgRecallStore::from_vfs().expect("a postgres mount serves recall");
    store.migrate().await.expect("migrate recall schema");

    store
        .index(RecallDoc {
            id: "recall-round-trip".to_string(),
            content: "the kernel mounts one backend at boot".to_string(),
            embedding: unit_vector(1.0),
            metadata: serde_json::json!({"source": "test"}),
        })
        .await
        .expect("index document");

    let results = store
        .search(&SearchRequest::new(unit_vector(1.0), 5))
        .await
        .expect("search recall docs");
    let found = results
        .iter()
        .find(|result| result.id == "recall-round-trip")
        .expect("the indexed document should come back");
    assert_eq!(found.content, "the kernel mounts one backend at boot");
    assert!(found.score > 0.9, "an identical vector should score high");

    store
        .delete("recall-round-trip")
        .await
        .expect("delete recall doc");
}
