use super::*;
use chaos_ipc::models::function_call_output_content_items_to_text;
use mcp_host::protocol::types::TaskStatus;
use serde_json::json;

#[tokio::test]
async fn serverless_cancel_routes_to_internal_task_store() {
    let (session, mut turn) = crate::chaos::make_session_and_context().await;
    turn.approval_policy
        .set(chaos_ipc::protocol::ApprovalPolicy::Headless)
        .unwrap();
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let task = session
        .services
        .internal_task_store
        .create_task(
            None,
            TaskStatus::Completed,
            None,
            Some(json!({"output": "done"})),
            None,
        )
        .await;
    let output = handle_cancel_task(
        session.clone(),
        turn.clone(),
        "cancel".into(),
        TaskIdArgs {
            server: None,
            task_id: task.task_id.clone(),
        },
    )
    .await
    .unwrap();
    let text = function_call_output_content_items_to_text(&output.body).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["taskId"], task.task_id);
    assert!(value.get("server").is_none());
    for server in [Some("chaos_local".into()), Some("not-configured".into())] {
        assert!(
            handle_cancel_task(
                session.clone(),
                turn.clone(),
                "cancel-invalid".into(),
                TaskIdArgs {
                    server,
                    task_id: task.task_id.clone()
                },
            )
            .await
            .is_err()
        );
    }
    assert!(serde_json::from_value::<CallToolAsyncArgs>(json!({"tool": "demo"})).is_err());
}
