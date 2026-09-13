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
mod tests;
