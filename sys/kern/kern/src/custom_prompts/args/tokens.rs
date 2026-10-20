//! Preserve text-element identity while applying shell-style argument quoting.
use super::{ByteRange, PromptArg, TextElement};
use shlex::Shlex;

pub(super) fn parse_tokens_with_elements(
    rest: &str,
    text_elements: &[TextElement],
) -> Vec<PromptArg> {
    let mut elements = text_elements.to_vec();
    elements.sort_by_key(|elem| elem.byte_range.start);
    let (rest_for_shlex, replacements) = replace_text_elements_with_sentinels(rest, &elements);
    Shlex::new(&rest_for_shlex)
        .map(|token| apply_replacements_to_token(token, &replacements))
        .collect()
}

#[derive(Debug, Clone)]
struct ElementReplacement {
    sentinel: String,
    text: String,
    placeholder: Option<String>,
}

/// Replace element ranges with tokens that survive shlex splitting.
fn replace_text_elements_with_sentinels(
    rest: &str,
    elements: &[TextElement],
) -> (String, Vec<ElementReplacement>) {
    let mut out = String::with_capacity(rest.len());
    let mut replacements = Vec::new();
    let mut cursor = 0;
    for (idx, elem) in elements.iter().enumerate() {
        let start = elem.byte_range.start;
        let end = elem.byte_range.end;
        out.push_str(&rest[cursor..start]);
        let mut sentinel = format!("__CHAOS_ELEM_{idx}__");
        // Never mistake user content for an element sentinel.
        while rest.contains(&sentinel) {
            sentinel.push('_');
        }
        out.push_str(&sentinel);
        replacements.push(ElementReplacement {
            sentinel,
            text: rest[start..end].to_string(),
            placeholder: elem.placeholder(rest).map(str::to_string),
        });
        cursor = end;
    }
    out.push_str(&rest[cursor..]);
    (out, replacements)
}

/// Restore element text and ranges inside each emitted argument.
fn apply_replacements_to_token(token: String, replacements: &[ElementReplacement]) -> PromptArg {
    if replacements.is_empty() {
        return PromptArg {
            text: token,
            text_elements: Vec::new(),
        };
    }
    let mut out = String::with_capacity(token.len());
    let mut out_elements = Vec::new();
    let mut cursor = 0;
    while cursor < token.len() {
        let Some((offset, replacement)) = next_replacement(&token, cursor, replacements) else {
            out.push_str(&token[cursor..]);
            break;
        };
        let start_in_token = cursor + offset;
        out.push_str(&token[cursor..start_in_token]);
        let start = out.len();
        out.push_str(&replacement.text);
        let end = out.len();
        if start < end {
            out_elements.push(TextElement::new(
                ByteRange { start, end },
                replacement.placeholder.clone(),
            ));
        }
        cursor = start_in_token + replacement.sentinel.len();
    }
    PromptArg {
        text: out,
        text_elements: out_elements,
    }
}

fn next_replacement<'a>(
    token: &str,
    cursor: usize,
    replacements: &'a [ElementReplacement],
) -> Option<(usize, &'a ElementReplacement)> {
    let slice = &token[cursor..];
    let mut best: Option<(usize, &'a ElementReplacement)> = None;
    for replacement in replacements {
        if let Some(pos) = slice.find(&replacement.sentinel) {
            match best {
                Some((best_pos, _)) if best_pos <= pos => {}
                _ => best = Some((pos, replacement)),
            }
        }
    }
    best
}
