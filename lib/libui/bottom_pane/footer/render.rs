use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

use crate::key_hint;
use crate::render::line_utils::prefix_lines;
use crate::status::format_tokens_compact;
use crate::ui_consts::FOOTER_INDENT_COLS;

use super::shortcuts::shortcut_overlay_lines;
use super::types::CollaborationModeIndicator;
use super::types::FooterMode;
use super::types::FooterProps;
use super::types::LeftSideState;
use super::types::ShortcutsState;
use super::types::SummaryHintKind;
use super::types::SummaryLeft;

const FOOTER_CONTEXT_GAP_COLS: u16 = 1;

pub fn toggle_shortcut_mode(current: FooterMode, ctrl_c_hint: bool, is_empty: bool) -> FooterMode {
    if ctrl_c_hint && matches!(current, FooterMode::QuitShortcutReminder) {
        return current;
    }

    let base_mode = if is_empty {
        FooterMode::ComposerEmpty
    } else {
        FooterMode::ComposerHasDraft
    };

    match current {
        FooterMode::ShortcutOverlay | FooterMode::QuitShortcutReminder => base_mode,
        _ => FooterMode::ShortcutOverlay,
    }
}

pub fn esc_hint_mode(current: FooterMode, is_task_running: bool) -> FooterMode {
    if is_task_running {
        current
    } else {
        FooterMode::EscHint
    }
}

pub fn reset_mode_after_activity(current: FooterMode) -> FooterMode {
    match current {
        FooterMode::EscHint
        | FooterMode::ShortcutOverlay
        | FooterMode::QuitShortcutReminder
        | FooterMode::ComposerHasDraft => FooterMode::ComposerEmpty,
        other => other,
    }
}

pub fn footer_height(props: &FooterProps) -> u16 {
    let show_shortcuts_hint = match props.mode {
        FooterMode::ComposerEmpty => true,
        FooterMode::ComposerHasDraft => false,
        FooterMode::QuitShortcutReminder | FooterMode::ShortcutOverlay | FooterMode::EscHint => {
            false
        }
    };
    let show_queue_hint = match props.mode {
        FooterMode::ComposerHasDraft => props.is_task_running,
        FooterMode::QuitShortcutReminder
        | FooterMode::ComposerEmpty
        | FooterMode::ShortcutOverlay
        | FooterMode::EscHint => false,
    };
    footer_from_props_lines(
        props,
        /*collaboration_mode_indicator*/ None,
        /*show_cycle_hint*/ false,
        show_shortcuts_hint,
        show_queue_hint,
    )
    .len() as u16
}

/// Render a single precomputed footer line.
pub fn render_footer_line(area: Rect, buf: &mut Buffer, line: Line<'static>) {
    Paragraph::new(prefix_lines(
        vec![line],
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    ))
    .render(area, buf);
}

/// Render footer content directly from `FooterProps`.
///
/// This is intentionally not part of the width-based collapse/fallback logic.
/// Transient instructional states (shortcut overlay, Esc hint, quit reminder)
/// prioritize "what to do next" instructions and currently suppress the
/// collaboration mode label entirely. When collapse logic has already chosen a
/// specific single line, prefer `render_footer_line`.
pub fn render_footer_from_props(
    area: Rect,
    buf: &mut Buffer,
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) {
    Paragraph::new(prefix_lines(
        footer_from_props_lines(
            props,
            collaboration_mode_indicator,
            show_cycle_hint,
            show_shortcuts_hint,
            show_queue_hint,
        ),
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    ))
    .render(area, buf);
}

pub fn left_fits(area: Rect, left_width: u16) -> bool {
    let max_width = area.width.saturating_sub(FOOTER_INDENT_COLS as u16);
    left_width <= max_width
}

fn left_side_line(
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    state: LeftSideState,
) -> Line<'static> {
    let mut line = Line::from("");
    match state.hint {
        SummaryHintKind::None => {}
        SummaryHintKind::Shortcuts => {
            line.push_span(key_hint::plain(KeyCode::Char('?')));
            line.push_span(" for shortcuts".dim());
        }
        SummaryHintKind::QueueMessage => {
            line.push_span(key_hint::plain(KeyCode::Tab));
            line.push_span(" to queue message".dim());
        }
        SummaryHintKind::QueueShort => {
            line.push_span(key_hint::plain(KeyCode::Tab));
            line.push_span(" to queue".dim());
        }
    };

    if let Some(collaboration_mode_indicator) = collaboration_mode_indicator {
        if !matches!(state.hint, SummaryHintKind::None) {
            line.push_span(" · ".dim());
        }
        line.push_span(collaboration_mode_indicator.styled_span(state.show_cycle_hint));
    }

    line
}

