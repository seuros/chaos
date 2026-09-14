use super::*;
use crate::test_support::buffer_lines;
use crate::test_support::renderable_buffer_with_size;
use pretty_assertions::assert_eq;
use ratatui::style::Modifier;

fn render_lines(renderable: &dyn Renderable, width: u16, height: u16) -> Vec<String> {
    let buf = renderable_buffer_with_size(renderable, width, height);
    buffer_lines(&buf)
}

fn first_non_space_style_after_marker(buf: &Buffer, row: u16, width: u16) -> Option<Modifier> {
    let marker_col = (0..width)
        .find(|&col| buf[(col, row)].symbol() == "-" || buf[(col, row)].symbol() == "+")?;
    for col in marker_col + 1..width {
        if buf[(col, row)].symbol() != " " {
            return Some(buf[(col, row)].style().add_modifier);
        }
    }
    None
}

fn preview_line_number(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let digits_len = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits_len == 0 {
        return None;
    }
    let digits = &trimmed[..digits_len];
    if !trimmed[digits_len..].starts_with(' ') {
        return None;
    }
    digits.parse::<usize>().ok()
}

fn preview_line_marker(line: &str) -> Option<char> {
    let trimmed = line.trim_start();
    let digits_len = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits_len == 0 {
        return None;
    }
    let mut chars = trimmed[digits_len..].chars();
    if chars.next()? != ' ' {
        return None;
    }
    chars.next()
}

pub(crate) fn theme_picker_suite() {
    theme_picker_uses_half_width_with_stacked_fallback_preview();
    theme_picker_items_include_search_values_for_preview_mapping();
    wide_preview_renders_all_lines_with_vertical_center_and_left_inset();
    narrow_preview_renders_single_add_and_single_remove_in_four_lines();
    deleted_preview_code_uses_dim_overlay_like_real_diff_renderer();
    subtitle_uses_tilde_path_when_codex_home_under_home_directory();
    subtitle_falls_back_when_tilde_path_subtitle_is_too_wide();
    subtitle_falls_back_to_preview_instructions_without_tilde_path();
    subtitle_falls_back_for_94_column_terminal_side_by_side_layout();
    unavailable_configured_theme_falls_back_to_configured_or_default_selection();
}
#[cfg(test)]
fn theme_picker_uses_half_width_with_stacked_fallback_preview() {
    let params = build_theme_picker_params(None, None, None);
    assert_eq!(params.side_content_width, SideContentWidth::Half);
    assert_eq!(params.side_content_min_width, WIDE_PREVIEW_MIN_WIDTH);
    assert!(params.stacked_side_content.is_some());
}

#[cfg(test)]
fn theme_picker_items_include_search_values_for_preview_mapping() {
    let params = build_theme_picker_params(None, None, None);
    assert!(
        params.items.iter().all(|item| item.search_value.is_some()),
        "theme picker preview mapping relies on item search_value to stay aligned with final item order"
    );
}

#[cfg(test)]
fn wide_preview_renders_all_lines_with_vertical_center_and_left_inset() {
    let lines = render_lines(&ThemePreviewWideRenderable, 80, 20);
    let numbered_rows: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| preview_line_number(line).map(|_| idx))
        .collect();
    let total_preview_lines = WIDE_PREVIEW_ROWS.len();

    assert_eq!(numbered_rows.len(), total_preview_lines);
    let first_row = *numbered_rows
        .first()
        .expect("expected at least one preview row");
    let last_row = *numbered_rows
        .last()
        .expect("expected at least one preview row");
    assert!(
        first_row > 0,
        "expected top padding before centered preview"
    );
    assert!(
        last_row < 19,
        "expected bottom padding after centered preview"
    );

    let first_line = &lines[first_row];
    assert!(
        first_line.starts_with("  31  fn summarize"),
        "expected wide preview to start after a 2-char inset"
    );

    let markers: Vec<char> = lines
        .iter()
        .filter_map(|line| preview_line_marker(line))
        .collect();
    assert!(
        markers.contains(&'+'),
        "expected wide preview to include at least one addition line"
    );
    assert!(
        markers.contains(&'-'),
        "expected wide preview to include at least one removal line"
    );
}

