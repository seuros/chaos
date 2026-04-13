use chaos_kern::ProcessSortKey;
use ratatui::layout::Rect;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use crate::diff_render::display_path_for;
use crate::text_formatting::truncate_text;

use super::formatting::format_created_label;
use super::formatting::format_updated_label;
use super::row::Row;
use super::state::PickerState;

/// Pre-computed column widths and formatted labels for all visible rows.
///
/// Widths are measured in Unicode display width (not byte length) so columns
/// align correctly when labels contain non-ASCII characters.
pub(super) struct ColumnMetrics {
    pub(super) max_created_width: usize,
    pub(super) max_updated_width: usize,
    pub(super) max_branch_width: usize,
    pub(super) max_cwd_width: usize,
    /// (created_label, updated_label, branch_label, cwd_label) per row.
    pub(super) labels: Vec<(String, String, String, String)>,
}

/// Determines which columns to render given available terminal width.
///
/// When the terminal is narrow, only one timestamp column is shown (whichever
/// matches the current sort key). Branch and CWD are hidden if their max
/// widths are zero (no data to show).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ColumnVisibility {
    pub(super) show_created: bool,
    pub(super) show_updated: bool,
    pub(super) show_branch: bool,
    pub(super) show_cwd: bool,
}

pub(super) fn calculate_column_metrics(rows: &[Row], include_cwd: bool) -> ColumnMetrics {
    fn right_elide(s: &str, max: usize) -> String {
        if s.chars().count() <= max {
            return s.to_string();
        }
        if max <= 1 {
            return "…".to_string();
        }
        let tail_len = max - 1;
        let tail: String = s
            .chars()
            .rev()
            .take(tail_len)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("…{tail}")
    }

    let mut labels: Vec<(String, String, String, String)> = Vec::with_capacity(rows.len());
    let mut max_created_width = UnicodeWidthStr::width("Created at");
    let mut max_updated_width = UnicodeWidthStr::width("Updated at");
    let mut max_branch_width = UnicodeWidthStr::width("Branch");
    let mut max_cwd_width = if include_cwd {
        UnicodeWidthStr::width("CWD")
    } else {
        0
    };

    for row in rows {
        let created = format_created_label(row);
        let updated = format_updated_label(row);
        let branch_raw = row.git_branch.clone().unwrap_or_default();
        let branch = right_elide(&branch_raw, /*max*/ 24);
        let cwd = if include_cwd {
            let cwd_raw = row
                .cwd
                .as_ref()
                .map(|p| display_path_for(p, std::path::Path::new("/")))
                .unwrap_or_default();
            right_elide(&cwd_raw, /*max*/ 24)
        } else {
            String::new()
        };
        max_created_width = max_created_width.max(UnicodeWidthStr::width(created.as_str()));
        max_updated_width = max_updated_width.max(UnicodeWidthStr::width(updated.as_str()));
        max_branch_width = max_branch_width.max(UnicodeWidthStr::width(branch.as_str()));
        max_cwd_width = max_cwd_width.max(UnicodeWidthStr::width(cwd.as_str()));
        labels.push((created, updated, branch, cwd));
    }

    ColumnMetrics {
        max_created_width,
        max_updated_width,
        max_branch_width,
        max_cwd_width,
        labels,
    }
}

