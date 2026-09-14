use super::*;
use std::path::PathBuf;

fn test_cwd() -> PathBuf {
    // These tests only need a stable absolute cwd; using temp_dir() avoids baking Unix- or
    // Windows-specific root semantics into the fixtures.
    std::env::temp_dir()
}

fn lines_to_plain_strings(lines: &[ratatui::text::Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

pub(crate) async fn controller_loose_vs_tight_with_commit_ticks_matches_full() {
    let mut ctrl = StreamController::new(None, &test_cwd());
    let mut lines = Vec::new();

    // Exact deltas from the session log (section: Loose vs. tight list items)
    let deltas = vec![
        "\n\n",
        "Loose",
        " vs",
        ".",
        " tight",
        " list",
        " items",
        ":\n",
        "1",
        ".",
        " Tight",
        " item",
        "\n",
        "2",
        ".",
        " Another",
        " tight",
        " item",
        "\n\n",
        "1",
        ".",
        " Loose",
        " item",
        " with",
        " its",
        " own",
        " paragraph",
        ".\n\n",
        "  ",
        " This",
        " paragraph",
        " belongs",
        " to",
        " the",
        " same",
        " list",
        " item",
        ".\n\n",
        "2",
        ".",
        " Second",
        " loose",
        " item",
        " with",
        " a",
        " nested",
        " list",
        " after",
        " a",
        " blank",
        " line",
        ".\n\n",
        "  ",
        " -",
        " Nested",
        " bullet",
        " under",
        " a",
        " loose",
        " item",
        "\n",
        "  ",
        " -",
        " Another",
        " nested",
        " bullet",
        "\n\n",
    ];

    // Simulate streaming with a commit tick attempt after each delta.
    for d in deltas.iter() {
        ctrl.push(d);
        while let (Some(cell), idle) = ctrl.on_commit_tick() {
            lines.extend(cell.transcript_lines(u16::MAX));
            if idle {
                break;
            }
        }
    }
    // Finalize and flush remaining lines now.
    if let Some(cell) = ctrl.finalize() {
        lines.extend(cell.transcript_lines(u16::MAX));
    }

    let streamed: Vec<_> = lines_to_plain_strings(&lines)
        .into_iter()
        // skip • and 2-space indentation
        .map(|s| s.chars().skip(2).collect::<String>())
        .collect();

    // Full render of the same source
    let source: String = deltas.iter().copied().collect();
    let mut rendered: Vec<ratatui::text::Line<'static>> = Vec::new();
    let test_cwd = test_cwd();
    crate::markdown::append_markdown(&source, None, Some(test_cwd.as_path()), &mut rendered);
    let rendered_strs = lines_to_plain_strings(&rendered);

    assert_eq!(streamed, rendered_strs);

    // Also assert exact expected plain strings for clarity.
    let expected = vec![
        "Loose vs. tight list items:".to_string(),
        "".to_string(),
        "1. Tight item".to_string(),
        "2. Another tight item".to_string(),
        "3. Loose item with its own paragraph.".to_string(),
        "".to_string(),
        "   This paragraph belongs to the same list item.".to_string(),
        "4. Second loose item with a nested list after a blank line.".to_string(),
        "    - Nested bullet under a loose item".to_string(),
        "    - Another nested bullet".to_string(),
    ];
    assert_eq!(
        streamed, expected,
        "expected exact rendered lines for loose/tight section"
    );
}