/// Compute the single-line footer layout and whether the right-side context
/// indicator can be shown alongside it.
pub fn single_line_footer_layout(
    area: Rect,
    context_width: u16,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) -> (SummaryLeft, bool) {
    let hint_kind = if show_queue_hint {
        SummaryHintKind::QueueMessage
    } else if show_shortcuts_hint {
        SummaryHintKind::Shortcuts
    } else {
        SummaryHintKind::None
    };
    let default_state = LeftSideState {
        hint: hint_kind,
        show_cycle_hint,
    };
    let default_line = left_side_line(collaboration_mode_indicator, default_state);
    let default_width = default_line.width() as u16;
    if default_width > 0 && can_show_left_with_context(area, default_width, context_width) {
        return (SummaryLeft::Default, true);
    }

    let state_line = |state: LeftSideState| -> Line<'static> {
        if state == default_state {
            default_line.clone()
        } else {
            left_side_line(collaboration_mode_indicator, state)
        }
    };
    let state_width = |state: LeftSideState| -> u16 { state_line(state).width() as u16 };
    // When the mode cycle hint is applicable (idle, non-queue mode), only show
    // the right-side context indicator if the "(shift+tab to cycle)" variant
    // can also fit.
    let context_requires_cycle_hint = show_cycle_hint && !show_queue_hint;

    if show_queue_hint {
        // In queue mode, prefer dropping context before dropping the queue hint.
        let queue_states = [
            default_state,
            LeftSideState {
                hint: SummaryHintKind::QueueMessage,
                show_cycle_hint: false,
            },
            LeftSideState {
                hint: SummaryHintKind::QueueShort,
                show_cycle_hint: false,
            },
        ];

        // Pass 1: keep the right-side context indicator if any queue variant
        // can fit alongside it. We skip adjacent duplicates because
        // `default_state` can already be the no-cycle queue variant.
        let mut previous_state: Option<LeftSideState> = None;
        for state in queue_states {
            if previous_state == Some(state) {
                continue;
            }
            previous_state = Some(state);
            let width = state_width(state);
            if width > 0 && can_show_left_with_context(area, width, context_width) {
                if state == default_state {
                    return (SummaryLeft::Default, true);
                }
                return (SummaryLeft::Custom(state_line(state)), true);
            }
        }

        // Pass 2: if context cannot fit, drop it before dropping the queue
        // hint. Reuse the same dedupe so we do not try equivalent states twice.
        let mut previous_state: Option<LeftSideState> = None;
        for state in queue_states {
            if previous_state == Some(state) {
                continue;
            }
            previous_state = Some(state);
            let width = state_width(state);
            if width > 0 && left_fits(area, width) {
                if state == default_state {
                    return (SummaryLeft::Default, false);
                }
                return (SummaryLeft::Custom(state_line(state)), false);
            }
        }
    } else if collaboration_mode_indicator.is_some() {
        if show_cycle_hint {
            // First fallback: drop shortcut hint but keep the cycle
            // hint on the mode label if it can fit.
            let cycle_state = LeftSideState {
                hint: SummaryHintKind::None,
                show_cycle_hint: true,
            };
            let cycle_width = state_width(cycle_state);
            if cycle_width > 0 && can_show_left_with_context(area, cycle_width, context_width) {
                return (SummaryLeft::Custom(state_line(cycle_state)), true);
            }
            if cycle_width > 0 && left_fits(area, cycle_width) {
                return (SummaryLeft::Custom(state_line(cycle_state)), false);
            }
        }

        // Next fallback: mode label only. If the cycle hint is applicable but
        // cannot fit, we also suppress context so the right side does not
        // outlive "(shift+tab to cycle)" on the left.
        let mode_only_state = LeftSideState {
            hint: SummaryHintKind::None,
            show_cycle_hint: false,
        };
        let mode_only_width = state_width(mode_only_state);
        if !context_requires_cycle_hint
            && mode_only_width > 0
            && can_show_left_with_context(area, mode_only_width, context_width)
        {
            return (
                SummaryLeft::Custom(state_line(mode_only_state)),
                true, // show_context
            );
        }
        if mode_only_width > 0 && left_fits(area, mode_only_width) {
            return (
                SummaryLeft::Custom(state_line(mode_only_state)),
                false, // show_context
            );
        }
    }

    // Final fallback: if queue variants (or other earlier states) could not fit
    // at all, drop every hint and try to show just the mode label.
    if let Some(collaboration_mode_indicator) = collaboration_mode_indicator {
        let mode_only_state = LeftSideState {
            hint: SummaryHintKind::None,
            show_cycle_hint: false,
        };
        // Compute the width without going through `state_line` so we do not
        // depend on `default_state` (which may still be a queue variant).
        let mode_only_width =
            left_side_line(Some(collaboration_mode_indicator), mode_only_state).width() as u16;
        if !context_requires_cycle_hint
            && can_show_left_with_context(area, mode_only_width, context_width)
        {
            return (
                SummaryLeft::Custom(left_side_line(
                    Some(collaboration_mode_indicator),
                    mode_only_state,
                )),
                true, // show_context
            );
        }
        if left_fits(area, mode_only_width) {
            return (
                SummaryLeft::Custom(left_side_line(
                    Some(collaboration_mode_indicator),
                    mode_only_state,
                )),
                false, // show_context
            );
        }
    }

    (SummaryLeft::None, true)
}

