use super::*;
use crate::key_hint;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
#[cfg(feature = "vt100-tests")]
use crate::test_backend::VT100Backend;
use crate::test_support::render_test_backend_debug;
use crate::ui_consts::FOOTER_INDENT_COLS;
use crossterm::event::KeyCode;
macro_rules! assert_snapshot {
    ($($arg:tt)*) => {
        insta::with_settings!({ snapshot_path => "../snapshots" }, {
            insta::assert_snapshot!($($arg)*);
        })
    };
}
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use shortcuts::SHORTCUTS;
use shortcuts::ShortcutId;
use types::ShortcutsState;

fn badge(kind: chaos_ipc::config_types::ModeKind) -> CollaborationModeIndicator {
    CollaborationModeIndicator::new(kind, "gpt-5.4".to_string(), Some("high".to_string()))
}

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
    let status_line_active = uses_passive_footer_status_layout(props);
    let passive_status_line = if status_line_active {
        passive_footer_status_line(props)
    } else {
        None
    };
    let left_mode_indicator = if status_line_active {
        None
    } else {
        collaboration_mode_indicator.clone()
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
            left_mode_indicator.clone(),
            show_cycle_hint,
            show_shortcuts_hint,
            show_queue_hint,
        )
    };
    let right_line = if status_line_active {
        best_right_mode_indicator_line(
            area,
            left_width,
            collaboration_mode_indicator,
            show_cycle_hint,
        )
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
                left_mode_indicator.clone(),
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
    let mut terminal = ratatui::Terminal::new(VT100Backend::new(width, height)).expect("terminal");
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

pub(crate) fn footer_suite() {
    footer_snapshots();
    #[cfg(feature = "vt100-tests")]
    footer_status_line_truncates_to_keep_mode_indicator();
    paste_image_shortcut_is_ctrl_v();
    shortcut_help_includes_navigation_and_panes();
}

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
        Some(badge(chaos_ipc::config_types::ModeKind::Plan)),
    );

    snapshot_footer_with_mode_indicator(
        "footer_mode_indicator_narrow_overlap_hides",
        50,
        &props,
        Some(badge(chaos_ipc::config_types::ModeKind::Plan)),
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
        Some(badge(chaos_ipc::config_types::ModeKind::Plan)),
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
        Some(badge(chaos_ipc::config_types::ModeKind::Plan)),
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
        Some(badge(chaos_ipc::config_types::ModeKind::Plan)),
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
        Some(badge(chaos_ipc::config_types::ModeKind::Plan)),
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

    let screen = render_footer_with_mode_indicator(
        80,
        &props,
        Some(badge(chaos_ipc::config_types::ModeKind::Plan)),
    );
    let collapsed = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        collapsed.contains("Plan"),
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

fn shortcut_help_includes_navigation_and_panes() {
    for collaboration_modes_enabled in [false, true] {
        let lines = shortcuts::shortcut_overlay_lines(ShortcutsState {
            use_shift_enter_hint: false,
            esc_backtrack_hint: false,
            collaboration_modes_enabled,
        });
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "ctrl + o to view logs",
            "ctrl + l to clear screen (idle)",
            "ctrl + p to cycle permissions",
            "Agents (empty draft; no popup)",
            "previous agent",
            "next agent",
            "Panes (multiple panes open; no popup)",
            "/j/k/l focus",
            "/j/k/l resize panes",
            "cycle focus",
            "close one pane",
            "close all panes",
            "man/chaos-keyboard.7.md",
        ] {
            assert!(text.contains(expected), "missing shortcut help: {expected}");
        }
        assert_eq!(
            text.contains("shift + tab to change mode"),
            collaboration_modes_enabled
        );
        // The normal 80-column footer reserves two cells on each side.
        assert!(lines.iter().all(|line| line.width() <= 76));
        // KeyBinding uses the macOS glyph in tests; also check the wider Unix label.
        assert!(lines.iter().all(|line| {
            ratatui::text::Line::from(line.to_string().replace('⌥', "alt")).width() <= 76
        }));
    }
}
