//! Renders unified diffs with line numbers, gutter signs, and optional syntax
//! highlighting.
//!
//! Each `FileChange` variant (Add / Delete / Update) is rendered as a block of
//! diff lines, each prefixed by a right-aligned line number, a gutter sign
//! (`+` / `-` / ` `), and the content text.  When a recognized file extension
//! is present, the content text is syntax-highlighted using
//! [`crate::render::highlight`].
//!
//! **Theme-aware styling:** diff backgrounds adapt to the terminal's
//! background lightness via [`DiffTheme`].  Dark terminals get muted tints
//! (`#212922` green, `#3C170F` red); light terminals get GitHub-style pastels
//! with distinct gutter backgrounds for contrast. The renderer uses fixed
//! palettes for truecolor / 256-color / 16-color terminals so add/delete lines
//! remain visually distinct even when quantizing to limited palettes.
//!
//! **Syntax-theme scope backgrounds:** when the active syntax theme defines
//! background colors for `markup.inserted` / `markup.deleted` (or fallback
//! `diff.inserted` / `diff.deleted`) scopes, those colors override the
//! hardcoded palette for rich color levels.  ANSI-16 mode always uses
//! foreground-only styling regardless of theme scope backgrounds.
//!
//! **Highlighting strategy for `Update` diffs:** the renderer highlights each
//! hunk as a single concatenated block rather than line-by-line.  This
//! preserves syntect's parser state across consecutive lines within a hunk
//! (important for multi-line strings, block comments, etc.).  Cross-hunk state
//! is intentionally *not* preserved because hunks are visually separated and
//! re-synchronize at context boundaries anyway.
//!
//! **Wrapping:** long lines are hard-wrapped at the available column width.
//! Syntax-highlighted spans are split at character boundaries with styles
//! preserved across the split so that no color information is lost.

mod inline;
mod side_by_side;
mod utils;