#[cfg(test)]
fn narrow_preview_renders_single_add_and_single_remove_in_four_lines() {
    let lines = render_lines(&ThemePreviewNarrowRenderable, 80, 6);
    let numbered_lines: Vec<usize> = lines
        .iter()
        .filter_map(|line| preview_line_number(line))
        .collect();
    let markers: Vec<char> = lines
        .iter()
        .filter_map(|line| preview_line_marker(line))
        .collect();

    assert_eq!(numbered_lines, vec![12, 13, 13, 14]);
    assert_eq!(markers.len(), 4);
    assert_eq!(markers.iter().filter(|&&m| m == '+').count(), 1);
    assert_eq!(markers.iter().filter(|&&m| m == '-').count(), 1);
    let first_numbered = lines
        .iter()
        .find(|line| preview_line_number(line).is_some())
        .expect("expected at least one rendered preview row");
    assert!(
        first_numbered.starts_with("12  fn greet"),
        "expected narrow preview line numbers to start at the left edge"
    );
}

#[cfg(test)]
fn deleted_preview_code_uses_dim_overlay_like_real_diff_renderer() {
    let width = 80;
    let height = 6;
    let buf = renderable_buffer_with_size(&ThemePreviewNarrowRenderable, width, height);
    let lines = render_lines(&ThemePreviewNarrowRenderable, width, height);
    let deleted_row = lines
        .iter()
        .enumerate()
        .find_map(|(row, line)| (preview_line_marker(line) == Some('-')).then_some(row as u16))
        .expect("expected a deleted preview row");
    let modifiers = first_non_space_style_after_marker(&buf, deleted_row, width)
        .expect("expected code text after diff marker");
    assert!(
        modifiers.contains(Modifier::DIM),
        "expected deleted preview code to be dimmed"
    );
}

#[cfg(test)]
fn subtitle_uses_tilde_path_when_codex_home_under_home_directory() {
    let home = dirs::home_dir().expect("home directory should be available");
    let chaos_home = home.join(".chaos");

    let subtitle = theme_picker_subtitle(Some(&chaos_home), Some(200));

    assert!(subtitle.contains("~"));
    assert!(subtitle.contains("directory"));
}

#[cfg(test)]
fn subtitle_falls_back_when_tilde_path_subtitle_is_too_wide() {
    let home = dirs::home_dir().expect("home directory should be available");
    let long_segment = "a".repeat(120);
    let chaos_home = home.join(long_segment).join(".chaos");

    let subtitle = theme_picker_subtitle(Some(&chaos_home), Some(140));

    assert_eq!(subtitle, PREVIEW_FALLBACK_SUBTITLE);
}

#[cfg(test)]
fn subtitle_falls_back_to_preview_instructions_without_tilde_path() {
    let subtitle = theme_picker_subtitle(None, None);
    assert_eq!(subtitle, PREVIEW_FALLBACK_SUBTITLE);
}

#[cfg(test)]
fn subtitle_falls_back_for_94_column_terminal_side_by_side_layout() {
    let home = dirs::home_dir().expect("home directory should be available");
    let chaos_home = home.join(".chaos");

    let subtitle = theme_picker_subtitle(Some(&chaos_home), Some(94));

    assert_eq!(subtitle, PREVIEW_FALLBACK_SUBTITLE);
}

#[cfg(test)]
fn unavailable_configured_theme_falls_back_to_configured_or_default_selection() {
    let configured_or_default_theme = highlight::configured_theme_name();
    let params = build_theme_picker_params(Some("not-a-real-theme"), None, Some(120));
    let selected_idx = params
        .initial_selected_idx
        .expect("expected selected index for active fallback theme");
    let selected_name = params.items[selected_idx]
        .search_value
        .as_deref()
        .expect("expected search value to contain canonical theme name");

    assert_eq!(selected_name, configured_or_default_theme);
}
