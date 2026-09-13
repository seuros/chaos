use super::*;

#[test]
fn resolves_index_and_page_uris() {
    assert_eq!(
        resolve_resource_uri(MANUAL_INDEX_URI).expect("resolve index"),
        Some(ResolvedManualResource::Index)
    );
    assert_eq!(
        resolve_resource_uri("chaos://man/chaos-mcp.7").expect("resolve page"),
        Some(ResolvedManualResource::Page(
            MANUAL_PAGES
                .iter()
                .find(|page| page.id == "chaos-mcp.7")
                .expect("embedded page")
        ))
    );
    assert!(matches!(
        resolve_resource_uri("chaos://man/chaos-modes.7#switching")
            .expect("resolve anchored page"),
        Some(ResolvedManualResource::Page(page)) if page.id == "chaos-modes.7"
    ));
}

#[test]
fn rejects_unknown_manual_page() {
    let err = resolve_resource_uri("chaos://man/missing.7").expect_err("unknown page");
    assert_eq!(err, "manual page not found: missing.7");

    for uri in ["chaos://man/README", "chaos://man/../Cargo.toml"] {
        assert!(resolve_resource_uri(uri).is_err());
    }
}

#[test]
fn index_lists_discovered_pages_with_resource_uris() {
    let text = index_json().expect("manual index");
    assert!(!text.contains('\n'), "model-facing JSON must be compact");
    let value: serde_json::Value = serde_json::from_str(&text).expect("parse index");
    let pages = value["pages"].as_array().expect("pages array");
    assert_eq!(pages.len(), MANUAL_PAGES.len());
    assert!(MANUAL_PAGES.windows(2).all(|pair| pair[0].id < pair[1].id));
    for page in MANUAL_PAGES {
        assert!(pages.iter().any(|entry| entry["id"] == page.id
            && entry["uri"] == page_uri(page.id)
            && entry["title"] == page.title
            && entry["summary"] == page.summary));
    }
    for id in ["chaos-appearance.7", "chaos-httpd.8", "chaos-install.7"] {
        assert!(pages.iter().any(|page| page["id"] == id));
    }
}

#[test]
fn rendered_page_preserves_see_also_links() {
    let page = MANUAL_PAGES
        .iter()
        .find(|page| page.id == "chaos-mcp.7")
        .expect("embedded page");
    let rendered = render_page(page);
    assert!(rendered.starts_with("## NAME"));
    assert!(rendered.contains("[chaos-modes.7](chaos://man/chaos-modes.7)"));
    assert_eq!(rendered.matches("chaos://man/chaos-modes.7").count(), 1);
    assert!(rendered.contains("[chaos-install.7](chaos://man/chaos-install.7)"));
    assert!(rendered.contains("## SEE ALSO"));
    assert!(!rendered.contains("+++"));
    assert!(!rendered.contains("](./"));
}

#[test]
fn rendered_pages_append_only_index_navigation() {
    for page in MANUAL_PAGES {
        let rendered = render_page(page);
        let (_, footer) = rendered
            .rsplit_once("\n\n---\n\n")
            .expect("manual navigation footer");
        assert_eq!(footer, "Index: `chaos://man`\n", "{}", page.id);
    }
}

#[test]
fn rewrites_relative_manual_markdown_links() {
    assert_eq!(
        rewrite_manual_links("[Modes](./chaos-modes.7.md#switching)"),
        "[Modes](chaos://man/chaos-modes.7#switching)"
    );
}

#[test]
fn omits_only_the_leading_document_title() {
    for newline in ["\n", "\r\n"] {
        let body = format!("## Section{newline}{newline}Body.{newline}# Later heading");
        let source = format!("# Document title{newline}{newline}{body}");
        assert_eq!(without_document_title(&source), body);
    }
    for source in [
        "## Section\n\nBody.",
        "Introduction.\n\n# Later heading",
        "#not-a-heading\n\nBody.",
        "```markdown\n# Example heading\n```",
    ] {
        assert_eq!(without_document_title(source), source);
    }
    assert_eq!(without_document_title("# Title"), "");
}

#[test]
fn appearance_resource_omits_frontmatter_and_document_title() {
    let Some(ResolvedManualResource::Page(page)) =
        resolve_resource_uri("chaos://man/chaos-appearance.7").unwrap()
    else {
        panic!("missing appearance manual");
    };
    assert!(page.source.starts_with("# chaos-appearance(7)"));
    let rendered = render_page(page);
    assert!(rendered.starts_with("## Configuration"));
    assert!(!rendered.contains("# chaos-appearance(7)"));
    assert!(rendered.contains("chaos config set appearance.user.bold true"));
    assert!(!rendered.contains("summary ="));
    assert!(!rendered.contains("+++"));
}