pub use inline::{
    push_wrapped_diff_line_with_style_context, push_wrapped_diff_line_with_syntax_and_style_context,
};
pub use side_by_side::{
    DiffSummary, calculate_add_remove_from_diff, create_diff_summary, display_path_for,
};
pub use utils::{
    DiffLineType, DiffRenderStyleContext, current_diff_render_style_context, line_number_width,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::render_test_backend_debug;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::prelude::Widget;
    use ratatui::style::Color;
    use ratatui::style::Style;
    use ratatui::text::Text;
    use ratatui::widgets::Paragraph;
    use ratatui::widgets::Wrap;
    use std::collections::HashMap;
    use std::path::Path;
    use std::path::PathBuf;

    use crate::render::highlight::DiffScopeBackgroundRgbs;
    use crate::render::highlight::highlight_code_to_styled_spans;
    use crate::terminal_palette::StdoutColorLevel;
    use crate::terminal_palette::indexed_color;
    use crate::terminal_palette::rgb_color;

    use utils::{
        DARK_256_ADD_LINE_BG_IDX, DARK_256_DEL_LINE_BG_IDX, DARK_TC_ADD_LINE_BG_RGB,
        DARK_TC_DEL_LINE_BG_RGB, DiffColorLevel, DiffTheme, LIGHT_TC_ADD_LINE_BG_RGB,
        LIGHT_TC_ADD_NUM_BG_RGB, LIGHT_TC_DEL_LINE_BG_RGB, LIGHT_TC_DEL_NUM_BG_RGB,
        LIGHT_TC_GUTTER_FG_RGB, TAB_WIDTH, diff_color_level_for_terminal,
        fallback_diff_backgrounds, resolve_diff_backgrounds_for, style_add, style_del,
        style_gutter_dim, style_gutter_for, style_line_bg_for, style_sign_add, style_sign_del,
    };

    use inline::{
        detect_lang_for_path, push_wrapped_diff_line_inner_with_theme_and_color_level,
        wrap_styled_spans,
    };

    fn diff_summary_for_tests(
        changes: &HashMap<PathBuf, FileChange>,
    ) -> Vec<ratatui::text::Line<'static>> {
        create_diff_summary(changes, &PathBuf::from("/"), 80)
    }

    fn snapshot_lines(
        name: &str,
        lines: Vec<ratatui::text::Line<'static>>,
        width: u16,
        height: u16,
    ) {
        assert_snapshot!(
            name,
            render_test_backend_debug(width, height, |f| {
                Paragraph::new(Text::from(lines))
                    .wrap(Wrap { trim: false })
                    .render(f.area(), f.buffer_mut());
            })
        );
    }

    fn display_width(text: &str) -> usize {
        text.chars()
            .map(|ch| ch.width().unwrap_or(if ch == '\t' { TAB_WIDTH } else { 0 }))
            .sum()
    }

    fn line_display_width(line: &ratatui::text::Line<'static>) -> usize {
        line.spans
            .iter()
            .map(|span| display_width(span.content.as_ref()))
            .sum()
    }

    fn snapshot_lines_text(name: &str, lines: &[ratatui::text::Line<'static>]) {
        let text = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .map(|s| s.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_snapshot!(name, text);
    }

    fn diff_gallery_changes() -> HashMap<PathBuf, FileChange> {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();

        let rust_original =
            "fn greet(name: &str) {\n    println!(\"hello\");\n    println!(\"bye\");\n}\n";
        let rust_modified = "fn greet(name: &str) {\n    println!(\"hello {name}\");\n    println!(\"emoji: 🚀✨ and CJK: 你好世界\");\n}\n";
        let rust_patch = diffy::create_patch(rust_original, rust_modified).to_string();
        changes.insert(
            PathBuf::from("src/lib.rs"),
            FileChange::Update {
                unified_diff: rust_patch,
                move_path: None,
            },
        );

        let py_original = "def add(a, b):\n\treturn a + b\n\nprint(add(1, 2))\n";
        let py_modified = "def add(a, b):\n\treturn a + b + 42\n\nprint(add(1, 2))\n";
        let py_patch = diffy::create_patch(py_original, py_modified).to_string();
        changes.insert(
            PathBuf::from("scripts/calc.txt"),
            FileChange::Update {
                unified_diff: py_patch,
                move_path: Some(PathBuf::from("scripts/calc.py")),
            },
        );

        changes.insert(
            PathBuf::from("assets/banner.txt"),
            FileChange::Add {
                content: "HEADER\tVALUE\nrocket\t🚀\ncity\t東京\n".to_string(),
            },
        );
        changes.insert(
            PathBuf::from("examples/new_sample.rs"),
            FileChange::Add {
                content: "pub fn greet(name: &str) {\n    println!(\"Hello, {name}!\");\n}\n"
                    .to_string(),
            },
        );

        changes.insert(
            PathBuf::from("tmp/obsolete.log"),
            FileChange::Delete {
                content: "old line 1\nold line 2\nold line 3\n".to_string(),
            },
        );
        changes.insert(
            PathBuf::from("legacy/old_script.py"),
            FileChange::Delete {
                content: "def legacy(x):\n    return x + 1\nprint(legacy(3))\n".to_string(),
            },
        );

        changes
    }

    fn snapshot_diff_gallery(name: &str, width: u16, height: u16) {
        let lines = create_diff_summary(
            &diff_gallery_changes(),
            &PathBuf::from("/"),
            usize::from(width),
        );
        snapshot_lines(name, lines, width, height);
    }

    use chaos_ipc::protocol::FileChange;
    use unicode_width::UnicodeWidthChar;

    #[test]
    fn ansi16_add_style_uses_foreground_only() {
        let style = style_add(
            DiffTheme::Dark,
            DiffColorLevel::Ansi16,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        );
        assert_eq!(style.fg, Some(Color::Green));
        assert_eq!(style.bg, None);
    }

    #[test]
    fn ansi16_del_style_uses_foreground_only() {
        let style = style_del(
            DiffTheme::Dark,
            DiffColorLevel::Ansi16,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        );
        assert_eq!(style.fg, Some(Color::Red));
        assert_eq!(style.bg, None);
    }

    #[test]
    fn ansi16_sign_styles_use_foreground_only() {
        let add_sign = style_sign_add(
            DiffTheme::Dark,
            DiffColorLevel::Ansi16,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        );
        assert_eq!(add_sign.fg, Some(Color::Green));
        assert_eq!(add_sign.bg, None);

        let del_sign = style_sign_del(
            DiffTheme::Dark,
            DiffColorLevel::Ansi16,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        );
        assert_eq!(del_sign.fg, Some(Color::Red));
        assert_eq!(del_sign.bg, None);
    }

    #[test]
    fn display_path_prefers_cwd_without_git_repo() {
        let cwd = PathBuf::from("/workspace/chaos");
        let path = cwd.join("tui").join("example.png");

        let rendered = display_path_for(&path, &cwd);

        assert_eq!(
            rendered,
            PathBuf::from("tui")
                .join("example.png")
                .display()
                .to_string()
        );
    }

    #[test]
    fn ui_snapshot_wrap_behavior_insert() {
        let long_line = "this is a very long line that should wrap across multiple terminal columns and continue";

        let lines = push_wrapped_diff_line_with_style_context(
            1,
            DiffLineType::Insert,
            long_line,
            80,
            line_number_width(1),
            current_diff_render_style_context(),
        );

        snapshot_lines("wrap_behavior_insert", lines, 90, 8);
    }

    #[test]
    fn ui_snapshot_apply_update_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        let original = "line one\nline two\nline three\n";
        let modified = "line one\nline two changed\nline three\n";
        let patch = diffy::create_patch(original, modified).to_string();

        changes.insert(
            PathBuf::from("example.txt"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = diff_summary_for_tests(&changes);

        snapshot_lines("apply_update_block", lines, 80, 12);
    }

    #[test]
    fn ui_snapshot_apply_update_with_rename_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        let original = "A\nB\nC\n";
        let modified = "A\nB changed\nC\n";
        let patch = diffy::create_patch(original, modified).to_string();

        changes.insert(
            PathBuf::from("old_name.rs"),
            FileChange::Update {
                unified_diff: patch,
                move_path: Some(PathBuf::from("new_name.rs")),
            },
        );

        let lines = diff_summary_for_tests(&changes);

        snapshot_lines("apply_update_with_rename_block", lines, 80, 12);
    }

    #[test]
    fn ui_snapshot_apply_multiple_files_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();

        let patch_a = diffy::create_patch("one\n", "one changed\n").to_string();
        changes.insert(
            PathBuf::from("a.txt"),
            FileChange::Update {
                unified_diff: patch_a,
                move_path: None,
            },
        );

        changes.insert(
            PathBuf::from("b.txt"),
            FileChange::Add {
                content: "new\n".to_string(),
            },
        );

        let lines = diff_summary_for_tests(&changes);

        snapshot_lines("apply_multiple_files_block", lines, 80, 14);
    }

    #[test]
    fn ui_snapshot_apply_add_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("new_file.txt"),
            FileChange::Add {
                content: "alpha\nbeta\n".to_string(),
            },
        );

        let lines = diff_summary_for_tests(&changes);

        snapshot_lines("apply_add_block", lines, 80, 10);
    }

    #[test]
    fn ui_snapshot_apply_delete_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("tmp_delete_example.txt"),
            FileChange::Delete {
                content: "first\nsecond\nthird\n".to_string(),
            },
        );

        let lines = diff_summary_for_tests(&changes);
        snapshot_lines("apply_delete_block", lines, 80, 12);
    }

    #[test]
    fn ui_snapshot_apply_update_block_wraps_long_lines() {
        let original = "line 1\nshort\nline 3\n";
        let modified = "line 1\nshort this_is_a_very_long_modified_line_that_should_wrap_across_multiple_terminal_columns_and_continue_even_further_beyond_eighty_columns_to_force_multiple_wraps\nline 3\n";
        let patch = diffy::create_patch(original, modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("long_example.txt"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 72);

        snapshot_lines("apply_update_block_wraps_long_lines", lines, 80, 12);
    }

    #[test]
    fn ui_snapshot_apply_update_block_wraps_long_lines_text() {
        let original = "1\n2\n3\n4\n";
        let modified = "1\nadded long line which wraps and_if_there_is_a_long_token_it_will_be_broken\n3\n4 context line which also wraps across\n";
        let patch = diffy::create_patch(original, modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("wrap_demo.txt"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 28);
        snapshot_lines_text("apply_update_block_wraps_long_lines_text", &lines);
    }

    #[test]
    fn ui_snapshot_apply_update_block_line_numbers_three_digits_text() {
        let original = (1..=110).map(|i| format!("line {i}\n")).collect::<String>();
        let modified = (1..=110)
            .map(|i| {
                if i == 100 {
                    format!("line {i} changed\n")
                } else {
                    format!("line {i}\n")
                }
            })
            .collect::<String>();
        let patch = diffy::create_patch(&original, &modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("hundreds.txt"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 80);
        snapshot_lines_text("apply_update_block_line_numbers_three_digits_text", &lines);
    }

    #[test]
    fn ui_snapshot_apply_update_block_relativizes_path() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let abs_old = cwd.join("abs_old.rs");
        let abs_new = cwd.join("abs_new.rs");

        let original = "X\nY\n";
        let modified = "X changed\nY\n";
        let patch = diffy::create_patch(original, modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            abs_old,
            FileChange::Update {
                unified_diff: patch,
                move_path: Some(abs_new),
            },
        );

        let lines = create_diff_summary(&changes, &cwd, 80);

        snapshot_lines("apply_update_block_relativizes_path", lines, 80, 10);
    }

    #[test]
    fn ui_snapshot_syntax_highlighted_insert_wraps() {
        let long_rust = "fn very_long_function_name(arg_one: String, arg_two: String, arg_three: String, arg_four: String) -> Result<String, Box<dyn std::error::Error>> { Ok(arg_one) }";

        let syntax_spans =
            highlight_code_to_styled_spans(long_rust, "rust").expect("rust highlighting");
        let spans = &syntax_spans[0];

        let lines = push_wrapped_diff_line_with_syntax_and_style_context(
            1,
            DiffLineType::Insert,
            long_rust,
            80,
            line_number_width(1),
            spans,
            current_diff_render_style_context(),
        );

        assert!(
            lines.len() > 1,
            "syntax-highlighted long line should wrap to multiple lines, got {}",
            lines.len()
        );

        snapshot_lines("syntax_highlighted_insert_wraps", lines, 90, 10);
    }

    #[test]
    fn ui_snapshot_syntax_highlighted_insert_wraps_text() {
        let long_rust = "fn very_long_function_name(arg_one: String, arg_two: String, arg_three: String, arg_four: String) -> Result<String, Box<dyn std::error::Error>> { Ok(arg_one) }";

        let syntax_spans =
            highlight_code_to_styled_spans(long_rust, "rust").expect("rust highlighting");
        let spans = &syntax_spans[0];

        let lines = push_wrapped_diff_line_with_syntax_and_style_context(
            1,
            DiffLineType::Insert,
            long_rust,
            80,
            line_number_width(1),
            spans,
            current_diff_render_style_context(),
        );

        snapshot_lines_text("syntax_highlighted_insert_wraps_text", &lines);
    }

    #[test]
    fn ui_snapshot_diff_gallery_80x24() {
        snapshot_diff_gallery("diff_gallery_80x24", 80, 24);
    }

    #[test]
    fn ui_snapshot_diff_gallery_94x35() {
        snapshot_diff_gallery("diff_gallery_94x35", 94, 35);
    }

    #[test]
    fn ui_snapshot_diff_gallery_120x40() {
        snapshot_diff_gallery("diff_gallery_120x40", 120, 40);
    }

    #[test]
    fn ui_snapshot_ansi16_insert_delete_no_background() {
        let mut lines = push_wrapped_diff_line_inner_with_theme_and_color_level(
            1,
            DiffLineType::Insert,
            "added in ansi16 mode",
            80,
            line_number_width(2),
            None,
            DiffTheme::Dark,
            DiffColorLevel::Ansi16,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        );
        lines.extend(push_wrapped_diff_line_inner_with_theme_and_color_level(
            2,
            DiffLineType::Delete,
            "deleted in ansi16 mode",
            80,
            line_number_width(2),
            None,
            DiffTheme::Dark,
            DiffColorLevel::Ansi16,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        ));

        snapshot_lines("ansi16_insert_delete_no_background", lines, 40, 4);
    }

    #[test]
    fn truecolor_dark_theme_uses_configured_backgrounds() {
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Insert,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::TrueColor)
            ),
            Style::default().bg(rgb_color(DARK_TC_ADD_LINE_BG_RGB))
        );
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Delete,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::TrueColor)
            ),
            Style::default().bg(rgb_color(DARK_TC_DEL_LINE_BG_RGB))
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Insert,
                DiffTheme::Dark,
                DiffColorLevel::TrueColor
            ),
            style_gutter_dim()
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Delete,
                DiffTheme::Dark,
                DiffColorLevel::TrueColor
            ),
            style_gutter_dim()
        );
    }

    #[test]
    fn ansi256_dark_theme_uses_distinct_add_and_delete_backgrounds() {
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Insert,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi256)
            ),
            Style::default().bg(indexed_color(DARK_256_ADD_LINE_BG_IDX))
        );
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Delete,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi256)
            ),
            Style::default().bg(indexed_color(DARK_256_DEL_LINE_BG_IDX))
        );
        assert_ne!(
            style_line_bg_for(
                DiffLineType::Insert,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi256)
            ),
            style_line_bg_for(
                DiffLineType::Delete,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi256)
            ),
            "256-color mode should keep add/delete backgrounds distinct"
        );
    }

    #[test]
    fn theme_scope_backgrounds_override_truecolor_fallback_when_available() {
        let backgrounds = resolve_diff_backgrounds_for(
            DiffTheme::Dark,
            DiffColorLevel::TrueColor,
            DiffScopeBackgroundRgbs {
                inserted: Some((1, 2, 3)),
                deleted: Some((4, 5, 6)),
            },
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Insert, backgrounds),
            Style::default().bg(rgb_color((1, 2, 3)))
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Delete, backgrounds),
            Style::default().bg(rgb_color((4, 5, 6)))
        );
    }

    #[test]
    fn theme_scope_backgrounds_quantize_to_ansi256() {
        let backgrounds = resolve_diff_backgrounds_for(
            DiffTheme::Dark,
            DiffColorLevel::Ansi256,
            DiffScopeBackgroundRgbs {
                inserted: Some((0, 95, 0)),
                deleted: None,
            },
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Insert, backgrounds),
            Style::default().bg(indexed_color(22))
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Delete, backgrounds),
            Style::default().bg(indexed_color(DARK_256_DEL_LINE_BG_IDX))
        );
    }

    #[test]
    fn ui_snapshot_theme_scope_background_resolution() {
        let backgrounds = resolve_diff_backgrounds_for(
            DiffTheme::Dark,
            DiffColorLevel::TrueColor,
            DiffScopeBackgroundRgbs {
                inserted: Some((12, 34, 56)),
                deleted: None,
            },
        );
        let snapshot = format!(
            "insert={:?}\ndelete={:?}",
            style_line_bg_for(DiffLineType::Insert, backgrounds).bg,
            style_line_bg_for(DiffLineType::Delete, backgrounds).bg,
        );
        assert_snapshot!("theme_scope_background_resolution", snapshot);
    }

    #[test]
    fn ansi16_disables_line_and_gutter_backgrounds() {
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Insert,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16)
            ),
            Style::default()
        );
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Delete,
                fallback_diff_backgrounds(DiffTheme::Light, DiffColorLevel::Ansi16)
            ),
            Style::default()
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Insert,
                DiffTheme::Light,
                DiffColorLevel::Ansi16
            ),
            Style::default().fg(Color::Black)
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Delete,
                DiffTheme::Light,
                DiffColorLevel::Ansi16
            ),
            Style::default().fg(Color::Black)
        );
        let themed_backgrounds = resolve_diff_backgrounds_for(
            DiffTheme::Light,
            DiffColorLevel::Ansi16,
            DiffScopeBackgroundRgbs {
                inserted: Some((8, 9, 10)),
                deleted: Some((11, 12, 13)),
            },
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Insert, themed_backgrounds),
            Style::default()
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Delete, themed_backgrounds),
            Style::default()
        );
    }

    #[test]
    fn light_truecolor_theme_uses_readable_gutter_and_line_backgrounds() {
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Insert,
                fallback_diff_backgrounds(DiffTheme::Light, DiffColorLevel::TrueColor)
            ),
            Style::default().bg(rgb_color(LIGHT_TC_ADD_LINE_BG_RGB))
        );
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Delete,
                fallback_diff_backgrounds(DiffTheme::Light, DiffColorLevel::TrueColor)
            ),
            Style::default().bg(rgb_color(LIGHT_TC_DEL_LINE_BG_RGB))
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Insert,
                DiffTheme::Light,
                DiffColorLevel::TrueColor
            ),
            Style::default()
                .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
                .bg(rgb_color(LIGHT_TC_ADD_NUM_BG_RGB))
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Delete,
                DiffTheme::Light,
                DiffColorLevel::TrueColor
            ),
            Style::default()
                .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
                .bg(rgb_color(LIGHT_TC_DEL_NUM_BG_RGB))
        );
    }

    #[test]
    fn light_theme_wrapped_lines_keep_number_gutter_contrast() {
        let lines = push_wrapped_diff_line_inner_with_theme_and_color_level(
            12,
            DiffLineType::Insert,
            "abcdefghij",
            8,
            line_number_width(12),
            None,
            DiffTheme::Light,
            DiffColorLevel::TrueColor,
            fallback_diff_backgrounds(DiffTheme::Light, DiffColorLevel::TrueColor),
        );

        assert!(
            lines.len() > 1,
            "expected wrapped output for gutter style verification"
        );
        assert_eq!(
            lines[0].spans[0].style,
            Style::default()
                .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
                .bg(rgb_color(LIGHT_TC_ADD_NUM_BG_RGB))
        );
        assert_eq!(
            lines[1].spans[0].style,
            Style::default()
                .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
                .bg(rgb_color(LIGHT_TC_ADD_NUM_BG_RGB))
        );
        assert_eq!(lines[0].style.bg, Some(rgb_color(LIGHT_TC_ADD_LINE_BG_RGB)));
        assert_eq!(lines[1].style.bg, Some(rgb_color(LIGHT_TC_ADD_LINE_BG_RGB)));
    }

    #[test]
    fn ansi16_maps_to_ansi16_diff_palette() {
        assert_eq!(
            diff_color_level_for_terminal(StdoutColorLevel::Ansi16),
            DiffColorLevel::Ansi16
        );
    }

    #[test]
    fn add_diff_uses_path_extension_for_highlighting() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("highlight_add.rs"),
            FileChange::Add {
                content: "pub fn sum(a: i32, b: i32) -> i32 { a + b }\n".to_string(),
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 80);
        let has_rgb = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
        });
        assert!(
            has_rgb,
            "add diff for .rs file should produce syntax-highlighted (RGB) spans"
        );
    }

    #[test]
    fn delete_diff_uses_path_extension_for_highlighting() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("highlight_delete.py"),
            FileChange::Delete {
                content: "def scale(x):\n    return x * 2\n".to_string(),
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 80);
        let has_rgb = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
        });
        assert!(
            has_rgb,
            "delete diff for .py file should produce syntax-highlighted (RGB) spans"
        );
    }

    #[test]
    fn detect_lang_for_common_paths() {
        assert!(detect_lang_for_path(Path::new("foo.rs")).is_some());
        assert!(detect_lang_for_path(Path::new("bar.py")).is_some());
        assert!(detect_lang_for_path(Path::new("app.tsx")).is_some());

        assert!(detect_lang_for_path(Path::new("Makefile")).is_none());
        assert!(detect_lang_for_path(Path::new("randomfile")).is_none());
    }

    #[test]
    fn wrap_styled_spans_single_line() {
        let spans = vec![ratatui::text::Span::raw("short")];
        let result = wrap_styled_spans(&spans, 80);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn wrap_styled_spans_splits_long_content() {
        let long_text = "a".repeat(100);
        let spans = vec![ratatui::text::Span::raw(long_text)];
        let result = wrap_styled_spans(&spans, 40);
        assert!(
            result.len() >= 3,
            "100 chars at 40 cols should produce at least 3 lines, got {}",
            result.len()
        );
    }

    #[test]
    fn wrap_styled_spans_flushes_at_span_boundary() {
        let style_a = Style::default().fg(Color::Red);
        let style_b = Style::default().fg(Color::Blue);
        let spans = vec![
            ratatui::text::Span::styled("aaaa", style_a),
            ratatui::text::Span::styled("bb", style_b),
        ];
        let result = wrap_styled_spans(&spans, 4);
        assert_eq!(
            result.len(),
            2,
            "span ending exactly at max_cols should flush before next span: {result:?}"
        );
        let first_width: usize = result[0].iter().map(|s| s.content.chars().count()).sum();
        assert!(
            first_width <= 4,
            "first line should be at most 4 cols wide, got {first_width}"
        );
    }

    #[test]
    fn wrap_styled_spans_preserves_styles() {
        let style = Style::default().fg(Color::Green);
        let text = "x".repeat(50);
        let spans = vec![ratatui::text::Span::styled(text, style)];
        let result = wrap_styled_spans(&spans, 20);
        for chunk in &result {
            for span in chunk {
                assert_eq!(span.style, style, "style should be preserved across wraps");
            }
        }
    }

    #[test]
    fn wrap_styled_spans_tabs_have_visible_width() {
        let spans = vec![ratatui::text::Span::raw("\tabcde")];
        let result = wrap_styled_spans(&spans, 8);
        assert!(
            result.len() >= 2,
            "tab + 5 chars should exceed 8 cols and wrap, got {} line(s): {result:?}",
            result.len()
        );
    }

    #[test]
    fn wrap_styled_spans_wraps_before_first_overflowing_char() {
        let spans = vec![ratatui::text::Span::raw("abcd\t界")];
        let result = wrap_styled_spans(&spans, 5);

        let line_text: Vec<String> = result
            .iter()
            .map(|line| {
                line.iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(line_text, vec!["abcd", "\t", "界"]);

        let line_width = |line: &[ratatui::text::Span<'static>]| -> usize {
            line.iter()
                .flat_map(|span| span.content.chars())
                .map(|ch| ch.width().unwrap_or(if ch == '\t' { TAB_WIDTH } else { 0 }))
                .sum()
        };
        for line in &result {
            assert!(
                line_width(line) <= 5,
                "wrapped line exceeded width 5: {line:?}"
            );
        }
    }

    #[test]
    fn fallback_wrapping_uses_display_width_for_tabs_and_wide_chars() {
        let width = 8;
        let lines = push_wrapped_diff_line_with_style_context(
            1,
            DiffLineType::Insert,
            "abcd\t界🙂",
            width,
            line_number_width(1),
            current_diff_render_style_context(),
        );

        assert!(lines.len() >= 2, "expected wrapped output, got {lines:?}");
        for line in &lines {
            assert!(
                line_display_width(line) <= width,
                "fallback wrapped line exceeded width {width}: {line:?}"
            );
        }
    }

    #[test]
    fn large_update_diff_skips_highlighting() {
        let line_count = 10_500;
        let original: String = (0..line_count).map(|i| format!("line {i}\n")).collect();
        let modified: String = (0..line_count)
            .map(|i| {
                if i % 2 == 0 {
                    format!("line {i} changed\n")
                } else {
                    format!("line {i}\n")
                }
            })
            .collect();
        let patch = diffy::create_patch(&original, &modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("huge.rs"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 80);

        assert!(
            lines.len() > 100,
            "expected many output lines from large diff, got {}",
            lines.len(),
        );

        for line in &lines {
            for span in &line.spans {
                if let Some(ratatui::style::Color::Rgb(..)) = span.style.fg {
                    panic!(
                        "large diff should not have syntax-highlighted spans, \
                         got RGB color in style {:?} for {:?}",
                        span.style, span.content,
                    );
                }
            }
        }
    }

    #[test]
    fn rename_diff_uses_destination_extension_for_highlighting() {
        let original = "fn main() {}\n";
        let modified = "fn main() { println!(\"hi\"); }\n";
        let patch = diffy::create_patch(original, modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("foo.xyzzy"),
            FileChange::Update {
                unified_diff: patch,
                move_path: Some(PathBuf::from("foo.rs")),
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 80);
        let has_rgb = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
        });
        assert!(
            has_rgb,
            "rename from .xyzzy to .rs should produce syntax-highlighted (RGB) spans"
        );
    }

    #[test]
    fn update_diff_preserves_multiline_highlight_state_within_hunk() {
        let original = "fn demo() {\n    let s = \"hello\";\n}\n";
        let modified = "fn demo() {\n    let s = \"hello\nworld\";\n}\n";
        let patch = diffy::create_patch(original, modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("demo.rs"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let expected_multiline =
            highlight_code_to_styled_spans("    let s = \"hello\nworld\";\n", "rust")
                .expect("rust highlighting");
        let expected_style = expected_multiline
            .get(1)
            .and_then(|line| {
                line.iter()
                    .find(|span| span.content.as_ref().contains("world"))
            })
            .map(|span| span.style)
            .expect("expected highlighted span for second multiline string line");

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 120);
        let actual_style = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref().contains("world"))
            .map(|span| span.style)
            .expect("expected rendered diff span containing 'world'");

        assert_eq!(actual_style, expected_style);
    }
}
