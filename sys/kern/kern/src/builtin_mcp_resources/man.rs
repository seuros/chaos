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

const AGENT_MANUAL_PAGES: [ManualPageSpec; 4] = [
    ManualPageSpec {
        id: "chaos-mcp.7",
        title: "chaos-mcp(7)",
        summary: "MCP client and server usage",
        source: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../man/chaos-mcp.7.md"
        )),
    },
    ManualPageSpec {
        id: "chaos-modes.7",
        title: "chaos-modes(7)",
        summary: "Collaboration mode discovery and switching",
        source: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../man/chaos-modes.7.md"
        )),
    },
    ManualPageSpec {
        id: "chaos-storage.7",
        title: "chaos-storage(7)",
        summary: "Persisted history and agent history access",
        source: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../man/chaos-storage.7.md"
        )),
    },
    ManualPageSpec {
        id: "chaos-synopsis.7",
        title: "chaos-synopsis(7)",
        summary: "Sub-agent orchestration gate",
        source: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../man/chaos-synopsis.7.md"
        )),
    },
];

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

    let page = AGENT_MANUAL_PAGES
        .iter()
        .find(|page| page.id == page_id)
        .ok_or_else(|| format!("manual page not found: {page_id}"))?;
    Ok(Some(ResolvedManualResource::Page(page)))
}

pub fn index_json() -> Result<String, String> {
    let index = ManualIndex {
        index_uri: MANUAL_INDEX_URI,
        page_uri_template: MANUAL_PAGE_URI_TEMPLATE,
        pages: AGENT_MANUAL_PAGES
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
    let mut rendered = rewrite_manual_links(&strip_see_also(page.source));
    rendered.push_str("\n\n---\n\n## MANUAL RESOURCES\n\n");
    rendered.push_str("- Index: `chaos://man`\n");
    for related in AGENT_MANUAL_PAGES
        .iter()
        .filter(|related| related.id != page.id)
    {
        rendered.push_str(&format!(
            "- `{}` — {}\n",
            page_uri(related.id),
            related.summary
        ));
    }
    rendered
}

fn rewrite_manual_links(source: &str) -> String {
    let mut rendered = source.to_string();
    for page in &AGENT_MANUAL_PAGES {
        rendered = rendered.replace(
            &format!("](./{}.md", page.id),
            &format!("]({}", page_uri(page.id)),
        );
    }
    rendered
}

fn strip_see_also(source: &str) -> String {
    source
        .split_once("\n## SEE ALSO\n")
        .map_or(source, |(body, _)| body)
        .trim_end()
        .to_string()
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
                AGENT_MANUAL_PAGES
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

        let err =
            resolve_resource_uri("chaos://man/chaos-httpd.8").expect_err("operator-only page");
        assert_eq!(err, "manual page not found: chaos-httpd.8");
    }

    #[test]
    fn index_lists_only_agent_facing_pages_with_resource_uris() {
        let text = index_json().expect("manual index");
        assert!(!text.contains('\n'), "model-facing JSON must be compact");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse index");
        let pages = value["pages"].as_array().expect("pages array");
        assert_eq!(pages.len(), 4);
        assert_eq!(pages[0]["id"], "chaos-mcp.7");
        assert_eq!(pages[0]["uri"], "chaos://man/chaos-mcp.7");
    }

    #[test]
    fn rendered_page_exposes_other_manual_resource_uris() {
        let page = AGENT_MANUAL_PAGES
            .iter()
            .find(|page| page.id == "chaos-mcp.7")
            .expect("embedded page");
        let rendered = render_page(page);
        assert!(rendered.starts_with("# chaos-mcp(7)"));
        assert!(rendered.contains("Index: `chaos://man`"));
        assert!(rendered.contains("`chaos://man/chaos-modes.7`"));
        assert!(!rendered.contains("`chaos://man/chaos-mcp.7`"));
        assert!(!rendered.contains("chaos://man/chaos-install.7"));
        assert!(!rendered.contains("](./"));
    }

    #[test]
    fn rewrites_relative_manual_markdown_links() {
        assert_eq!(
            rewrite_manual_links("[Modes](./chaos-modes.7.md#switching)"),
            "[Modes](chaos://man/chaos-modes.7#switching)"
        );
    }

    #[test]
    fn strips_source_see_also_before_adding_curated_navigation() {
        assert_eq!(
            strip_see_also("# Page\n\nBody.\n\n## SEE ALSO\n\n- hidden\n"),
            "# Page\n\nBody."
        );
    }
}
