use chaos_ipc::custom_prompts::CustomPrompt;
use std::path::Path;
use std::path::PathBuf;

/// Enabled prompts stored for `chaos_home`, sorted by name. The `path` field is
/// a `chaos://prompts/<name>` locator; prompts no longer live on disk.
pub async fn discover_prompts(chaos_home: &Path) -> Vec<CustomPrompt> {
    let documents = match crate::user_documents::enabled(
        chaos_home,
        crate::user_documents::DocumentKind::Prompt,
    )
    .await
    {
        Ok(documents) => documents,
        Err(err) => {
            tracing::warn!("failed to load stored prompts: {err:#}");
            return Vec::new();
        }
    };
    documents
        .into_iter()
        .map(|document| {
            let (description, argument_hint, body) = parse_frontmatter(&document.source);
            CustomPrompt {
                path: PathBuf::from(format!("chaos://prompts/{}", document.name)),
                name: document.name,
                content: body,
                description,
                argument_hint,
            }
        })
        .collect()
}

/// Parse optional YAML-like frontmatter at the beginning of `content`.
/// Supported keys:
/// - `description`: short description shown in the slash popup
/// - `argument-hint` or `argument_hint`: brief hint string shown after the description
///   Returns (description, argument_hint, body_without_frontmatter).
pub(crate) fn parse_frontmatter(content: &str) -> (Option<String>, Option<String>, String) {
    let mut segments = content.split_inclusive('\n');
    let Some(first_segment) = segments.next() else {
        return (None, None, String::new());
    };
    let first_line = first_segment.trim_end_matches(['\r', '\n']);
    if first_line.trim() != "---" {
        return (None, None, content.to_string());
    }

    let mut desc: Option<String> = None;
    let mut hint: Option<String> = None;
    let mut frontmatter_closed = false;
    let mut consumed = first_segment.len();

    for segment in segments {
        let line = segment.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim();

        if trimmed == "---" {
            frontmatter_closed = true;
            consumed += segment.len();
            break;
        }

        if trimmed.is_empty() || trimmed.starts_with('#') {
            consumed += segment.len();
            continue;
        }

        if let Some((k, v)) = trimmed.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let mut val = v.trim().to_string();
            if val.len() >= 2 {
                let bytes = val.as_bytes();
                let first = bytes[0];
                let last = bytes[bytes.len() - 1];
                if (first == b'\"' && last == b'\"') || (first == b'\'' && last == b'\'') {
                    val = val[1..val.len().saturating_sub(1)].to_string();
                }
            }
            match key.as_str() {
                "description" => desc = Some(val),
                "argument-hint" | "argument_hint" => hint = Some(val),
                _ => {}
            }
        }

        consumed += segment.len();
    }

    if !frontmatter_closed {
        // Unterminated frontmatter: treat input as-is.
        return (None, None, content.to_string());
    }

    let body = if consumed >= content.len() {
        String::new()
    } else {
        content[consumed..].to_string()
    };
    (desc, hint, body)
}

#[cfg(test)]
mod tests;
