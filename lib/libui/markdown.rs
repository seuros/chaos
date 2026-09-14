use ratatui::text::Line;
use std::path::Path;

/// Render assistant Markdown into `lines`, applying the configured prose style
/// and resolving local file-link display relative to `cwd`.
///
/// Callers that already know the session working directory should pass it here so streamed and
/// non-streamed rendering show the same relative path text even if the process cwd differs.
pub fn append_markdown(
    markdown_source: &str,
    width: Option<usize>,
    cwd: Option<&Path>,
    lines: &mut Vec<Line<'static>>,
) {
    let rendered = crate::markdown_render::render_markdown_with_prose_style(
        markdown_source,
        width,
        cwd,
        crate::theme::assistant_message(),
    );
    crate::render::line_utils::push_owned_lines(&rendered.lines, lines);
}

#[cfg(test)]
pub(crate) mod tests;
