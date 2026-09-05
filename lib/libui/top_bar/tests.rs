use super::*;
use crate::top_bar::widgets::architecture;
use crate::top_bar::widgets::battery;
use crate::top_bar::widgets::container;
use crate::top_bar::widgets::hostname;
use crate::top_bar::widgets::multiplexer;
use crate::top_bar::widgets::os;
use crate::top_bar::widgets::sandbox;
use crate::top_bar::widgets::storage;
use chaos_kern::{PersistenceHealth, PersistenceStatus, RuntimeStorageBackend};
use chaos_sysinfo::MultiplexerInfo;
use chaos_sysinfo::PowerInfo;
use chaos_sysinfo::SandboxKind;
use tokio::sync::watch;

fn clock(time: &str) -> BarWidget {
    let mut clock = widgets::clock::new();
    clock.refresh(&time.parse().expect("test time"));
    clock
}

fn render(widgets: &[BarWidget], width: u16) -> Buffer {
    let area = Rect::new(0, 0, width, 1);
    let mut buffer = Buffer::empty(area);
    Bar {
        widgets,
        palette: crate::theme::palette(),
    }
    .render_ref(area, &mut buffer);
    buffer
}

fn text(buffer: &Buffer) -> String {
    buffer
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

pub(crate) fn top_bar_suite() {
    shared_content_measures_unicode_and_uses_container_palette();
    watched_state_selects_coalesces_and_retains_snapshots();
    clock_fills_row_and_hides_whole_below_minimum_width();
    clock_updates_on_wall_clock_minute_boundaries();
    priority_admission_preserves_visual_order_and_spacing();
    oversized_widgets_do_not_block_smaller_ones();
    unicode_widgets_have_bounded_non_overlapping_rectangles();
    background_replaces_stale_cells_and_tracks_mode();
    three_widgets_resize_by_priority_and_reappear();
    battery_snapshots_update_hidden_widgets_and_preserve_unknown_state();
    static_widgets_preserve_labels_styles_and_optional_visibility();
    full_bar_preserves_visual_order_and_priorities();
    persistence_changes_preempt_and_restore_hidden_widgets();
    storage_backend_changes_remeasure_whole_labels();
    #[cfg(feature = "vt100-tests")]
    {
        pinned_widget_row_preserves_cursor_viewport_and_history();
        pinned_full_bar_clears_backend_and_warning_transitions();
    }
}

fn shared_content_measures_unicode_and_uses_container_palette() {
    use ratatui::style::{Color, Modifier};

    let palette = crate::theme::Palette {
        top_bar_bg: Color::Magenta,
        top_bar_fg: Color::Cyan,
        accent: Color::LightMagenta,
        success: Color::LightCyan,
        warning: Color::LightGreen,
        error: Color::Green,
        ..crate::theme::palette()
    };
    let label = "机器-cafe\u{301}";
    for (tone, color) in [
        (Tone::Normal, palette.top_bar_fg),
        (Tone::Accent, palette.accent),
        (Tone::Success, palette.success),
        (Tone::Warning, palette.warning),
        (Tone::Error, palette.error),
    ] {
        let widgets = [BarWidget::text(
            "unicode",
            Side::Left,
            180,
            Content::new(label).tone(tone).bold(),
        )];
        assert_eq!(widgets[0].spec().min_width, 9);
        let area = Rect::new(3, 2, 11, 1);
        let mut buffer = Buffer::empty(area);
        Bar {
            widgets: &widgets,
            palette,
        }
        .render_ref(area, &mut buffer);
        let mut expected = Buffer::empty(area);
        Bar {
            widgets: &[],
            palette,
        }
        .render_ref(area, &mut expected);
        Line::from(label)
            .style(Style::default().fg(color).add_modifier(Modifier::BOLD))
            .render(Rect::new(4, 2, 9, 1), &mut expected);
        assert_eq!(buffer, expected, "{tone:?}");
        assert_eq!(text(&render(&widgets, 10)), " ".repeat(10));
    }
}

fn watched_state_selects_coalesces_and_retains_snapshots() {
    let (tx, rx) = watch::channel((5_u8, 0_u8));
    let mut widgets = [BarWidget::watched(
        "sample",
        Side::Left,
        128,
        rx,
        |snapshot| snapshot.0,
        |value| Content::new(value.to_string()),
    )];
    let now = "2026-09-05T12:34:00Z[UTC]".parse().unwrap();
    assert_eq!(text(&render(&widgets, 3)), " 5 ");
    assert!(!widgets[0].refresh(&now).changed);
    tx.send_replace((5, 1));
    assert!(
        !widgets[0].refresh(&now).changed,
        "unselected fields are ignored"
    );

    tx.send_replace((6, 1));
    tx.send_replace((123, 2));
    let update = widgets[0].refresh(&now);
    assert!(update.changed);
    assert_eq!(update.next, None);
    assert_eq!(widgets[0].spec().min_width, 3);
    assert_eq!(text(&render(&widgets, 5)), " 123 ");

    tx.send_replace((42, 3));
    drop(tx);
    assert!(
        widgets[0].refresh(&now).changed,
        "the final snapshot survives closure"
    );
    assert!(!widgets[0].refresh(&now).changed);
    assert_eq!(text(&render(&widgets, 4)), " 42 ");
}

fn full_bar(status: watch::Receiver<PersistenceStatus>) -> Vec<BarWidget> {
    let mut info = chaos_sysinfo::sysinfo().clone();
    info.os = "linux".into();
    info.os_distro = "arch".into();
    info.arch = "x86_64".into();
    info.sandbox_type = SandboxKind::Seccomp;
    info.in_container = true;
    info.container_type = "podman".into();
    info.multiplexer = Some(MultiplexerInfo {
        kind: "tmux".into(),
        id: "%3".into(),
    });
    let (_, power) = watch::channel(PowerInfo {
        has_battery: true,
        battery_level: Some(87),
        charger_connected: false,
    });
    // Match runtime registration, including the asynchronously appended group.
    let mut widgets = widgets::initial_widgets("host".into(), power, status);
    widgets.extend(widgets::environment_widgets(&info));
    refresh_all(&mut widgets);
    widgets
}

fn refresh_all(widgets: &mut [BarWidget]) {
    let now = "2026-09-05T12:34:00Z[UTC]".parse().unwrap();
    for widget in widgets {
        widget.refresh(&now);
    }
}

fn static_widgets_preserve_labels_styles_and_optional_visibility() {
    let palette = crate::theme::palette();
    let cases: Vec<(BarWidget, &str, u8, ratatui::style::Color)> = vec![
        (
            os::new("linux", "arch"),
            "linux (arch)",
            100,
            palette.top_bar_fg,
        ),
        (os::new("macos", ""), "macos", 100, palette.top_bar_fg),
        (
            architecture::new("aarch64".into()),
            "aarch64",
            80,
            palette.top_bar_fg,
        ),
        (sandbox::new(SandboxKind::None), "", 160, palette.top_bar_fg),
        (
            sandbox::new(SandboxKind::Seatbelt),
            "seatbelt",
            160,
            palette.top_bar_fg,
        ),
        (
            sandbox::new(SandboxKind::Seccomp),
            "seccomp",
            160,
            palette.top_bar_fg,
        ),
        (
            sandbox::new(SandboxKind::Capsicum),
            "capsicum",
            160,
            palette.top_bar_fg,
        ),
        (container::new(false, "docker"), "", 150, palette.warning),
        (container::new(true, ""), "container", 150, palette.warning),
        (container::new(true, "jail"), "jail", 150, palette.warning),
        (multiplexer::new(None), "", 120, palette.accent),
        (
            multiplexer::new(Some(&MultiplexerInfo {
                kind: "tmux".into(),
                id: "%42".into(),
            })),
            "tmux %42",
            120,
            palette.accent,
        ),
        (
            multiplexer::new(Some(&MultiplexerInfo {
                kind: "zellij".into(),
                id: "terminal_3".into(),
            })),
            "zellij terminal_3",
            120,
            palette.accent,
        ),
        (
            multiplexer::new(Some(&MultiplexerInfo {
                kind: "screen".into(),
                id: String::new(),
            })),
            "screen",
            120,
            palette.accent,
        ),
    ];
    for (mut widget, label, priority, color) in cases {
        let spec = widget.spec();
        assert_eq!(spec.side, Side::Left);
        assert_eq!(spec.priority, priority);
        assert_eq!(spec.min_width, crate::width::display_width(label));
        for time in ["2026-09-05T12:34:00Z[UTC]", "2026-09-06T01:02:00Z[UTC]"] {
            let update = widget.refresh(&time.parse().unwrap());
            assert!(!update.changed, "static widget must not request redraws");
            assert_eq!(update.next, None);
        }
        let widgets = vec![widget];
        let width = spec.min_width as u16 + 2;
        assert_eq!(text(&render(&widgets, width)), format!(" {label} "));
        if !label.is_empty() {
            assert_eq!(render(&widgets, width)[(1, 0)].fg, color);
            assert_eq!(
                text(&render(&widgets, width - 1)),
                " ".repeat(usize::from(width - 1))
            );
        }
    }
}

fn full_bar_preserves_visual_order_and_priorities() {
    let (_, rx) = watch::channel(PersistenceStatus {
        health: PersistenceHealth::Degraded,
        ..PersistenceStatus::default()
    });
    let widgets = full_bar(rx);
    let specs: Vec<_> = widgets.iter().map(BarWidget::spec).collect();
    let order = layout::arrange(Rect::new(0, 0, 200, 1), &specs)
        .into_iter()
        .map(|placement| {
            let spec = specs[placement.index];
            (spec.id, spec.priority)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        [
            ("hostname", 180),
            ("os", 100),
            ("architecture", 80),
            ("sandbox", 160),
            ("container", 150),
            ("multiplexer", 120),
            ("storage", 200),
            ("persistence", 255),
            ("battery", 240),
            ("clock", 220),
        ]
    );
    let expected = " host │ linux (arch) │ x86_64 │ seccomp │ podman │ tmux %3 SQLITE │ ⚠ log │ ● 87% │ 12:34 ";
    let width = crate::width::display_width(expected) as u16;
    let wide = render(&widgets, width);
    assert_eq!(text(&wide), expected);
    let narrow = text(&render(&widgets, width - 1));
    assert!(
        !narrow.contains("x86_64"),
        "the lowest-priority widget goes first"
    );
    for label in [
        "host",
        "linux (arch)",
        "seccomp",
        "podman",
        "tmux %3",
        "SQLITE",
        "⚠ log",
        "● 87%",
        "12:34",
    ] {
        assert!(narrow.contains(label), "{label} should still fit");
    }
    for width in 0..=200 {
        let buffer = render(&widgets, width);
        assert_eq!(
            crate::width::display_width(&text(&buffer)),
            usize::from(width)
        );
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| cell.bg == crate::theme::palette().top_bar_bg)
        );
        for placement in layout::arrange(buffer.area, &specs) {
            assert_eq!(
                usize::from(placement.area.width),
                specs[placement.index].min_width
            );
        }
    }
    assert_eq!(
        render(&widgets, wide.area.width),
        wide,
        "widening restores all widgets"
    );
}

