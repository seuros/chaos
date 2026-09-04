use sqlx::migrate::Migrator;

pub(crate) static STATE_MIGRATOR: Migrator = sqlx::migrate!("./db/migrate/sqlite");
pub(crate) static POSTGRES_STATE_MIGRATOR: Migrator = sqlx::migrate!("./db/migrate/postgres");

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_ipc::ProcessId;
    use chaos_ipc::models::ContentItem;
    use chaos_ipc::models::ResponseItem;
    use chaos_ipc::protocol::RolloutItem;
    use pretty_assertions::assert_eq;
    use sqlx::Row;
    use uuid::Uuid;

    const SQLITE_BIGBANG_CHECKSUM: &[u8] = &[
        0x3b, 0xa2, 0x8c, 0xc2, 0x27, 0xaa, 0x65, 0x0a, 0xb2, 0x20, 0x73, 0x1e, 0xe0, 0x17, 0xe5,
        0xf6, 0xf9, 0x15, 0xe6, 0x9a, 0xaf, 0xda, 0xf7, 0x7c, 0xb8, 0xfc, 0x48, 0x0d, 0xbd, 0x9c,
        0x19, 0x2c, 0xd2, 0x7d, 0x05, 0xd7, 0xdc, 0xf4, 0x72, 0x84, 0xf7, 0x3c, 0x0c, 0x50, 0x71,
        0x66, 0xd4, 0x7e,
    ];
    const POSTGRES_BIGBANG_CHECKSUM: &[u8] = &[
        0xd3, 0x75, 0xe5, 0x5b, 0xcd, 0x3d, 0x86, 0xea, 0x85, 0xde, 0x65, 0x4f, 0xd0, 0xa0, 0x04,
        0x23, 0x16, 0x02, 0xc4, 0x29, 0x35, 0xa4, 0x33, 0xcc, 0x52, 0xb5, 0x70, 0xde, 0x6f, 0xea,
        0x2e, 0x6b, 0x93, 0xf2, 0x01, 0x4f, 0x11, 0x69, 0x65, 0xae, 0xb9, 0xd2, 0xf6, 0xb0, 0x77,
        0x0d, 0x80, 0x9d,
    ];

    #[test]
    fn initial_migration_checksums_remain_immutable() {
        assert_eq!(
            STATE_MIGRATOR.iter().next().unwrap().checksum.as_ref(),
            SQLITE_BIGBANG_CHECKSUM,
            "create a new SQLite migration instead of editing 0001_bigbang.sql"
        );
        assert_eq!(
            POSTGRES_STATE_MIGRATOR
                .iter()
                .next()
                .unwrap()
                .checksum
                .as_ref(),
            POSTGRES_BIGBANG_CHECKSUM,
            "create a new Postgres migration instead of editing 0001_bigbang.sql"
        );
    }

    #[test]
    fn sqlite_and_postgres_migrations_advance_together() {
        let sqlite_versions = STATE_MIGRATOR
            .iter()
            .map(|migration| migration.version)
            .collect::<Vec<_>>();
        let postgres_versions = POSTGRES_STATE_MIGRATOR
            .iter()
            .map(|migration| migration.version)
            .collect::<Vec<_>>();

        assert_eq!(sqlite_versions, postgres_versions);
        assert_eq!(sqlite_versions.last().copied(), Some(13));
    }

    #[tokio::test]
    async fn sqlite_preview_backfill_repairs_legacy_blank_metadata() {
        let chaos_home =
            std::env::temp_dir().join(format!("chaos-preview-backfill-{}", Uuid::now_v7()));
        let runtime = crate::StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("runtime db should initialize");
        let process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000512")
            .expect("valid process id");

        sqlx::query(
            r#"
INSERT INTO processes (
    id,
    source,
    source_json,
    model_provider,
    cwd,
    created_at,
    updated_at,
    title,
    first_user_message
) VALUES (?, 'cli', '"cli"', 'test-provider', '/tmp/project', 1, 2, '', '')
            "#,
        )
        .bind(process_id.to_string())
        .execute(runtime.pool())
        .await
        .expect("legacy process row should insert");

        let context_item = RolloutItem::ResponseItem(ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: concat!(
                    "<environment_context>\n",
                    "  <cwd>/old/project</cwd>\n",
                    "</environment_context>\n",
                    "<environment_context>\n",
                    "  <shell>zsh</shell>\n",
                    "</environment_context>"
                )
                .to_string(),
            }],
            end_turn: None,
            phase: None,
        });
        let request_item = RolloutItem::ResponseItem(ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "## My request for Chaos: Recover the conversation title".to_string(),
            }],
            end_turn: None,
            phase: None,
        });
        for (seq, item) in [context_item, request_item].into_iter().enumerate() {
            sqlx::query(
                r#"
INSERT INTO journal_entries (process_id, seq, recorded_at, item_type, payload_json)
VALUES (?, ?, 2, 'response_item', ?)
            "#,
            )
            .bind(process_id.to_string())
            .bind(seq as i64)
            .bind(serde_json::to_string(&item).expect("rollout item should serialize"))
            .execute(runtime.pool())
            .await
            .expect("legacy journal entry should insert");
        }

        sqlx::raw_sql(include_str!(
            "../db/migrate/sqlite/0013_retry_process_preview_backfill.sql"
        ))
        .execute(runtime.pool())
        .await
        .expect("preview backfill should run");

        let row =
            sqlx::query("SELECT title, first_user_message, updated_at FROM processes WHERE id = ?")
                .bind(process_id.to_string())
                .fetch_one(runtime.pool())
                .await
                .expect("repaired metadata should load");
        assert_eq!(
            row.try_get::<String, _>("first_user_message")
                .expect("first_user_message"),
            "Recover the conversation title"
        );
        assert_eq!(
            row.try_get::<String, _>("title").expect("title"),
            "Recover the conversation title"
        );
        assert_eq!(
            row.try_get::<i64, _>("updated_at").expect("updated_at"),
            2,
            "metadata repair must not make an old session look newly updated"
        );

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }
}
