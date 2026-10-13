use super::*;

const MINIMAL_TMTHEME: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>name</key><string>Test</string>
<key>settings</key><array><dict>
<key>settings</key><dict>
<key>foreground</key><string>#FFFFFF</string>
<key>background</key><string>#000000</string>
</dict></dict></array>
</dict></plist>"#;

#[test]
fn theme_validation_uses_the_tmtheme_loader() {
    assert!(validate(DocumentKind::Theme, "ok", MINIMAL_TMTHEME).is_ok());
    for bad in [
        "<plist",
        "<plist version=\"1.0\"><dict></dict></plist>",
        "not a plist",
    ] {
        assert!(
            validate(DocumentKind::Theme, "bad", bad).is_err(),
            "{bad:?} must not import"
        );
    }
}

#[tokio::test]
async fn invalid_theme_replacement_keeps_the_stored_document() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    crate::test_support::bootstrap_sqlite(home.path())?;
    import(home.path(), DocumentKind::Theme, "night", MINIMAL_TMTHEME).await?;

    assert!(
        import(home.path(), DocumentKind::Theme, "night", "<plist")
            .await
            .is_err()
    );

    let stored = get(home.path(), DocumentKind::Theme, "night")
        .await?
        .expect("theme still stored");
    assert_eq!(stored.source, MINIMAL_TMTHEME);
    Ok(())
}