fn persistence_changes_preempt_and_restore_hidden_widgets() {
    let (tx, rx) = watch::channel(PersistenceStatus::default());
    let mut widgets = full_bar(rx);
    assert_eq!(text(&render(&widgets, 7)), " ● 87% ");
    let now = "2026-09-05T12:34:00Z[UTC]".parse().unwrap();
    for health in [
        PersistenceHealth::Degraded,
        PersistenceHealth::Failing,
        PersistenceHealth::Failed,
    ] {
        tx.send_modify(|status| status.health = health);
        let warning = widgets
            .iter_mut()
            .find(|widget| widget.spec().id == "persistence")
            .unwrap();
        let update = warning.refresh(&now);
        assert!(update.changed);
        assert_eq!(update.next, None, "health is event-driven, not polled");
        assert!(!warning.refresh(&now).changed);
        assert_eq!(
            text(&render(&widgets, 6)),
            " host ",
            "even priority 255 needs its full width"
        );
        let buffer = render(&widgets, 7);
        assert_eq!(text(&buffer), " ⚠ log ");
        let palette = crate::theme::palette();
        assert_eq!(
            buffer[(1, 0)].fg,
            if health == PersistenceHealth::Degraded {
                palette.warning
            } else {
                palette.error
            }
        );
    }
    let mut buffer = render(&widgets, 90);
    tx.send_modify(|status| status.health = PersistenceHealth::Healthy);
    refresh_all(&mut widgets);
    Bar {
        widgets: &widgets,
        palette: crate::theme::palette(),
    }
    .render_ref(buffer.area, &mut buffer);
    assert_eq!(
        buffer,
        render(&widgets, 90),
        "recovery must clear old warning cells"
    );
    assert!(!text(&buffer).contains("⚠ log"));
    assert!(!text(&buffer).contains("│  │"));
    assert_eq!(text(&render(&widgets, 7)), " ● 87% ");
}