fn right_aligned_x(area: Rect, content_width: u16) -> Option<u16> {
    if area.is_empty() {
        return None;
    }

    let right_padding = FOOTER_INDENT_COLS as u16;
    let max_width = area.width.saturating_sub(right_padding);
    if content_width == 0 || max_width == 0 {
        return None;
    }

    if content_width >= max_width {
        return Some(area.x.saturating_add(right_padding));
    }

    Some(
        area.x
            .saturating_add(area.width)
            .saturating_sub(content_width)
            .saturating_sub(right_padding),
    )
}

pub fn max_left_width_for_right(area: Rect, right_width: u16) -> Option<u16> {
    let context_x = right_aligned_x(area, right_width)?;
    let left_start = area.x + FOOTER_INDENT_COLS as u16;

    // minimal one column gap between left and right
    let gap = FOOTER_CONTEXT_GAP_COLS;

    if context_x <= left_start + gap {
        return Some(0);
    }

    Some(context_x.saturating_sub(left_start + gap))
}

pub fn can_show_left_with_context(area: Rect, left_width: u16, context_width: u16) -> bool {
    let Some(context_x) = right_aligned_x(area, context_width) else {
        return true;
    };
    if left_width == 0 {
        return true;
    }
    let left_extent = FOOTER_INDENT_COLS as u16 + left_width + FOOTER_CONTEXT_GAP_COLS;
    left_extent <= context_x.saturating_sub(area.x)
}

pub fn render_context_right(area: Rect, buf: &mut Buffer, line: &Line<'static>) {
    if area.is_empty() {
        return;
    }

    let context_width = line.width() as u16;
    let Some(mut x) = right_aligned_x(area, context_width) else {
        return;
    };
    let y = area.y + area.height.saturating_sub(1);
    let max_x = area.x.saturating_add(area.width);

    for span in &line.spans {
        if x >= max_x {
            break;
        }
        let span_width = span.width() as u16;
        if span_width == 0 {
            continue;
        }
        let remaining = max_x.saturating_sub(x);
        let draw_width = span_width.min(remaining);
        buf.set_span(x, y, span, draw_width);
        x = x.saturating_add(span_width);
    }
}

pub fn inset_footer_hint_area(mut area: Rect) -> Rect {
    if area.width > 2 {
        area.x += 2;
        area.width = area.width.saturating_sub(2);
    }
    area
}

pub fn render_footer_hint_items(area: Rect, buf: &mut Buffer, items: &[(String, String)]) {
    if items.is_empty() {
        return;
    }

    footer_hint_items_line(items).render(inset_footer_hint_area(area), buf);
}

/// Map `FooterProps` to footer lines without width-based collapse.
///
/// This is the canonical FooterMode-to-text mapping. It powers transient,
/// instructional states (shortcut overlay, Esc hint, quit reminder) and also
/// the default rendering for base states when collapse is not applied (or when
/// `single_line_footer_layout` returns `SummaryLeft::Default`). Collapse and
/// fallback decisions live in `single_line_footer_layout`; this function only
/// formats the chosen/default content.
fn footer_from_props_lines(
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) -> Vec<Line<'static>> {
    // Passive footer context can come from the configurable status line, the
    // active agent label, or both combined.
    if let Some(status_line) = passive_footer_status_line(props) {
        return vec![status_line.dim()];
    }
    match props.mode {
        FooterMode::QuitShortcutReminder => {
            vec![quit_shortcut_reminder_line(props.quit_shortcut_key)]
        }
        FooterMode::ComposerEmpty => {
            let state = LeftSideState {
                hint: if show_shortcuts_hint {
                    SummaryHintKind::Shortcuts
                } else {
                    SummaryHintKind::None
                },
                show_cycle_hint,
            };
            vec![left_side_line(collaboration_mode_indicator, state)]
        }
        FooterMode::ShortcutOverlay => {
            let state = ShortcutsState {
                use_shift_enter_hint: props.use_shift_enter_hint,
                esc_backtrack_hint: props.esc_backtrack_hint,
                collaboration_modes_enabled: props.collaboration_modes_enabled,
            };
            shortcut_overlay_lines(state)
        }
        FooterMode::EscHint => vec![esc_hint_line(props.esc_backtrack_hint)],
        FooterMode::ComposerHasDraft => {
            let state = LeftSideState {
                hint: if show_queue_hint {
                    SummaryHintKind::QueueMessage
                } else if show_shortcuts_hint {
                    SummaryHintKind::Shortcuts
                } else {
                    SummaryHintKind::None
                },
                show_cycle_hint,
            };
            vec![left_side_line(collaboration_mode_indicator, state)]
        }
    }
}

