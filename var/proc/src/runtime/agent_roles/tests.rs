use super::*;

#[tokio::test]
async fn agent_roles_round_trip_with_revision_checks() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDbHandle::Sqlite(StateRuntime::init(home.path().into(), "test".into()).await?);
    assert!(db.list_agent_roles().await?.is_empty());

    let first = db
        .put_agent_role(
            "researcher",
            "toml",
            "name = 'researcher'",
            Some("/repo"),
            0,
        )
        .await?;
    assert_eq!(first, 1);
    assert!(
        db.put_agent_role("researcher", "toml", "x", None, 0)
            .await
            .is_err(),
        "create must not overwrite"
    );
    let role = db.get_agent_role("researcher").await?.expect("stored");
    assert_eq!(role.format, "toml");
    assert_eq!(role.base_dir.as_deref(), Some("/repo"));
    assert!(role.enabled);

    let second = db
        .put_agent_role(
            "researcher",
            "md",
            "---\nname = 'researcher'\n---\nbody",
            None,
            first,
        )
        .await?;
    assert_eq!(second, 2);
    assert!(
        db.put_agent_role("researcher", "md", "stale", None, first)
            .await
            .is_err()
    );

    let third = db
        .set_agent_role_enabled("researcher", second, false)
        .await?;
    assert!(
        !db.get_agent_role("researcher")
            .await?
            .expect("stored")
            .enabled
    );
    assert!(db.delete_agent_role("researcher", second).await.is_err());
    db.delete_agent_role("researcher", third).await?;
    assert!(db.get_agent_role("researcher").await?.is_none());
    Ok(())
}
