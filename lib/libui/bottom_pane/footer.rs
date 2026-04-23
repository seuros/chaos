//! The bottom-pane footer renders transient hints and context indicators.
//!
//! The footer is pure rendering: it formats `FooterProps` into `Line`s without mutating any state.
//! It intentionally does not decide *which* footer content should be shown; that is owned by the
//! `ChatComposer` (which selects a `FooterMode`) and by higher-level state machines like
//! `ChatWidget` (which decides when quit/interrupt is allowed).
//!
//! Some footer content is time-based rather than event-based, such as the "press again to quit"
//! hint. The owning widgets schedule redraws so time-based hints can expire even if the UI is
//! otherwise idle.
//!
//! Terminology used in this module:
//! - "status line" means the configurable contextual row built from `/statusline` items such as
//!   model, git branch, and context usage.
//! - "instructional footer" means a row that tells the user what to do next, such as quit
//!   confirmation, shortcut help, or queue hints.
//! - "contextual footer" means the footer is free to show ambient context instead of an
//!   instruction. In that state, the footer may render the configured status line, the active
//!   agent label, or both combined.
//!
//! Single-line collapse overview:
//! 1. The composer decides the current `FooterMode` and hint flags, then calls
//!    `single_line_footer_layout` for the base single-line modes.
//! 2. `single_line_footer_layout` applies the width-based fallback rules:
//!    (If this description is hard to follow, just try it out by resizing
//!    your terminal width; these rules were built out of trial and error.)
//!    - Start with the fullest left-side hint plus the right-side context.
//!    - When the queue hint is active, prefer keeping that queue hint visible,
//!      even if it means dropping the right-side context earlier; the queue
//!      hint may also be shortened before it is removed.
//!    - When the queue hint is not active but the mode cycle hint is applicable,
//!      drop "? for shortcuts" before dropping "(shift+tab to cycle)".
//!    - If "(shift+tab to cycle)" cannot fit, also hide the right-side
//!      context to avoid too many state transitions in quick succession.
//!    - Finally, try a mode-only line (with and without context), and fall
//!      back to no left-side footer if nothing can fit.
//! 3. When collapse chooses a specific line, callers render it via
//!    `render_footer_line`. Otherwise, callers render the straightforward
//!    mode-to-text mapping via `render_footer_from_props`.
//!
//! In short: `single_line_footer_layout` chooses *what* best fits, and the two
//! render helpers choose whether to draw the chosen line or the default
//! `FooterProps` mapping.

mod collaboration_mode;
mod render;
mod shortcuts;
mod types;