/// Computes which columns fit in the available width.
///
/// The algorithm reserves at least `MIN_PREVIEW_WIDTH` characters for the
/// conversation preview. If both timestamp columns don't fit, only the one
/// matching the current sort key is shown.
pub(super) fn column_visibility(
    area_width: u16,
    metrics: &ColumnMetrics,
    sort_key: ProcessSortKey,
) -> ColumnVisibility {
    const MIN_PREVIEW_WIDTH: usize = 10;

    let show_branch = metrics.max_branch_width > 0;
    let show_cwd = metrics.max_cwd_width > 0;

    let mut preview_width = area_width as usize;
    preview_width = preview_width.saturating_sub(2); // marker
    if metrics.max_created_width > 0 {
        preview_width = preview_width.saturating_sub(metrics.max_created_width + 2);
    }
    if metrics.max_updated_width > 0 {
        preview_width = preview_width.saturating_sub(metrics.max_updated_width + 2);
    }
    if show_branch {
        preview_width = preview_width.saturating_sub(metrics.max_branch_width + 2);
    }
    if show_cwd {
        preview_width = preview_width.saturating_sub(metrics.max_cwd_width + 2);
    }

    let show_both = preview_width >= MIN_PREVIEW_WIDTH;
    let show_created = if show_both {
        metrics.max_created_width > 0
    } else {
        sort_key == ProcessSortKey::CreatedAt
    };
    let show_updated = if show_both {
        metrics.max_updated_width > 0
    } else {
        sort_key == ProcessSortKey::UpdatedAt
    };

    ColumnVisibility {
        show_created,
        show_updated,
        show_branch,
        show_cwd,
    }
}

pub(super) fn search_line(state: &PickerState) -> Line<'_> {
    if let Some(error) = state.inline_error.as_deref() {
        return Line::from(error.red());
    }
    if state.query.is_empty() {
        return Line::from("Type to search".dim());
    }
    Line::from(format!("Search: {}", state.query))
}

pub(super) fn render_column_headers(
    frame: &mut crate::custom_terminal::Frame,
    area: Rect,
    metrics: &ColumnMetrics,
    sort_key: ProcessSortKey,
) {
    if area.height == 0 {
        return;
    }

    let mut spans: Vec<Span> = vec!["  ".into()];
    let visibility = column_visibility(area.width, metrics, sort_key);
    if visibility.show_created {
        let label = format!(
            "{text:<width$}",
            text = "Created at",
            width = metrics.max_created_width
        );
        spans.push(Span::from(label).bold());
        spans.push("  ".into());
    }
    if visibility.show_updated {
        let label = format!(
            "{text:<width$}",
            text = "Updated at",
            width = metrics.max_updated_width
        );
        spans.push(Span::from(label).bold());
        spans.push("  ".into());
    }
    if visibility.show_branch {
        let label = format!(
            "{text:<width$}",
            text = "Branch",
            width = metrics.max_branch_width
        );
        spans.push(Span::from(label).bold());
        spans.push("  ".into());
    }
    if visibility.show_cwd {
        let label = format!(
            "{text:<width$}",
            text = "CWD",
            width = metrics.max_cwd_width
        );
        spans.push(Span::from(label).bold());
        spans.push("  ".into());
    }
    spans.push("Conversation".bold());
    frame.render_widget(Line::from(spans), area);
}

