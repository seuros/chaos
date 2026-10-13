use super::*;
use crate::test_support::seed_document;
use crate::user_documents::DocumentKind;

#[tokio::test]
async fn empty_when_nothing_is_stored() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    crate::test_support::bootstrap_sqlite(home.path())?;
    assert!(discover_prompts(home.path()).await.is_empty());
    Ok(())
}

#[tokio::test]
async fn stored_prompts_are_sorted_by_name_with_locator_paths() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    seed_document(home.path(), DocumentKind::Prompt, "b", "b body").await?;
    seed_document(home.path(), DocumentKind::Prompt, "a", "a body").await?;

    let found = discover_prompts(home.path()).await;
    let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b"]);
    assert_eq!(found[0].path, PathBuf::from("chaos://prompts/a"));
    assert_eq!(found[0].content, "a body");
    Ok(())
}

#[tokio::test]
async fn disabled_prompts_are_skipped() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    seed_document(home.path(), DocumentKind::Prompt, "hidden", "body").await?;
    seed_document(home.path(), DocumentKind::Prompt, "shown", "body").await?;
    crate::user_documents::set_enabled(home.path(), DocumentKind::Prompt, "hidden", false).await?;

    let names: Vec<String> = discover_prompts(home.path())
        .await
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, vec!["shown"]);
    Ok(())
}

#[tokio::test]
async fn parses_frontmatter_and_strips_from_body() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let text = "---\nname: ignored\ndescription: \"Quick review command\"\n\
                argument-hint: \"[file] [priority]\"\n---\nActual body with $1 and $ARGUMENTS";
    seed_document(home.path(), DocumentKind::Prompt, "withmeta", text).await?;

    let found = discover_prompts(home.path()).await;
    assert_eq!(found.len(), 1);
    let p = &found[0];
    assert_eq!(p.name, "withmeta");
    assert_eq!(p.description.as_deref(), Some("Quick review command"));
    assert_eq!(p.argument_hint.as_deref(), Some("[file] [priority]"));
    // Body should not include the frontmatter delimiters.
    assert_eq!(p.content, "Actual body with $1 and $ARGUMENTS");
    Ok(())
}

#[test]
fn parse_frontmatter_preserves_body_newlines() {
    let content = "---\r\ndescription: \"Line endings\"\r\nargument_hint: \"[arg]\"\r\n\
                   ---\r\nFirst line\r\nSecond line\r\n";
    let (desc, hint, body) = parse_frontmatter(content);
    assert_eq!(desc.as_deref(), Some("Line endings"));
    assert_eq!(hint.as_deref(), Some("[arg]"));
    assert_eq!(body, "First line\r\nSecond line\r\n");
}
