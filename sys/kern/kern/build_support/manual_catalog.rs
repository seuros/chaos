//! Build-time discovery and frontmatter validation for embedded manual resources.

use serde::Deserialize;
use std::fmt::Write;
use std::path::Path;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
    title: String,
    summary: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ManualPage {
    id: String,
    title: String,
    summary: String,
    body: String,
}

fn manual_id(filename: &str) -> Option<&str> {
    let id = filename.strip_suffix(".md")?;
    let (_, section) = id.rsplit_once('.')?;
    matches!(section, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9").then_some(id)
}

fn parse_page(id: &str, source: &str) -> Result<ManualPage, String> {
    let (name, _) = id.rsplit_once('.').ok_or("missing manual section")?;
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "manual filename must use letters, digits, '-' or '_' before its section".into(),
        );
    }
    let mut lines = source.split_inclusive('\n');
    let opening = lines.next().unwrap_or_default();
    if opening.trim_end_matches(['\r', '\n']) != "+++" {
        return Err("missing TOML frontmatter: the first line must be +++".into());
    }
    let start = opening.len();
    let mut offset = start;
    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "+++" {
            let frontmatter: Frontmatter = toml::from_str(&source[start..offset])
                .map_err(|error| format!("invalid TOML frontmatter: {error}"))?;
            for (field, value) in [
                ("title", &frontmatter.title),
                ("summary", &frontmatter.summary),
            ] {
                if value.trim().is_empty() || value.chars().any(char::is_control) {
                    return Err(format!("{field} must be nonempty, single-line text"));
                }
            }
            let body = source[offset + line.len()..].trim_start_matches(['\r', '\n']);
            if body.trim().is_empty() {
                return Err("manual body must not be empty".into());
            }
            return Ok(ManualPage {
                id: id.into(),
                title: frontmatter.title.trim().into(),
                summary: frontmatter.summary.trim().into(),
                body: body.into(),
            });
        }
        offset += line.len();
    }
    Err("unterminated TOML frontmatter: expected a closing +++ line".into())
}

pub fn read_catalog(directory: &Path) -> Result<Vec<ManualPage>, String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", directory.display()))?;
        if entry
            .file_type()
            .map_err(|error| format!("{}: {error}", entry.path().display()))?
            .is_file()
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    let mut pages = Vec::new();
    for path in paths {
        let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(manual_id)
        else {
            continue;
        };
        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let page =
            parse_page(id, &source).map_err(|error| format!("{}: {error}", path.display()))?;
        pages.push(page);
    }
    // Canonical IDs, rather than filesystem enumeration order, define the index.
    pages.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(pages)
}

pub fn render_catalog(pages: &[ManualPage]) -> String {
    let mut output = String::from("&[\n");
    for page in pages {
        // Debug string formatting produces escaped Rust literals, including for
        // quotes, backslashes, Unicode, and Markdown code fences.
        writeln!(
            output,
            "ManualPageSpec {{ id: {:?}, title: {:?}, summary: {:?}, source: {:?} }},",
            page.id, page.title, page.summary, page.body,
        )
        .expect("write generated source to String");
    }
    output.push_str("]\n");
    output
}

#[cfg(test)]
#[path = "manual_catalog_tests.rs"]
mod tests;
