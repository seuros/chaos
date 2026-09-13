use chaos_ipc::ProcessId;
use chaos_ipc::protocol::CompactedItem;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionSource;

use crate::AppendBatchInput;
use crate::CreateProcessInput;
use crate::InitializeProcessInput;
use crate::JournalClient;
use crate::JournalEntry;

const TEST_DATABASE_URL_ENV: &str = "TEST_DATABASE_URL";

fn postgres_test_url() -> Option<String> {
    std::env::var(TEST_DATABASE_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn compacted(message: &str) -> RolloutItem {
    RolloutItem::Compacted(CompactedItem {
        message: message.to_string(),
        replacement_history: None,
    })
}

#[tokio::test]
async fn postgres_client_supports_complete_journal_lifecycle() {
    let Some(database_url) = postgres_test_url() else {
        eprintln!("skipping PostgreSQL journal validation; {TEST_DATABASE_URL_ENV} is not set");
        return;
    };
    let pool = chaos_proc::open_runtime_db_postgres_url(&database_url)
        .await
        .expect("open PostgreSQL runtime database");
    let client = JournalClient::postgres_pool(pool.clone());
    assert!(matches!(&client, JournalClient::Postgres(_)));

    let process_id = ProcessId::new();
    let first_item = compacted("first");
    let initialized = client
        .initialize_process(InitializeProcessInput {
            create: CreateProcessInput {
                process_id,
                parent: None,
                source: SessionSource::Cli,
                cwd: std::env::temp_dir().join(process_id.to_string()),
                created_at: jiff::Timestamp::now(),
                title: Some("PostgreSQL journal lifecycle".to_string()),
                model_provider: Some("openai".to_string()),
                cli_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            },
            owner_id: "postgres-owner-initial".to_string(),
            ttl_ms: 30_000,
            items: vec![JournalEntry {
                seq: 0,
                recorded_at: jiff::Timestamp::now(),
                item: first_item.clone(),
            }],
        })
        .await
        .expect("initialize PostgreSQL journal");
    assert_eq!(initialized.next_seq, 1);

    let loaded = client
        .load_journal(process_id)
        .await
        .expect("load initialized PostgreSQL journal");
    assert_eq!(loaded.next_seq, 1);
    assert_eq!(loaded.items.len(), 1);
    assert_eq!(
        serde_json::to_value(&loaded.items[0].item).expect("serialize loaded item"),
        serde_json::to_value(&first_item).expect("serialize first item")
    );

    client
        .release_lease(
            process_id,
            initialized.lease.owner_id,
            initialized.lease.lease_token,
        )
        .await
        .expect("release initial PostgreSQL lease");
    let resumed_owner = "postgres-owner-resumed".to_string();
    let resumed_lease = client
        .acquire_lease(process_id, resumed_owner.clone(), 30_000)
        .await
        .expect("acquire resumed PostgreSQL lease");
    let second_item = compacted("second");
    client
        .append_batch(AppendBatchInput {
            process_id,
            owner_id: resumed_owner.clone(),
            lease_token: resumed_lease.lease_token.clone(),
            expected_next_seq: loaded.next_seq,
            items: vec![JournalEntry {
                seq: loaded.next_seq,
                recorded_at: jiff::Timestamp::now(),
                item: second_item.clone(),
            }],
        })
        .await
        .expect("append resumed PostgreSQL journal");

    let loaded = client
        .load_journal(process_id)
        .await
        .expect("load appended PostgreSQL journal");
    assert_eq!(loaded.next_seq, 2);
    assert_eq!(loaded.items.len(), 2);
    assert_eq!(
        serde_json::to_value(&loaded.items[1].item).expect("serialize loaded item"),
        serde_json::to_value(&second_item).expect("serialize second item")
    );

    let previous_default = client
        .get_default_process()
        .await
        .expect("read prior default process");
    client
        .set_default_process(process_id)
        .await
        .expect("set PostgreSQL default process");
    assert_eq!(
        client
            .get_default_process()
            .await
            .expect("read PostgreSQL default process"),
        Some(process_id)
    );
    match previous_default {
        Some(previous) => client
            .set_default_process(previous)
            .await
            .expect("restore prior default process"),
        None => {
            sqlx::query("DELETE FROM settings WHERE key = 'default_session_id'")
                .execute(&pool)
                .await
                .expect("clear test default process");
        }
    }

    client
        .release_lease(process_id, resumed_owner, resumed_lease.lease_token)
        .await
        .expect("release resumed PostgreSQL lease");
}