fn storage_backend_changes_remeasure_whole_labels() {
    let (tx, rx) = watch::channel(PersistenceStatus::default());
    let mut widgets = [storage::new(rx)];
    let now = "2026-09-05T12:34:00Z[UTC]".parse().unwrap();
    for (backend, label, changed) in [
        (RuntimeStorageBackend::Sqlite, "SQLITE", false),
        (RuntimeStorageBackend::Postgres, "🐘", true),
        (RuntimeStorageBackend::Sqlite, "SQLITE", true),
    ] {
        tx.send_modify(|status| status.backend = backend);
        let update = widgets[0].refresh(&now);
        assert_eq!(update.changed, changed);
        assert_eq!(update.next, None);
        assert!(!widgets[0].refresh(&now).changed);
        let label_width = crate::width::display_width(label) as u16;
        assert_eq!(widgets[0].spec().min_width, usize::from(label_width));
        let palette = crate::theme::palette();
        let color = if backend == RuntimeStorageBackend::Postgres {
            palette.accent
        } else {
            palette.top_bar_fg
        };
        for width in 0..=20 {
            let mut expected = render(&[], width);
            if width >= label_width + 2 {
                Line::from(label).style(Style::default().fg(color)).render(
                    Rect::new(width - 1 - label_width, 0, label_width, 1),
                    &mut expected,
                );
            }
            assert_eq!(render(&widgets, width), expected);
        }
    }
    tx.send_modify(|status| status.health = PersistenceHealth::Failed);
    assert!(
        !widgets[0].refresh(&now).changed,
        "health alone does not change storage"
    );
}

