use ratatui::text::Line;
use ratatui::text::Span;
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::util::LinesWithEndings;

use super::singletons::syntax_set;
use super::style_conversion::convert_style;
use super::syntax_lookup::find_syntax;
use super::theme_management::theme_lock;

// -- Guardrail constants ------------------------------------------------------

/// Skip highlighting for inputs larger than 512 KB to avoid excessive memory
/// and CPU usage.  Callers fall back to plain unstyled text.
pub(super) const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;

/// Skip highlighting for inputs with more than 10,000 lines.
pub(super) const MAX_HIGHLIGHT_LINES: usize = 10_000;

/// Check whether an input exceeds the safe highlighting limits.
///
/// Callers that highlight content in a loop (e.g. per diff-line) should
/// pre-check the aggregate size with this function and skip highlighting
/// entirely when it returns `true`.
pub fn exceeds_highlight_limits(total_bytes: usize, total_lines: usize) -> bool {
    total_bytes > MAX_HIGHLIGHT_BYTES || total_lines > MAX_HIGHLIGHT_LINES
}

// -- Core highlighting --------------------------------------------------------

/// Core highlighter that accepts an explicit theme reference.
///
/// This keeps production behavior and test behavior on the same code path:
/// production callers pass the global theme lock, while tests can pass a
/// concrete theme without mutating process-global state.
pub(super) fn highlight_to_line_spans_with_theme(
    code: &str,
    lang: &str,
    theme: &Theme,
) -> Option<Vec<Vec<Span<'static>>>> {
    if code.is_empty() {
        return None;
    }

    if code.len() > MAX_HIGHLIGHT_BYTES || code.lines().count() > MAX_HIGHLIGHT_LINES {
        return None;
    }

    let syntax = find_syntax(lang)?;
    let mut h = HighlightLines::new(syntax, theme);
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();

    for line in LinesWithEndings::from(code) {
        let ranges = h.highlight_line(line, syntax_set()).ok()?;
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (style, text) in ranges {
            // Strip trailing line endings (LF and CR) since we handle line
            // breaks ourselves.  CRLF inputs would otherwise leave a stray \r.
            let text = text.trim_end_matches(['\n', '\r']);
            if text.is_empty() {
                continue;
            }
            spans.push(Span::styled(text.to_string(), convert_style(style)));
        }
        if spans.is_empty() {
            spans.push(Span::raw(String::new()));
        }
        lines.push(spans);
    }

    Some(lines)
}

/// Parse `code` using syntect for `lang` and return per-line styled spans.
/// Each inner Vec represents one source line.  Returns None when the language
/// is not recognized or the input exceeds safety limits.
pub(super) fn highlight_to_line_spans(code: &str, lang: &str) -> Option<Vec<Vec<Span<'static>>>> {
    let theme_guard = match theme_lock().read() {
        Ok(theme_guard) => theme_guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    highlight_to_line_spans_with_theme(code, lang, &theme_guard)
}

// -- Public API ---------------------------------------------------------------

/// Highlight code in any supported language, returning styled ratatui `Line`s.
///
/// Falls back to plain unstyled text when the language is not recognized or the
/// input exceeds safety guardrails.  Callers can always render the result
/// directly -- the fallback path produces equivalent plain-text lines.
pub fn highlight_code_to_lines(code: &str, lang: &str) -> Vec<Line<'static>> {
    if let Some(line_spans) = highlight_to_line_spans(code, lang) {
        line_spans.into_iter().map(Line::from).collect()
    } else {
        // Fallback: plain text, one Line per source line.
        // Use `lines()` instead of `split('\n')` to avoid a phantom trailing
        // empty element when the input ends with '\n' (as pulldown-cmark emits).
        let mut result: Vec<Line<'static>> =
            code.lines().map(|l| Line::from(l.to_string())).collect();
        if result.is_empty() {
            result.push(Line::from(String::new()));
        }
        result
    }
}

/// Backward-compatible wrapper for bash highlighting used by exec cells.
pub fn highlight_bash_to_lines(script: &str) -> Vec<Line<'static>> {
    highlight_code_to_lines(script, "bash")
}

/// Highlight code and return per-line styled spans for diff integration.
///
/// Returns `None` when the language is unrecognized or the input exceeds
/// guardrails.  The caller (`diff_render`) uses this signal to fall back to
/// plain diff coloring.
pub fn highlight_code_to_styled_spans(code: &str, lang: &str) -> Option<Vec<Vec<Span<'static>>>> {
    highlight_to_line_spans(code, lang)
}