/// Returns the contextual footer row when the footer is not busy showing an instructional hint.
///
/// The returned line may contain the configured status line, the currently viewed agent label, or
/// both combined. Active instructional states such as quit reminders, shortcut overlays, and queue
/// prompts deliberately return `None` so those call-to-action hints stay visible.
pub fn passive_footer_status_line(props: &FooterProps) -> Option<Line<'static>> {
    if !shows_passive_footer_line(props) {
        return None;
    }

    let mut line = if props.status_line_enabled {
        props.status_line_value.clone()
    } else {
        None
    };

    if let Some(active_agent_label) = props.active_agent_label.as_ref() {
        if let Some(existing) = line.as_mut() {
            existing.spans.push(" · ".into());
            existing.spans.push(active_agent_label.clone().into());
        } else {
            line = Some(Line::from(active_agent_label.clone()));
        }
    }

    line
}

/// Whether the current footer mode allows contextual information to replace instructional hints.
///
/// In practice this means the composer is idle, or it has a draft but is not currently running a
/// task, so the footer can spend the row on ambient context instead of "what to do next" text.
pub fn shows_passive_footer_line(props: &FooterProps) -> bool {
    match props.mode {
        FooterMode::ComposerEmpty => true,
        FooterMode::ComposerHasDraft => !props.is_task_running,
        FooterMode::QuitShortcutReminder | FooterMode::ShortcutOverlay | FooterMode::EscHint => {
            false
        }
    }
}

/// Whether callers should reserve the dedicated status-line layout for a contextual footer row.
///
/// The dedicated layout exists for the configurable `/statusline` row. An agent label by itself
/// can be rendered by the standard footer flow, so this only becomes `true` when the status line
/// feature is enabled and the current mode allows contextual footer content.
pub fn uses_passive_footer_status_layout(props: &FooterProps) -> bool {
    props.status_line_enabled && shows_passive_footer_line(props)
}

pub fn footer_line_width(
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) -> u16 {
    footer_from_props_lines(
        props,
        collaboration_mode_indicator,
        show_cycle_hint,
        show_shortcuts_hint,
        show_queue_hint,
    )
    .last()
    .map(|line| line.width() as u16)
    .unwrap_or(0)
}

pub fn footer_hint_items_width(items: &[(String, String)]) -> u16 {
    if items.is_empty() {
        return 0;
    }
    footer_hint_items_line(items).width() as u16
}

fn footer_hint_items_line(items: &[(String, String)]) -> Line<'static> {
    let mut spans = Vec::with_capacity(items.len() * 4);
    for (idx, (key, label)) in items.iter().enumerate() {
        spans.push(" ".into());
        spans.push(key.clone().bold());
        spans.push(format!(" {label}").into());
        if idx + 1 != items.len() {
            spans.push("   ".into());
        }
    }
    Line::from(spans)
}

fn quit_shortcut_reminder_line(key: crate::key_hint::KeyBinding) -> Line<'static> {
    Line::from(vec![key.into(), " again to quit".into()]).dim()
}

fn esc_hint_line(esc_backtrack_hint: bool) -> Line<'static> {
    let esc = key_hint::plain(KeyCode::Esc);
    if esc_backtrack_hint {
        Line::from(vec![esc.into(), " again to edit previous message".into()]).dim()
    } else {
        Line::from(vec![
            esc.into(),
            " ".into(),
            esc.into(),
            " to edit previous message".into(),
        ])
        .dim()
    }
}

pub fn context_window_line(percent: Option<i64>, used_tokens: Option<i64>) -> Line<'static> {
    if let Some(percent) = percent {
        let percent = percent.clamp(0, 100);
        return Line::from(vec![Span::from(format!("{percent}% context left")).dim()]);
    }

    if let Some(tokens) = used_tokens {
        let used_fmt = format_tokens_compact(tokens);
        return Line::from(vec![Span::from(format!("{used_fmt} used")).dim()]);
    }

    Line::from(vec![Span::from("100% context left").dim()])
}