fn three_widgets_resize_by_priority_and_reappear() {
    let (_, rx) = watch::channel(PowerInfo {
        has_battery: true,
        battery_level: Some(87),
        charger_connected: false,
    });
    let mut widgets = [
        hostname::new("host".into()),
        battery::new(rx),
        clock("2026-09-05T12:34:00Z[UTC]"),
    ];
    let now = "2026-09-05T12:34:00Z[UTC]".parse().unwrap();
    for widget in &mut widgets {
        widget.refresh(&now);
    }
    for (width, expected) in [
        (20, " host ● 87% │ 12:34 "),
        (19, "     ● 87% │ 12:34 "),
        (7, " ● 87% "),
        (6, " host "),
        (7, " ● 87% "),
        (20, " host ● 87% │ 12:34 "),
    ] {
        assert_eq!(text(&render(&widgets, width)), expected);
    }
}

fn battery_snapshots_update_hidden_widgets_and_preserve_unknown_state() {
    let (tx, rx) = watch::channel(PowerInfo::default());
    let mut widgets = vec![battery::new(rx)];
    let now = "2026-09-05T12:34:00Z[UTC]".parse().unwrap();
    assert!(!widgets[0].refresh(&now).changed);
    assert_eq!(widgets[0].spec().min_width, 0);
    for (power, expected_text, color) in [
        (
            PowerInfo {
                has_battery: true,
                battery_level: Some(15),
                charger_connected: false,
            },
            "● 15%",
            crate::theme::palette().error,
        ),
        (
            PowerInfo {
                has_battery: true,
                battery_level: Some(15),
                charger_connected: true,
            },
            "⚡ 15%",
            crate::theme::palette().success,
        ),
        (
            PowerInfo {
                has_battery: true,
                battery_level: None,
                charger_connected: false,
            },
            "● ?%",
            crate::theme::palette().warning,
        ),
        (
            PowerInfo {
                has_battery: true,
                battery_level: Some(100),
                charger_connected: false,
            },
            "● 100%",
            crate::theme::palette().top_bar_fg,
        ),
    ] {
        tx.send_replace(power);
        let update = widgets[0].refresh(&now);
        assert!(update.changed);
        assert_eq!(update.next, None, "the runtime owns power polling");
        assert!(
            !widgets[0].refresh(&now).changed,
            "identical samples do not redraw"
        );
        assert_eq!(
            widgets[0].spec().min_width,
            crate::width::display_width(expected_text)
        );
        assert_eq!(
            text(&render(&widgets, 3)),
            "   ",
            "hidden does not mean stale"
        );
        let mut expected = Buffer::empty(Rect::new(0, 0, 20, 1));
        let palette = crate::theme::palette();
        Bar {
            widgets: &[],
            palette,
        }
        .render_ref(expected.area, &mut expected);
        let width = crate::width::display_width(expected_text) as u16;
        ratatui::widgets::Widget::render(
            Line::from(expected_text).style(Style::default().fg(color)),
            Rect::new(20 - 1 - width, 0, width, 1),
            &mut expected,
        );
        assert_eq!(render(&widgets, 20), expected);
    }
    tx.send_replace(PowerInfo::default());
    assert!(widgets[0].refresh(&now).changed);
    assert_eq!(widgets[0].spec().min_width, 0);
    widgets.push(clock("2026-09-05T12:34:00Z[UTC]"));
    assert_eq!(text(&render(&widgets, 20)), "              12:34 ");
}

