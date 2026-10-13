use super::*;

#[tokio::test]
async fn documents_are_scoped_by_kind_with_revision_checks() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDbHandle::Sqlite(StateRuntime::init(home.path().into(), "test".into()).await?);

    let first = db
        .put_document(
            DocumentKind::Mode,
            "focus",
            "+++\nid = 'focus'\n+++\nbody",
            0,
        )
        .await?;
    assert_eq!(first, 1);
    db.put_document(DocumentKind::Prompt, "focus", "prompt body", 0)
        .await?;
    assert_eq!(db.list_documents(DocumentKind::Mode).await?.len(), 1);
    assert_eq!(db.list_documents(DocumentKind::Prompt).await?.len(), 1);
    assert!(db.list_documents(DocumentKind::Theme).await?.is_empty());

    assert!(
        db.put_document(DocumentKind::Mode, "focus", "x", 0)
            .await
            .is_err(),
        "create must not overwrite"
    );
    let second = db
        .put_document(
            DocumentKind::Mode,
            "focus",
            "+++\nid = 'focus'\n+++\nnew",
            first,
        )
        .await?;
    assert_eq!(second, 2);
    assert!(
        db.put_document(DocumentKind::Mode, "focus", "stale", first)
            .await
            .is_err()
    );

    let third = db
        .set_document_enabled(DocumentKind::Mode, "focus", second, false)
        .await?;
    let stored = db
        .get_document(DocumentKind::Mode, "focus")
        .await?
        .expect("stored");
    assert!(!stored.enabled);
    assert_eq!(stored.source, "+++\nid = 'focus'\n+++\nnew");
    assert!(
        db.delete_document(DocumentKind::Mode, "focus", second)
            .await
            .is_err()
    );
    db.delete_document(DocumentKind::Mode, "focus", third)
        .await?;
    assert!(
        db.get_document(DocumentKind::Mode, "focus")
            .await?
            .is_none()
    );
    assert_eq!(db.list_documents(DocumentKind::Prompt).await?.len(), 1);
    Ok(())
}
