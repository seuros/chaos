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

#[tokio::test]
async fn installation_settings_are_scoped_and_revision_checked() -> anyhow::Result<()> {
    let home = std::env::temp_dir().join(format!("chaos-inst-settings-{}", Uuid::now_v7()));
    let runtime = RuntimeDbHandle::Sqlite(StateRuntime::init(home.clone(), "test".into()).await?);
    let empty = runtime.installation_settings_snapshot("one").await?;
    assert_eq!(empty.revision, 0);
    assert_eq!(empty.settings, serde_json::json!({}));
    assert_eq!(
        runtime
            .commit_installation_settings("one", 0, &serde_json::json!({"model":"a"}))
            .await?,
        1
    );
    assert!(
        runtime
            .commit_installation_settings("one", 0, &serde_json::json!({"model":"b"}))
            .await
            .is_err()
    );
    assert_eq!(
        runtime
            .commit_installation_settings("one", 1, &serde_json::json!({"model":"b"}))
            .await?,
        2
    );
    assert_eq!(
        runtime
            .installation_settings_snapshot("one")
            .await?
            .settings["model"],
        "b"
    );
    assert_eq!(
        runtime
            .installation_settings_snapshot("two")
            .await?
            .revision,
        0
    );
    assert_eq!(runtime.settings_snapshot().await?.revision, 0);
    drop(runtime);
    std::fs::remove_dir_all(home)?;
    Ok(())
}