pub(super) fn render_list(
    frame: &mut crate::custom_terminal::Frame,
    area: Rect,
    state: &PickerState,
    metrics: &ColumnMetrics,
) {
    if area.height == 0 {
        return;
    }

    let rows = &state.filtered_rows;
    if rows.is_empty() {
        let message = render_empty_state_line(state);
        frame.render_widget(message, area);
        return;
    }

    let capacity = area.height as usize;
    let start = state.scroll_top.min(rows.len().saturating_sub(1));
    let end = rows.len().min(start + capacity);
    let labels = &metrics.labels;
    let mut y = area.y;

    let visibility = column_visibility(area.width, metrics, state.sort_key);
    let max_created_width = metrics.max_created_width;
    let max_updated_width = metrics.max_updated_width;
    let max_branch_width = metrics.max_branch_width;
    let max_cwd_width = metrics.max_cwd_width;

    for (idx, (row, (created_label, updated_label, branch_label, cwd_label))) in rows[start..end]
        .iter()
        .zip(labels[start..end].iter())
        .enumerate()
    {
        let is_sel = start + idx == state.selected;
        let marker = if is_sel { "> ".bold() } else { "  ".into() };
        let marker_width = 2usize;
        let created_span = if visibility.show_created {
            Some(Span::from(format!("{created_label:<max_created_width$}")).dim())
        } else {
            None
        };
        let updated_span = if visibility.show_updated {
            Some(Span::from(format!("{updated_label:<max_updated_width$}")).dim())
        } else {
            None
        };
        let branch_span = if !visibility.show_branch {
            None
        } else if branch_label.is_empty() {
            Some(
                Span::from(format!(
                    "{empty:<width$}",
                    empty = "-",
                    width = max_branch_width
                ))
                .dim(),
            )
        } else {
            Some(Span::from(format!("{branch_label:<max_branch_width$}")).cyan())
        };
        let cwd_span = if !visibility.show_cwd {
            None
        } else if cwd_label.is_empty() {
            Some(
                Span::from(format!(
                    "{empty:<width$}",
                    empty = "-",
                    width = max_cwd_width
                ))
                .dim(),
            )
        } else {
            Some(Span::from(format!("{cwd_label:<max_cwd_width$}")).dim())
        };

        let mut preview_width = area.width as usize;
        preview_width = preview_width.saturating_sub(marker_width);
        if visibility.show_created {
            preview_width = preview_width.saturating_sub(max_created_width + 2);
        }
        if visibility.show_updated {
            preview_width = preview_width.saturating_sub(max_updated_width + 2);
        }
        if visibility.show_branch {
            preview_width = preview_width.saturating_sub(max_branch_width + 2);
        }
        if visibility.show_cwd {
            preview_width = preview_width.saturating_sub(max_cwd_width + 2);
        }
        let add_leading_gap = !visibility.show_created
            && !visibility.show_updated
            && !visibility.show_branch
            && !visibility.show_cwd;
        if add_leading_gap {
            preview_width = preview_width.saturating_sub(2);
        }
        let preview = truncate_text(row.display_preview(), preview_width);
        let mut spans: Vec<Span> = vec![marker];
        if let Some(created) = created_span {
            spans.push(created);
            spans.push("  ".into());
        }
        if let Some(updated) = updated_span {
            spans.push(updated);
            spans.push("  ".into());
        }
        if let Some(branch) = branch_span {
            spans.push(branch);
            spans.push("  ".into());
        }
        if let Some(cwd) = cwd_span {
            spans.push(cwd);
            spans.push("  ".into());
        }
        if add_leading_gap {
            spans.push("  ".into());
        }
        spans.push(preview.into());

        let line: Line = spans.into();
        let rect = Rect::new(area.x, y, area.width, 1);
        frame.render_widget(line, rect);
        y = y.saturating_add(1);
    }

    if state.pagination.loading.is_pending() && y < area.y.saturating_add(area.height) {
        let loading_line: Line = vec!["  ".into(), "Loading older sessions…".italic().dim()].into();
        let rect = Rect::new(area.x, y, area.width, 1);
        frame.render_widget(loading_line, rect);
    }
}

pub(super) fn render_empty_state_line(state: &PickerState) -> Line<'static> {
    if !state.query.is_empty() {
        if state.search_state.is_active()
            || (state.pagination.loading.is_pending() && state.pagination.next_cursor.is_some())
        {
            return vec!["Searching…".italic().dim()].into();
        }
        if state.pagination.reached_scan_limit {
            let msg = format!(
                "Search scanned first {} sessions; more may exist",
                state.pagination.num_scanned_records
            );
            return vec![Span::from(msg).italic().dim()].into();
        }
        return vec!["No results for your search".italic().dim()].into();
    }

    if state.all_rows.is_empty() && state.pagination.num_scanned_records == 0 {
        return vec!["No sessions yet".italic().dim()].into();
    }

    if state.pagination.loading.is_pending() {
        return vec!["Loading older sessions…".italic().dim()].into();
    }

    vec!["No sessions yet".italic().dim()].into()
}