fn clock_fills_row_and_hides_whole_below_minimum_width() {
    let widgets = vec![clock("2026-09-05T12:34:00Z[UTC]")];
    for width in [0, 1, 2, 5, 6, 7, 8, 40, 80, 120, 240] {
        let buffer = render(&widgets, width);
        assert_eq!(buffer.area.width, width);
        let expected = if width >= 7 {
            format!("{}12:34 ", " ".repeat(usize::from(width - 6)))
        } else {
            " ".repeat(usize::from(width))
        };
        assert_eq!(text(&buffer), expected);
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| cell.bg == crate::theme::palette().top_bar_bg)
        );
    }
}

fn clock_updates_on_wall_clock_minute_boundaries() {
    let mut clock = widgets::clock::new();
    let first = clock.refresh(&"2026-09-05T23:59:59.250Z[UTC]".parse().unwrap());
    assert!(first.changed);
    assert_eq!(first.next, Some(Duration::from_millis(750)));

    let same = clock.refresh(&"2026-09-05T23:59:59.750Z[UTC]".parse().unwrap());
    assert!(!same.changed);
    assert_eq!(same.next, Some(Duration::from_millis(250)));

    let midnight = clock.refresh(&"2026-09-06T00:00:00Z[UTC]".parse().unwrap());
    assert!(midnight.changed);
    assert_eq!(midnight.next, Some(Duration::from_secs(60)));
    assert_eq!(text(&render(&[clock], 7)), " 00:00 ");

    let mut clock = widgets::clock::new();
    clock.refresh(&"2026-09-05T12:34:00Z[UTC]".parse().unwrap());
    // A resumed machine samples the actual time, not one synthetic missed tick.
    clock.refresh(&"2026-09-05T15:20:12Z[UTC]".parse().unwrap());
    assert_eq!(text(&render(&[clock], 7)), " 15:20 ");
}