pub use collaboration_mode::mode_indicator_line;
pub use render::can_show_left_with_context;
pub use render::context_window_line;
pub use render::esc_hint_mode;
pub use render::footer_height;
pub use render::footer_hint_items_width;
pub use render::footer_line_width;
pub use render::inset_footer_hint_area;
pub use render::max_left_width_for_right;
pub use render::passive_footer_status_line;
pub use render::render_context_right;
pub use render::render_footer_from_props;
pub use render::render_footer_hint_items;
pub use render::render_footer_line;
pub use render::reset_mode_after_activity;
pub use render::single_line_footer_layout;
pub use render::toggle_shortcut_mode;
pub use render::uses_passive_footer_status_layout;
pub use types::CollaborationModeIndicator;
pub use types::FooterMode;
pub use types::FooterProps;
pub use types::SummaryLeft;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_hint;
    use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
    #[cfg(feature = "vt100-tests")]
    use crate::test_backend::VT100Backend;
    use crate::test_support::render_test_backend_debug;
    use crate::ui_consts::FOOTER_INDENT_COLS;
    use crossterm::event::KeyCode;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Stylize;
    use ratatui::text::Line;
    use shortcuts::SHORTCUTS;
    use shortcuts::ShortcutId;
    use types::ShortcutsState;

    fn snapshot_footer(name: &str, props: FooterProps) {
        snapshot_footer_with_mode_indicator(name, 80, &props, None);
    }

    fn draw_footer(
        area: Rect,
        buf: &mut Buffer,
        props: &FooterProps,
        collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    ) {
        let show_cycle_hint = !props.is_task_running;
        let show_shortcuts_hint = match props.mode {
            FooterMode::ComposerEmpty => true,
            FooterMode::ComposerHasDraft => false,
            FooterMode::QuitShortcutReminder
            | FooterMode::ShortcutOverlay
            | FooterMode::EscHint => false,
        };
        let show_queue_hint = match props.mode {
            FooterMode::ComposerHasDraft => props.is_task_running,
            FooterMode::QuitShortcutReminder
            | FooterMode::ComposerEmpty
            | FooterMode::ShortcutOverlay
            | FooterMode::EscHint => false,
        };
        let status_line_active = uses_passive_footer_status_layout(props);
        let passive_status_line = if status_line_active {
            passive_footer_status_line(props)
        } else {
            None
        };
        let left_mode_indicator = if status_line_active {
            None
        } else {
            collaboration_mode_indicator
        };
        let available_width = area.width.saturating_sub(FOOTER_INDENT_COLS as u16) as usize;
        let mut truncated_status_line = if status_line_active
            && matches!(
                props.mode,
                FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
            ) {
            passive_status_line
                .as_ref()
                .map(|line| line.clone().dim())
                .map(|line| truncate_line_with_ellipsis_if_overflow(line, available_width))
        } else {
            None
        };
        let mut left_width = if status_line_active {
            truncated_status_line
                .as_ref()
                .map(|line| line.width() as u16)
                .unwrap_or(0)
        } else {
            footer_line_width(
                props,
                left_mode_indicator,
                show_cycle_hint,
                show_shortcuts_hint,
                show_queue_hint,
            )
        };
        let right_line = if status_line_active {
            let full = mode_indicator_line(collaboration_mode_indicator, show_cycle_hint);
            let compact = mode_indicator_line(collaboration_mode_indicator, false);
            let full_width = full.as_ref().map(|line| line.width() as u16).unwrap_or(0);
            if can_show_left_with_context(area, left_width, full_width) {
                full
            } else {
                compact
            }
        } else {
            Some(context_window_line(
                props.context_window_percent,
                props.context_window_used_tokens,
            ))
        };
        let right_width = right_line
            .as_ref()
            .map(|line| line.width() as u16)
            .unwrap_or(0);
        if status_line_active
            && let Some(max_left) = max_left_width_for_right(area, right_width)
            && left_width > max_left
            && let Some(line) = passive_status_line
                .as_ref()
                .map(|line| line.clone().dim())
                .map(|line| truncate_line_with_ellipsis_if_overflow(line, max_left as usize))
        {
            left_width = line.width() as u16;
            truncated_status_line = Some(line);
        }
        let can_show_left_and_context = can_show_left_with_context(area, left_width, right_width);
        if matches!(
            props.mode,
            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
        ) {
            if status_line_active {
                if let Some(line) = truncated_status_line.clone() {
                    render_footer_line(area, buf, line);
                }
                if can_show_left_and_context && let Some(line) = &right_line {
                    render_context_right(area, buf, line);
                }
            } else {
                let (summary_left, show_context) = single_line_footer_layout(
                    area,
                    right_width,
                    left_mode_indicator,
                    show_cycle_hint,
                    show_shortcuts_hint,
                    show_queue_hint,
                );
                match summary_left {
                    SummaryLeft::Default => {
                        render_footer_from_props(
                            area,
                            buf,
                            props,
                            left_mode_indicator,
                            show_cycle_hint,
                            show_shortcuts_hint,
                            show_queue_hint,
                        );
                    }
                    SummaryLeft::Custom(line) => {
                        render_footer_line(area, buf, line);
                    }
                    SummaryLeft::None => {}
                }
                if show_context && let Some(line) = &right_line {
                    render_context_right(area, buf, line);
                }
            }
        } else {
            render_footer_from_props(
                area,
                buf,
                props,
                left_mode_indicator,
                show_cycle_hint,
                show_shortcuts_hint,
                show_queue_hint,
            );
            let show_context = can_show_left_and_context
                && !matches!(
                    props.mode,
                    FooterMode::EscHint
                        | FooterMode::QuitShortcutReminder
                        | FooterMode::ShortcutOverlay
                );
            if show_context && let Some(line) = &right_line {
                render_context_right(area, buf, line);
            }
        }
    }

    fn snapshot_footer_with_mode_indicator(
        name: &str,
        width: u16,
        props: &FooterProps,
        collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    ) {
        let height = footer_height(props).max(1);
        assert_snapshot!(
            name,
            render_test_backend_debug(width, height, |f| {
                draw_footer(
                    Rect::new(0, 0, f.area().width, height),
                    f.buffer_mut(),
                    props,
                    collaboration_mode_indicator,
                );
            })
        );
    }

    #[cfg(feature = "vt100-tests")]
    fn render_footer_with_mode_indicator(
        width: u16,
        props: &FooterProps,
        collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    ) -> String {
        let height = footer_height(props).max(1);
        let mut terminal =
            ratatui::Terminal::new(VT100Backend::new(width, height)).expect("terminal");
        terminal
            .draw(|f| {
                draw_footer(
                    Rect::new(0, 0, f.area().width, height),
                    f.buffer_mut(),
                    props,
                    collaboration_mode_indicator,
                );
            })
            .expect("draw footer");
        terminal.backend().vt100().screen().contents()
    }

    #[test]
    fn footer_snapshots() {
        snapshot_footer(
            "footer_shortcuts_default",
            FooterProps {
                mode: FooterMode::ComposerEmpty,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                collaboration_modes_enabled: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                status_line_value: None,
                status_line_enabled: false,
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_shortcuts_shift_and_esc",
            FooterProps {
                mode: FooterMode::ShortcutOverlay,
                esc_backtrack_hint: true,
                use_shift_enter_hint: true,
                is_task_running: false,
                collaboration_modes_enabled: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                status_line_value: None,
                status_line_enabled: false,
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_shortcuts_collaboration_modes_enabled",
            FooterProps {
                mode: FooterMode::ShortcutOverlay,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                collaboration_modes_enabled: true,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                status_line_value: None,
                status_line_enabled: false,
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_ctrl_c_quit_idle",
            FooterProps {
                mode: FooterMode::QuitShortcutReminder,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                collaboration_modes_enabled: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                status_line_value: None,
                status_line_enabled: false,
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_ctrl_c_quit_running",
            FooterProps {
                mode: FooterMode::QuitShortcutReminder,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: true,
                collaboration_modes_enabled: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                status_line_value: None,
                status_line_enabled: false,
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_esc_hint_idle",
            FooterProps {
                mode: FooterMode::EscHint,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                collaboration_modes_enabled: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                status_line_value: None,
                status_line_enabled: false,
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_esc_hint_primed",
            FooterProps {
                mode: FooterMode::EscHint,
                esc_backtrack_hint: true,
                use_shift_enter_hint: false,
                is_task_running: false,
                collaboration_modes_enabled: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                status_line_value: None,
                status_line_enabled: false,
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_shortcuts_context_running",
            FooterProps {
                mode: FooterMode::ComposerEmpty,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: true,
                collaboration_modes_enabled: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: Some(72),
                context_window_used_tokens: None,
                status_line_value: None,
                status_line_enabled: false,
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_context_tokens_used",
            FooterProps {
                mode: FooterMode::ComposerEmpty,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                collaboration_modes_enabled: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: Some(123_456),
                status_line_value: None,
                status_line_enabled: false,
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_composer_has_draft_queue_hint_enabled",
            FooterProps {
                mode: FooterMode::ComposerHasDraft,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: true,
                collaboration_modes_enabled: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                status_line_value: None,
                status_line_enabled: false,
                active_agent_label: None,
            },
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            collaboration_modes_enabled: true,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: None,
            context_window_used_tokens: None,
            status_line_value: None,
            status_line_enabled: false,
            active_agent_label: None,
        };

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_wide",
            120,
            &props,
            Some(CollaborationModeIndicator::Plan),
        );

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_narrow_overlap_hides",
            50,
            &props,
            Some(CollaborationModeIndicator::Plan),
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: true,
            collaboration_modes_enabled: true,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: None,
            context_window_used_tokens: None,
            status_line_value: None,
            status_line_enabled: false,
            active_agent_label: None,
        };

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_running_hides_hint",
            120,
            &props,
            Some(CollaborationModeIndicator::Plan),
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            collaboration_modes_enabled: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: None,
            context_window_used_tokens: None,
            status_line_value: Some(Line::from("Status line content".to_string())),
            status_line_enabled: true,
            active_agent_label: None,
        };

        snapshot_footer("footer_status_line_overrides_shortcuts", props);

        let props = FooterProps {
            mode: FooterMode::ComposerHasDraft,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: true,
            collaboration_modes_enabled: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: None,
            context_window_used_tokens: None,
            status_line_value: Some(Line::from("Status line content".to_string())),
            status_line_enabled: true,
            active_agent_label: None,
        };

        snapshot_footer("footer_status_line_yields_to_queue_hint", props);

        let props = FooterProps {
            mode: FooterMode::ComposerHasDraft,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            collaboration_modes_enabled: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: None,
            context_window_used_tokens: None,
            status_line_value: Some(Line::from("Status line content".to_string())),
            status_line_enabled: true,
            active_agent_label: None,
        };

        snapshot_footer("footer_status_line_overrides_draft_idle", props);

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            collaboration_modes_enabled: true,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: Some(50),
            context_window_used_tokens: None,
            status_line_value: None, // command timed out / empty
            status_line_enabled: true,
            active_agent_label: None,
        };

        snapshot_footer_with_mode_indicator(
            "footer_status_line_enabled_mode_right",
            120,
            &props,
            Some(CollaborationModeIndicator::Plan),
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            collaboration_modes_enabled: true,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: Some(50),
            context_window_used_tokens: None,
            status_line_value: None,
            status_line_enabled: false,
            active_agent_label: None,
        };

        snapshot_footer_with_mode_indicator(
            "footer_status_line_disabled_context_right",
            120,
            &props,
            Some(CollaborationModeIndicator::Plan),
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            collaboration_modes_enabled: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: Some(50),
            context_window_used_tokens: None,
            status_line_value: None,
            status_line_enabled: true,
            active_agent_label: None,
        };

        // has status line and no collaboration mode
        snapshot_footer_with_mode_indicator(
            "footer_status_line_enabled_no_mode_right",
            120,
            &props,
            None,
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            collaboration_modes_enabled: true,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: Some(50),
            context_window_used_tokens: None,
            status_line_value: Some(Line::from(
                "Status line content that should truncate before the mode indicator".to_string(),
            )),
            status_line_enabled: true,
            active_agent_label: None,
        };

        snapshot_footer_with_mode_indicator(
            "footer_status_line_truncated_with_gap",
            40,
            &props,
            Some(CollaborationModeIndicator::Plan),
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            collaboration_modes_enabled: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: None,
            context_window_used_tokens: None,
            status_line_value: None,
            status_line_enabled: false,
            active_agent_label: Some("Robie [scout]".to_string()),
        };

        snapshot_footer("footer_active_agent_label", props);

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            collaboration_modes_enabled: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: None,
            context_window_used_tokens: None,
            status_line_value: Some(Line::from("Status line content".to_string())),
            status_line_enabled: true,
            active_agent_label: Some("Robie [scout]".to_string()),
        };

        snapshot_footer("footer_status_line_with_active_agent_label", props);
    }

    #[cfg(feature = "vt100-tests")]
    #[test]
    fn footer_status_line_truncates_to_keep_mode_indicator() {
        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            collaboration_modes_enabled: true,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: Some(50),
            context_window_used_tokens: None,
            status_line_value: Some(Line::from(
                "Status line content that is definitely too long to fit alongside the mode label"
                    .to_string(),
            )),
            status_line_enabled: true,
            active_agent_label: None,
        };

        let screen =
            render_footer_with_mode_indicator(80, &props, Some(CollaborationModeIndicator::Plan));
        let collapsed = screen.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            collapsed.contains("Plan mode"),
            "mode indicator should remain visible"
        );
        assert!(
            !collapsed.contains("shift+tab to cycle"),
            "compact mode indicator should be used when space is tight"
        );
        assert!(
            screen.contains('…'),
            "status line should be truncated with ellipsis to keep mode indicator"
        );
    }

    #[test]
    fn paste_image_shortcut_is_ctrl_v() {
        let descriptor = SHORTCUTS
            .iter()
            .find(|descriptor| descriptor.id == ShortcutId::PasteImage)
            .expect("paste image shortcut");

        let actual_key = descriptor
            .binding_for(ShortcutsState {
                use_shift_enter_hint: false,
                esc_backtrack_hint: false,
                collaboration_modes_enabled: false,
            })
            .expect("shortcut binding")
            .key;

        assert_eq!(actual_key, key_hint::ctrl(KeyCode::Char('v')));
    }
}
