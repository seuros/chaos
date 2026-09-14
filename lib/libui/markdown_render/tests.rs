use super::*;
use pretty_assertions::assert_eq;
use ratatui::text::Text;

fn lines_to_strings(text: &Text<'_>) -> Vec<String> {
    text.lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect()
}

pub(crate) fn markdown_render_suite() {
    super::markdown_render_tests::markdown_render_suite();

    wraps_plain_text_when_width_provided();
    wraps_list_items_preserving_indent();
    wraps_nested_lists();
    wraps_ordered_lists();
    wraps_blockquotes();
    wraps_blockquotes_inside_lists();
    wraps_list_items_containing_blockquotes();
    does_not_wrap_code_blocks();
    does_not_split_long_url_like_token_without_scheme();
    fenced_code_info_string_with_metadata_highlights();
    crlf_code_block_no_extra_blank_lines();
}
#[cfg(test)]
fn wraps_plain_text_when_width_provided() {
    let markdown = "This is a simple sentence that should wrap.";
    let rendered = render_markdown_text_with_width(markdown, Some(16));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec![
            "This is a simple".to_string(),
            "sentence that".to_string(),
            "should wrap.".to_string(),
        ]
    );
}

#[cfg(test)]
fn wraps_list_items_preserving_indent() {
    let markdown = "- first second third fourth";
    let rendered = render_markdown_text_with_width(markdown, Some(14));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec!["- first second".to_string(), "  third fourth".to_string(),]
    );
}

#[cfg(test)]
fn wraps_nested_lists() {
    let markdown =
        "- outer item with several words to wrap\n  - inner item that also needs wrapping";
    let rendered = render_markdown_text_with_width(markdown, Some(20));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec![
            "- outer item with".to_string(),
            "  several words to".to_string(),
            "  wrap".to_string(),
            "    - inner item".to_string(),
            "      that also".to_string(),
            "      needs wrapping".to_string(),
        ]
    );
}

#[cfg(test)]
fn wraps_ordered_lists() {
    let markdown = "1. ordered item contains many words for wrapping";
    let rendered = render_markdown_text_with_width(markdown, Some(18));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec![
            "1. ordered item".to_string(),
            "   contains many".to_string(),
            "   words for".to_string(),
            "   wrapping".to_string(),
        ]
    );
}

#[cfg(test)]
fn wraps_blockquotes() {
    let markdown = "> block quote with content that should wrap nicely";
    let rendered = render_markdown_text_with_width(markdown, Some(22));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec![
            "> block quote with".to_string(),
            "> content that should".to_string(),
            "> wrap nicely".to_string(),
        ]
    );
}

#[cfg(test)]
fn wraps_blockquotes_inside_lists() {
    let markdown = "- list item\n  > block quote inside list that wraps";
    let rendered = render_markdown_text_with_width(markdown, Some(24));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec![
            "- list item".to_string(),
            "  > block quote inside".to_string(),
            "  > list that wraps".to_string(),
        ]
    );
}

#[cfg(test)]
fn wraps_list_items_containing_blockquotes() {
    let markdown = "1. item with quote\n   > quoted text that should wrap";
    let rendered = render_markdown_text_with_width(markdown, Some(24));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec![
            "1. item with quote".to_string(),
            "   > quoted text that".to_string(),
            "   > should wrap".to_string(),
        ]
    );
}

#[cfg(test)]
fn does_not_wrap_code_blocks() {
    let markdown = "````\nfn main() { println!(\"hi from a long line\"); }\n````";
    let rendered = render_markdown_text_with_width(markdown, Some(10));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec!["fn main() { println!(\"hi from a long line\"); }".to_string(),]
    );
}

#[cfg(test)]
fn does_not_split_long_url_like_token_without_scheme() {
    let url_like = "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890";
    let rendered = render_markdown_text_with_width(url_like, Some(24));
    let lines = lines_to_strings(&rendered);

    assert_eq!(
        lines.iter().filter(|line| line.contains(url_like)).count(),
        1,
        "expected full URL-like token in one rendered line, got: {lines:?}"
    );
}

#[cfg(test)]
fn fenced_code_info_string_with_metadata_highlights() {
    for info in &["rust,no_run", "rust no_run", "rust title=\"demo\""] {
        let markdown = format!("```{info}\nfn main() {{}}\n```\n");
        let rendered = render_markdown_text(&markdown);
        let has_rgb = rendered.lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
        });
        assert!(
            has_rgb,
            "info string \"{info}\" should still produce syntax highlighting"
        );
    }
}

#[cfg(test)]
fn crlf_code_block_no_extra_blank_lines() {
    let markdown = "```rust\r\nfn main() {}\r\n    line2\r\n```\r\n";
    let rendered = render_markdown_text(markdown);
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec!["fn main() {}".to_string(), "    line2".to_string()],
        "CRLF code block should not produce extra blank lines: {lines:?}"
    );
}
