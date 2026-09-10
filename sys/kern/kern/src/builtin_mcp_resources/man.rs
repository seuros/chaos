use serde::Serialize;

pub const MANUAL_INDEX_URI: &str = "chaos://man";
pub const MANUAL_PAGE_URI_TEMPLATE: &str = "chaos://man/{page}";
pub const MARKDOWN_MIME_TYPE: &str = "text/markdown";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManualPageSpec {
    pub id: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    source: &'static str,
}

const MANUAL_PAGES: &[ManualPageSpec] = include!(concat!(env!("OUT_DIR"), "/manual_pages.rs"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedManualResource {
    Index,
    Page(&'static ManualPageSpec),
}

#[derive(Serialize)]
struct ManualIndex<'a> {
    index_uri: &'static str,
    page_uri_template: &'static str,
    pages: Vec<ManualIndexEntry<'a>>,
}

#[derive(Serialize)]
struct ManualIndexEntry<'a> {
    id: &'a str,
    title: &'a str,
    summary: &'a str,
    uri: String,
}

pub fn page_uri(page_id: &str) -> String {
    format!("chaos://man/{page_id}")
}

pub fn resolve_resource_uri(uri: &str) -> Result<Option<ResolvedManualResource>, String> {
    if uri == MANUAL_INDEX_URI {
        return Ok(Some(ResolvedManualResource::Index));
    }

    let Some(page_reference) = uri.strip_prefix("chaos://man/") else {
        return Ok(None);
    };
    let page_id = page_reference
        .split_once('#')
        .map_or(page_reference, |(page_id, _)| page_id);
    if page_id.is_empty() {
        return Err("missing page id in manual resource URI".to_string());
    }

    let page = MANUAL_PAGES
        .iter()
        .find(|page| page.id == page_id)
        .ok_or_else(|| format!("manual page not found: {page_id}"))?;
    Ok(Some(ResolvedManualResource::Page(page)))
}

pub fn index_json() -> Result<String, String> {
    let index = ManualIndex {
        index_uri: MANUAL_INDEX_URI,
        page_uri_template: MANUAL_PAGE_URI_TEMPLATE,
        pages: MANUAL_PAGES
            .iter()
            .map(|page| ManualIndexEntry {
                id: page.id,
                title: page.title,
                summary: page.summary,
                uri: page_uri(page.id),
            })
            .collect(),
    };
    serde_json::to_string(&index)
        .map_err(|err| format!("failed to serialize manual index resource: {err}"))
}

pub fn render_page(page: &ManualPageSpec) -> String {
    let mut rendered = rewrite_manual_links(without_document_title(page.source).trim_end());
    rendered.push_str("\n\n---\n\nIndex: `chaos://man`\n");
    rendered
}

/// The catalog supplies the title; keep the resource body focused on content.
/// Only omit a leading H1, not section headings or headings later in the body.
fn without_document_title(source: &str) -> &str {
    let (heading, body) = source.split_once('\n').unwrap_or((source, ""));
    let heading = heading.trim_end_matches('\r');
    if heading == "#" || heading.starts_with("# ") || heading.starts_with("#\t") {
        body.trim_start_matches(['\r', '\n'])
    } else {
        source
    }
}

fn rewrite_manual_links(source: &str) -> String {
    let mut rendered = source.to_string();
    for page in MANUAL_PAGES {
        rendered = rendered.replace(
            &format!("](./{}.md", page.id),
            &format!("]({}", page_uri(page.id)),
        );
    }
    rendered
}

#[cfg(test)]
mod tests {
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
}
