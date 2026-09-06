use sqlx::migrate::Migrator;

pub(crate) static STATE_MIGRATOR: Migrator = sqlx::migrate!("./db/migrate/sqlite");
pub(crate) static POSTGRES_STATE_MIGRATOR: Migrator = sqlx::migrate!("./db/migrate/postgres");

#[cfg(test)]
mod tests {
    use chaos_ipc::ProcessId;
    use chaos_ipc::models::ContentItem;
    use chaos_ipc::models::ResponseItem;
    use chaos_ipc::protocol::RolloutItem;
    use pretty_assertions::assert_eq;
    use sqlx::Row;
    use uuid::Uuid;

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