fn spec(id: &'static str, side: Side, priority: u8, min_width: usize) -> WidgetSpec {
    WidgetSpec {
        id,
        side,
        priority,
        min_width,
    }
}

fn priority_admission_preserves_visual_order_and_spacing() {
    let specs = [
        spec("first", Side::Left, 0, 4),
        spec("second", Side::Right, 200, 5),
        spec("third", Side::Right, 255, 3),
        spec("fourth", Side::Right, 255, 3),
    ];
    let ids = |width| {
        layout::arrange(Rect::new(0, 0, width, 1), &specs)
            .into_iter()
            .map(|p| specs[p.index].id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(4), Vec::<&str>::new());
    assert_eq!(ids(5), ["third"]);
    assert_eq!(ids(10), ["first", "third"]);
    assert_eq!(ids(11), ["third", "fourth"]);
    assert_eq!(ids(19), ["second", "third", "fourth"]);
    assert_eq!(ids(24), ["first", "second", "third", "fourth"]);

    let placements = layout::arrange(Rect::new(0, 0, 24, 1), &specs);
    assert_eq!(placements[0].area, Rect::new(1, 0, 4, 1));
    assert_eq!(placements[1].area, Rect::new(6, 0, 5, 1));
    assert_eq!(placements[2].separator, Some(Rect::new(11, 0, 3, 1)));
    assert_eq!(placements[2].area, Rect::new(14, 0, 3, 1));
    assert_eq!(placements[3].separator, Some(Rect::new(17, 0, 3, 1)));
    assert_eq!(placements[3].area, Rect::new(20, 0, 3, 1));
}

fn oversized_widgets_do_not_block_smaller_ones() {
    let specs = [
        spec("oversized", Side::Left, 255, usize::MAX),
        spec("empty", Side::Left, 250, 0),
        spec("fits", Side::Right, 0, 5),
    ];
    let placements = layout::arrange(Rect::new(0, 0, 7, 1), &specs);
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].index, 2);
    assert_eq!(placements[0].area, Rect::new(1, 0, 5, 1));
}

fn unicode_widgets_have_bounded_non_overlapping_rectangles() {
    let specs = [
        spec(
            "unicode",
            Side::Left,
            180,
            crate::width::display_width("机器-cafe\u{301}"),
        ),
        spec("icon", Side::Right, 240, crate::width::display_width("🐘")),
        spec("clock", Side::Right, 220, 5),
    ];
    for width in 0..100 {
        let area = Rect::new(4, 2, width, 1);
        let placements = layout::arrange(area, &specs);
        let mut cells = std::collections::HashSet::new();
        for placement in placements {
            assert_eq!(
                usize::from(placement.area.width),
                specs[placement.index].min_width
            );
            for rect in [Some(placement.area), placement.separator]
                .into_iter()
                .flatten()
            {
                assert_eq!(rect.intersection(area), rect);
                for x in rect.left()..rect.right() {
                    assert!(cells.insert(x), "widgets/separators must not overlap");
                }
            }
        }
    }
}

fn background_replaces_stale_cells_and_tracks_mode() {
    use chaos_ipc::config_types::ModeKind;

    let area = Rect::new(0, 0, 40, 1);
    let mut buffer = Buffer::empty(area);
    for cell in &mut buffer.content {
        cell.set_symbol("X");
    }
    for mode in [ModeKind::Default, ModeKind::Plan] {
        let palette = crate::theme::palette_for_mode(mode, false);
        Bar {
            widgets: &[],
            palette,
        }
        .render_ref(area, &mut buffer);
        assert_eq!(text(&buffer), " ".repeat(40));
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| cell.bg == palette.top_bar_bg)
        );
    }
}

#[cfg(feature = "vt100-tests")]
fn pinned_terminal(
    width: u16,
) -> crate::custom_terminal::Terminal<crate::test_backend::VT100Backend> {
    use ratatui::layout::Position;

    let backend = crate::test_backend::VT100Backend::new(width, 5);
    let mut terminal = crate::custom_terminal::Terminal::with_options(backend).unwrap();
    terminal.set_viewport_area(Rect::new(0, 3, width, 1));
    crossterm::queue!(
        terminal.backend_mut(),
        crossterm::cursor::MoveTo(0, 1),
        crossterm::style::Print("H"),
    )
    .unwrap();
    terminal.set_cursor_position(Position::new(0, 2)).unwrap();
    terminal
}

