use super::*;

#[tokio::test]
async fn settings_cas_and_installation_scoped_revocation() -> anyhow::Result<()> {
    let home = std::env::temp_dir().join(format!("chaos-settings-{}", Uuid::now_v7()));
    let runtime = RuntimeDbHandle::Sqlite(StateRuntime::init(home.clone(), "test".into()).await?);
    let snapshot = runtime.settings_snapshot().await?;
    assert_eq!(snapshot.revision, 0);
    runtime
        .commit_settings(0, &serde_json::json!({"model":"test"}), Some("digest"))
        .await?;
    assert!(
        runtime
            .commit_settings(0, &serde_json::json!({}), None)
            .await
            .is_err()
    );
    assert_eq!(runtime.settings_snapshot().await?.settings["model"], "test");
    let mut approval = RememberedApproval {
        id: Uuid::now_v7().to_string(),
        installation_id: "one".into(),
        scope: "/project".into(),
        kind: "mcp".into(),
        subject: "server/tool".into(),
        identity: "v1:identity".into(),
        state: ApprovalState::Active,
        payload: Value::Null,
    };
    runtime.put_approval(&approval).await?;
    approval.id = Uuid::now_v7().to_string();
    approval.installation_id = "two".into();
    runtime.put_approval(&approval).await?;
    let revision = runtime.approval_revision().await?;
    assert_eq!(runtime.revoke_approvals("one", None).await?, 1);
    assert!(runtime.approval_revision().await? > revision);
    assert_eq!(
        runtime.list_approvals("one").await?[0].state,
        ApprovalState::Revoked
    );
    assert_eq!(
        runtime.list_approvals("two").await?[0].state,
        ApprovalState::Active
    );
    drop(runtime);
    std::fs::remove_dir_all(home)?;
    Ok(())
}
