mod rendering;
mod selection_logic;
mod types;

pub use selection_logic::ListSelectionView;
pub use types::ColumnWidthMode;
pub use types::SelectionAction;
pub use types::SelectionItem;
pub use types::SelectionViewParams;
pub use types::SideContentWidth;
pub use types::popup_content_width;
pub use types::side_by_side_layout_widths;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use crate::bottom_pane::popup_consts::standard_popup_hint_line;
    use crate::test_support::make_app_event_sender;
    use crate::test_support::make_app_event_sender_with_rx;
    use crate::test_support::renderable_string_with_size;
    use crossterm::event::KeyCode;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui::style::Style;

    use types::SIDE_CONTENT_GAP;

    struct MarkerRenderable {
        marker: &'static str,
        height: u16,
    }

    impl Renderable for MarkerRenderable {
        fn render(&self, area: Rect, buf: &mut Buffer) {
            for y in area.y..area.y.saturating_add(area.height) {
                for x in area.x..area.x.saturating_add(area.width) {
                    if x < buf.area().width && y < buf.area().height {
                        buf[(x, y)].set_symbol(self.marker);
                    }
                }
            }
        }

        fn desired_height(&self, _width: u16) -> u16 {
            self.height
        }
    }

    struct StyledMarkerRenderable {
        marker: &'static str,
        style: Style,
        height: u16,
    }

    impl Renderable for StyledMarkerRenderable {
        fn render(&self, area: Rect, buf: &mut Buffer) {
            for y in area.y..area.y.saturating_add(area.height) {
                for x in area.x..area.x.saturating_add(area.width) {
                    if x < buf.area().width && y < buf.area().height {
                        buf[(x, y)].set_symbol(self.marker).set_style(self.style);
                    }
                }
            }
        }

        fn desired_height(&self, _width: u16) -> u16 {
            self.height
        }
    }

    fn make_selection_view(subtitle: Option<&str>) -> ListSelectionView {
        let tx = make_app_event_sender();
        let items = vec![
            SelectionItem {
                name: "Read Only".to_string(),
                description: Some("Chaos can read files".to_string()),
                is_current: true,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Full Access".to_string(),
                description: Some("Chaos can edit files".to_string()),
                is_current: false,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];
        ListSelectionView::new(
            SelectionViewParams {
                title: Some("Select Approval Mode".to_string()),
                subtitle: subtitle.map(str::to_string),
                footer_hint: Some(standard_popup_hint_line()),
                items,
                ..Default::default()
            },
            tx,
        )
    }

    fn render_lines(view: &ListSelectionView) -> String {
        render_lines_with_width(view, 48)
    }

    fn render_lines_with_width(view: &ListSelectionView, width: u16) -> String {
        render_lines_in_area(view, width, view.desired_height(width))
    }

    fn render_lines_in_area(view: &ListSelectionView, width: u16, height: u16) -> String {
        renderable_string_with_size(view, width, height)
    }

    fn description_col(rendered: &str, item_marker: &str, description: &str) -> usize {
        let line = rendered
            .lines()
            .find(|line| line.contains(item_marker) && line.contains(description))
            .expect("expected rendered line to contain row marker and description");
        line.find(description)
            .expect("expected rendered line to contain description")
    }

    fn make_scrolling_width_items() -> Vec<SelectionItem> {
        let mut items: Vec<SelectionItem> = (1..=8)
            .map(|idx| SelectionItem {
                name: format!("Item {idx}"),
                description: Some(format!("desc {idx}")),
                dismiss_on_select: true,
                ..Default::default()
            })
            .collect();
        items.push(SelectionItem {
            name: "Item 9 with an intentionally much longer name".to_string(),
            description: Some("desc 9".to_string()),
            dismiss_on_select: true,
            ..Default::default()
        });
        items
    }

    fn render_before_after_scroll_snapshot(col_width_mode: ColumnWidthMode, width: u16) -> String {
        let tx = make_app_event_sender();
        let mut view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Debug".to_string()),
                items: make_scrolling_width_items(),
                col_width_mode,
                ..Default::default()
            },
            tx,
        );

        let before_scroll = render_lines_with_width(&view, width);
        for _ in 0..8 {
            view.handle_key_event(KeyEvent::from(KeyCode::Down));
        }
        let after_scroll = render_lines_with_width(&view, width);

        format!("before scroll:\n{before_scroll}\n\nafter scroll:\n{after_scroll}")
    }

    use super::super::bottom_pane_view::BottomPaneView;
    use crate::render::renderable::Renderable;
    use crossterm::event::KeyEvent;
    use ratatui::style::Stylize;

    #[test]
    fn renders_blank_line_between_title_and_items_without_subtitle() {
        let view = make_selection_view(None);
        assert_snapshot!(
            "list_selection_spacing_without_subtitle",
            render_lines(&view)
        );
    }

    #[test]
    fn renders_blank_line_between_subtitle_and_items() {
        let view = make_selection_view(Some("Switch between Chaos approval presets"));
        assert_snapshot!("list_selection_spacing_with_subtitle", render_lines(&view));
    }

    #[test]
    fn theme_picker_subtitle_uses_fallback_text_in_94x35_terminal() {
        let tx = make_app_event_sender();
        let home = dirs::home_dir().expect("home directory should be available");
        let chaos_home = home.join(".chaos");
        let params =
            crate::theme_picker::build_theme_picker_params(None, Some(&chaos_home), Some(94));
        let view = ListSelectionView::new(params, tx);

        let rendered = render_lines_in_area(&view, 94, 35);
        assert!(rendered.contains("Move up/down to live preview themes"));
    }

    #[test]
    fn theme_picker_enables_side_content_background_preservation() {
        let params = crate::theme_picker::build_theme_picker_params(None, None, Some(120));
        assert!(
            params.preserve_side_content_bg,
            "theme picker should preserve side-content backgrounds to keep diff preview styling",
        );
    }

    #[test]
    fn preserve_side_content_bg_keeps_rendered_background_colors() {
        let tx = make_app_event_sender();
        let view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Debug".to_string()),
                items: vec![SelectionItem {
                    name: "Item 1".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                }],
                side_content: Box::new(StyledMarkerRenderable {
                    marker: "+",
                    style: Style::default().bg(Color::Blue),
                    height: 1,
                }),
                side_content_width: SideContentWidth::Half,
                side_content_min_width: 10,
                preserve_side_content_bg: true,
                ..Default::default()
            },
            tx,
        );
        let area = Rect::new(0, 0, 120, 35);
        let mut buf = Buffer::empty(area);

        view.render(area, &mut buf);

        let plus_bg = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .find_map(|(x, y)| {
                let cell = &buf[(x, y)];
                (cell.symbol() == "+").then(|| cell.style().bg)
            })
            .expect("expected side content to render at least one '+' marker");
        assert_eq!(
            plus_bg,
            Some(Color::Blue),
            "expected side-content marker to preserve custom background styling",
        );
    }

    #[test]
    fn snapshot_footer_note_wraps() {
        let tx = make_app_event_sender();
        let items = vec![SelectionItem {
            name: "Read Only".to_string(),
            description: Some("Chaos can read files".to_string()),
            is_current: true,
            dismiss_on_select: true,
            ..Default::default()
        }];
        let footer_note = ratatui::text::Line::from(vec![
            "Note: ".dim(),
            "Use /setup-default-sandbox".cyan(),
            " to allow network access.".dim(),
        ]);
        let view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Select Approval Mode".to_string()),
                footer_note: Some(footer_note),
                footer_hint: Some(standard_popup_hint_line()),
                items,
                ..Default::default()
            },
            tx,
        );
        assert_snapshot!(
            "list_selection_footer_note_wraps",
            render_lines_with_width(&view, 40)
        );
    }

    #[test]
    fn renders_search_query_line_when_enabled() {
        let tx = make_app_event_sender();
        let items = vec![SelectionItem {
            name: "Read Only".to_string(),
            description: Some("Chaos can read files".to_string()),
            is_current: false,
            dismiss_on_select: true,
            ..Default::default()
        }];
        let mut view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Select Approval Mode".to_string()),
                footer_hint: Some(standard_popup_hint_line()),
                items,
                is_searchable: true,
                search_placeholder: Some("Type to search branches".to_string()),
                ..Default::default()
            },
            tx,
        );
        view.set_search_query("filters".to_string());

        let lines = render_lines(&view);
        assert!(
            lines.contains("filters"),
            "expected search query line to include rendered query, got {lines:?}"
        );
    }

    #[test]
    fn enter_with_no_matches_triggers_cancel_callback() {
        let (tx, mut rx) = make_app_event_sender_with_rx();
        let mut view = ListSelectionView::new(
            SelectionViewParams {
                items: vec![SelectionItem {
                    name: "Read Only".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                }],
                is_searchable: true,
                on_cancel: Some(Box::new(|tx: &_| {
                    tx.send(AppEvent::OpenApprovalsPopup);
                })),
                ..Default::default()
            },
            tx,
        );
        view.set_search_query("no-matches".to_string());

        view.handle_key_event(KeyEvent::from(KeyCode::Enter));

        assert!(view.is_complete());
        match rx.try_recv() {
            Ok(AppEvent::OpenApprovalsPopup) => {}
            Ok(other) => panic!("expected OpenApprovalsPopup cancel event, got {other:?}"),
            Err(err) => panic!("expected cancel callback event, got {err}"),
        }
    }

    #[test]
    fn move_down_without_selection_change_does_not_fire_callback() {
        let (tx, mut rx) = make_app_event_sender_with_rx();
        let mut view = ListSelectionView::new(
            SelectionViewParams {
                items: vec![SelectionItem {
                    name: "Only choice".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                }],
                on_selection_changed: Some(Box::new(|_idx, tx: &_| {
                    tx.send(AppEvent::OpenApprovalsPopup);
                })),
                ..Default::default()
            },
            tx,
        );

        while rx.try_recv().is_ok() {}

        view.handle_key_event(KeyEvent::from(KeyCode::Down));

        assert!(
            rx.try_recv().is_err(),
            "moving down in a single-item list should not fire on_selection_changed",
        );
    }

    #[test]
    fn wraps_long_option_without_overflowing_columns() {
        let tx = make_app_event_sender();
        let items = vec![
            SelectionItem {
                name: "Yes, proceed".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Yes, and don't ask again for commands that start with `python -mpre_commit run --files eslint-plugin/no-mixed-const-enum-exports.js`".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            },
        ];
        let view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Approval".to_string()),
                items,
                ..Default::default()
            },
            tx,
        );

        let rendered = render_lines_with_width(&view, 60);
        let command_line = rendered
            .lines()
            .find(|line| line.contains("python -mpre_commit run"))
            .expect("rendered lines should include wrapped command");
        assert!(
            command_line.starts_with("     `python -mpre_commit run"),
            "wrapped command line should align under the numbered prefix:\n{rendered}"
        );
        assert!(
            rendered.contains("eslint-plugin/no-")
                && rendered.contains("mixed-const-enum-exports.js"),
            "long command should not be truncated even when wrapped:\n{rendered}"
        );
    }

    #[test]
    fn width_changes_do_not_hide_rows() {
        let tx = make_app_event_sender();
        let items = vec![
            SelectionItem {
                name: "gpt-5.1-codex".to_string(),
                description: Some(
                    "Optimized for Codex. Balance of reasoning quality and coding ability."
                        .to_string(),
                ),
                is_current: true,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "gpt-5.1-codex-mini".to_string(),
                description: Some(
                    "Optimized for Codex. Cheaper, faster, but less capable.".to_string(),
                ),
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "gpt-4.1-codex".to_string(),
                description: Some(
                    "Legacy model. Use when you need compatibility with older automations."
                        .to_string(),
                ),
                dismiss_on_select: true,
                ..Default::default()
            },
        ];
        let view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Select Model and Effort".to_string()),
                items,
                ..Default::default()
            },
            tx,
        );
        let mut missing: Vec<u16> = Vec::new();
        for width in 60..=90 {
            let rendered = render_lines_with_width(&view, width);
            if !rendered.contains("3.") {
                missing.push(width);
            }
        }
        assert!(
            missing.is_empty(),
            "third option missing at widths {missing:?}"
        );
    }

    #[test]
    fn narrow_width_keeps_all_rows_visible() {
        let tx = make_app_event_sender();
        let desc = "x".repeat(10);
        let items: Vec<SelectionItem> = (1..=3)
            .map(|idx| SelectionItem {
                name: format!("Item {idx}"),
                description: Some(desc.clone()),
                dismiss_on_select: true,
                ..Default::default()
            })
            .collect();
        let view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Debug".to_string()),
                items,
                ..Default::default()
            },
            tx,
        );
        let rendered = render_lines_with_width(&view, 24);
        assert!(
            rendered.contains("3."),
            "third option missing for width 24:\n{rendered}"
        );
    }

    #[test]
    fn snapshot_model_picker_width_80() {
        let tx = make_app_event_sender();
        let items = vec![
            SelectionItem {
                name: "gpt-5.1-codex".to_string(),
                description: Some(
                    "Optimized for Codex. Balance of reasoning quality and coding ability."
                        .to_string(),
                ),
                is_current: true,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "gpt-5.1-codex-mini".to_string(),
                description: Some(
                    "Optimized for Codex. Cheaper, faster, but less capable.".to_string(),
                ),
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "gpt-4.1-codex".to_string(),
                description: Some(
                    "Legacy model. Use when you need compatibility with older automations."
                        .to_string(),
                ),
                dismiss_on_select: true,
                ..Default::default()
            },
        ];
        let view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Select Model and Effort".to_string()),
                items,
                ..Default::default()
            },
            tx,
        );
        assert_snapshot!(
            "list_selection_model_picker_width_80",
            render_lines_with_width(&view, 80)
        );
    }

    #[test]
    fn snapshot_narrow_width_preserves_third_option() {
        let tx = make_app_event_sender();
        let desc = "x".repeat(10);
        let items: Vec<SelectionItem> = (1..=3)
            .map(|idx| SelectionItem {
                name: format!("Item {idx}"),
                description: Some(desc.clone()),
                dismiss_on_select: true,
                ..Default::default()
            })
            .collect();
        let view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Debug".to_string()),
                items,
                ..Default::default()
            },
            tx,
        );
        assert_snapshot!(
            "list_selection_narrow_width_preserves_rows",
            render_lines_with_width(&view, 24)
        );
    }

    #[test]
    fn snapshot_auto_visible_col_width_mode_scroll_behavior() {
        assert_snapshot!(
            "list_selection_col_width_mode_auto_visible_scroll",
            render_before_after_scroll_snapshot(ColumnWidthMode::AutoVisible, 96)
        );
    }

    #[test]
    fn snapshot_auto_all_rows_col_width_mode_scroll_behavior() {
        assert_snapshot!(
            "list_selection_col_width_mode_auto_all_rows_scroll",
            render_before_after_scroll_snapshot(ColumnWidthMode::AutoAllRows, 96)
        );
    }

    #[test]
    fn snapshot_fixed_col_width_mode_scroll_behavior() {
        assert_snapshot!(
            "list_selection_col_width_mode_fixed_scroll",
            render_before_after_scroll_snapshot(ColumnWidthMode::Fixed, 96)
        );
    }

    #[test]
    fn auto_all_rows_col_width_does_not_shift_when_scrolling() {
        let tx = make_app_event_sender();

        let mut view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Debug".to_string()),
                items: make_scrolling_width_items(),
                col_width_mode: ColumnWidthMode::AutoAllRows,
                ..Default::default()
            },
            tx,
        );

        let before_scroll = render_lines_with_width(&view, 96);
        for _ in 0..8 {
            view.handle_key_event(KeyEvent::from(KeyCode::Down));
        }
        let after_scroll = render_lines_with_width(&view, 96);

        assert!(
            after_scroll.contains("9. Item 9 with an intentionally much longer name"),
            "expected the scrolled view to include the longer row:\n{after_scroll}"
        );

        let before_col = description_col(&before_scroll, "8. Item 8", "desc 8");
        let after_col = description_col(&after_scroll, "8. Item 8", "desc 8");
        assert_eq!(
            before_col, after_col,
            "description column changed across scroll:\nbefore:\n{before_scroll}\nafter:\n{after_scroll}"
        );
    }

    #[test]
    fn fixed_col_width_is_30_70_and_does_not_shift_when_scrolling() {
        let tx = make_app_event_sender();
        let width = 96;
        let mut view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Debug".to_string()),
                items: make_scrolling_width_items(),
                col_width_mode: ColumnWidthMode::Fixed,
                ..Default::default()
            },
            tx,
        );

        let before_scroll = render_lines_with_width(&view, width);
        let before_col = description_col(&before_scroll, "8. Item 8", "desc 8");
        let expected_desc_col = ((width.saturating_sub(2) as usize) * 3) / 10;
        assert_eq!(
            before_col, expected_desc_col,
            "fixed mode should place description column at a 30/70 split:\n{before_scroll}"
        );

        for _ in 0..8 {
            view.handle_key_event(KeyEvent::from(KeyCode::Down));
        }
        let after_scroll = render_lines_with_width(&view, width);
        let after_col = description_col(&after_scroll, "8. Item 8", "desc 8");
        assert_eq!(
            before_col, after_col,
            "fixed description column changed across scroll:\nbefore:\n{before_scroll}\nafter:\n{after_scroll}"
        );
    }

    #[test]
    fn side_layout_width_half_uses_exact_split() {
        let tx = make_app_event_sender();
        let view = ListSelectionView::new(
            SelectionViewParams {
                items: vec![SelectionItem {
                    name: "Item 1".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                }],
                side_content: Box::new(MarkerRenderable {
                    marker: "W",
                    height: 1,
                }),
                side_content_width: SideContentWidth::Half,
                side_content_min_width: 10,
                ..Default::default()
            },
            tx,
        );

        let content_width: u16 = 120;
        let expected = content_width.saturating_sub(SIDE_CONTENT_GAP) / 2;
        assert_eq!(view.side_layout_width(content_width), Some(expected));
    }

    #[test]
    fn side_layout_width_half_falls_back_when_list_would_be_too_narrow() {
        let tx = make_app_event_sender();
        let view = ListSelectionView::new(
            SelectionViewParams {
                items: vec![SelectionItem {
                    name: "Item 1".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                }],
                side_content: Box::new(MarkerRenderable {
                    marker: "W",
                    height: 1,
                }),
                side_content_width: SideContentWidth::Half,
                side_content_min_width: 50,
                ..Default::default()
            },
            tx,
        );

        assert_eq!(view.side_layout_width(80), None);
    }

    #[test]
    fn stacked_side_content_is_used_when_side_by_side_does_not_fit() {
        let tx = make_app_event_sender();
        let view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Debug".to_string()),
                items: vec![SelectionItem {
                    name: "Item 1".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                }],
                side_content: Box::new(MarkerRenderable {
                    marker: "W",
                    height: 1,
                }),
                stacked_side_content: Some(Box::new(MarkerRenderable {
                    marker: "N",
                    height: 1,
                })),
                side_content_width: SideContentWidth::Half,
                side_content_min_width: 60,
                ..Default::default()
            },
            tx,
        );

        let rendered = render_lines_with_width(&view, 70);
        assert!(
            rendered.contains('N'),
            "expected stacked marker to be rendered:\n{rendered}"
        );
        assert!(
            !rendered.contains('W'),
            "wide marker should not render in stacked mode:\n{rendered}"
        );
    }

    #[test]
    fn side_content_clearing_resets_symbols_and_style() {
        let tx = make_app_event_sender();
        let view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Debug".to_string()),
                items: vec![SelectionItem {
                    name: "Item 1".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                }],
                side_content: Box::new(MarkerRenderable {
                    marker: "W",
                    height: 1,
                }),
                side_content_width: SideContentWidth::Half,
                side_content_min_width: 10,
                ..Default::default()
            },
            tx,
        );

        let width = 120;
        let height = view.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        for y in 0..height {
            for x in 0..width {
                buf[(x, y)]
                    .set_symbol("X")
                    .set_style(Style::default().bg(crate::theme::red()));
            }
        }
        view.render(area, &mut buf);

        let cell = &buf[(width - 1, 0)];
        assert_eq!(cell.symbol(), " ");
        let style = cell.style();
        assert_eq!(style.fg, Some(Color::Reset));
        assert_eq!(style.bg, Some(Color::Reset));
        assert_eq!(style.underline_color, Some(Color::Reset));

        let mut saw_marker = false;
        for y in 0..height {
            for x in 0..width {
                let cell = &buf[(x, y)];
                if cell.symbol() == "W" {
                    saw_marker = true;
                    assert_eq!(cell.style().bg, Some(Color::Reset));
                }
            }
        }
        assert!(
            saw_marker,
            "expected side marker renderable to draw into buffer"
        );
    }

    #[test]
    fn side_content_clearing_handles_non_zero_buffer_origin() {
        let tx = make_app_event_sender();
        let view = ListSelectionView::new(
            SelectionViewParams {
                title: Some("Debug".to_string()),
                items: vec![SelectionItem {
                    name: "Item 1".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                }],
                side_content: Box::new(MarkerRenderable {
                    marker: "W",
                    height: 1,
                }),
                side_content_width: SideContentWidth::Half,
                side_content_min_width: 10,
                ..Default::default()
            },
            tx,
        );

        let width = 120;
        let height = view.desired_height(width);
        let area = Rect::new(0, 20, width, height);
        let mut buf = Buffer::empty(area);
        for y in area.y..area.y + height {
            for x in area.x..area.x + width {
                buf[(x, y)]
                    .set_symbol("X")
                    .set_style(Style::default().bg(crate::theme::red()));
            }
        }
        view.render(area, &mut buf);

        let cell = &buf[(area.x + width - 1, area.y)];
        assert_eq!(cell.symbol(), " ");
        assert_eq!(cell.style().bg, Some(Color::Reset));
    }
}