#[cfg(feature = "vt100-tests")]
fn assert_pinned_row(
    terminal: &crate::custom_terminal::Terminal<crate::test_backend::VT100Backend>,
    buffer: &Buffer,
) {
    let width = buffer.area.width;
    let screen = terminal.backend().vt100().screen();
    assert_eq!(screen.cursor_position(), (2, 0));
    assert_eq!(screen.cell(1, 0).unwrap().contents(), "H");
    assert_eq!(terminal.viewport_area, Rect::new(0, 3, width, 1));
    assert_eq!(
        terminal.last_known_cursor_pos,
        ratatui::layout::Position::new(0, 2)
    );
    for x in 0..width {
        let cell = screen.cell(0, x).unwrap();
        if !cell.is_wide_continuation() {
            // Erased and explicitly written spaces are the same display column.
            let actual = cell.contents();
            assert_eq!(
                if actual.is_empty() { " " } else { actual },
                buffer[(x, 0)].symbol(),
                "width {width}, column {x}",
            );
            assert_eq!(cell.bgcolor(), screen.cell(0, 0).unwrap().bgcolor());
        } else {
            // vt100 stores default attributes in continuation cells;
            // the leading cell carries the wide glyph's actual style.
            assert!(x > 0);
            assert_eq!(crate::width::display_width(buffer[(x - 1, 0)].symbol()), 2);
        }
    }
    assert_ne!(screen.cell(0, 0).unwrap().bgcolor(), vt100::Color::Default);
}

#[cfg(feature = "vt100-tests")]
fn pinned_full_bar_clears_backend_and_warning_transitions() {
    use ratatui::backend::Backend;

    let (tx, rx) = watch::channel(PersistenceStatus::default());
    let mut widgets = full_bar(rx);
    for width in [4, 7, 23, 40, 90, 120] {
        let mut terminal = pinned_terminal(width);
        for (backend, health) in [
            (RuntimeStorageBackend::Sqlite, PersistenceHealth::Healthy),
            (RuntimeStorageBackend::Postgres, PersistenceHealth::Failing),
            (RuntimeStorageBackend::Sqlite, PersistenceHealth::Healthy),
            (RuntimeStorageBackend::Postgres, PersistenceHealth::Healthy),
        ] {
            tx.send_replace(PersistenceStatus { backend, health });
            refresh_all(&mut widgets);
            let buffer = render(&widgets, width);
            terminal.draw_pinned_row(&buffer).unwrap();
            Backend::flush(terminal.backend_mut()).unwrap();
            assert_pinned_row(&terminal, &buffer);
        }
    }
}

#[cfg(feature = "vt100-tests")]
fn pinned_widget_row_preserves_cursor_viewport_and_history() {
    use ratatui::backend::Backend;

    for width in [1, 6, 7, 40, 120] {
        let mut terminal = pinned_terminal(width);
        let widgets = vec![clock("2026-09-05T12:34:00Z[UTC]")];
        let buffer = render(&widgets, width);
        terminal.draw_pinned_row(&buffer).unwrap();
        Backend::flush(terminal.backend_mut()).unwrap();
        assert_pinned_row(&terminal, &buffer);
        assert_eq!(
            &*terminal.get_frame().buffer,
            &Buffer::empty(Rect::new(0, 3, width, 1)),
        );

        // Cover a previous row after a mode/visibility change, not just first draw.
        terminal.draw_pinned_row(&render(&[], width)).unwrap();
        for x in 0..width {
            assert!(
                terminal
                    .backend()
                    .vt100()
                    .screen()
                    .cell(0, x)
                    .unwrap()
                    .contents()
                    .trim()
                    .is_empty()
            );
        }
    }
}
