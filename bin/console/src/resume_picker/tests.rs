use super::rendering::ColumnVisibility;
#[cfg(feature = "vt100-tests")]
use super::rendering::calculate_column_metrics;
use super::rendering::column_visibility;
#[cfg(feature = "vt100-tests")]
use super::rendering::render_column_headers;
#[cfg(feature = "vt100-tests")]
use super::rendering::render_list;
#[cfg(feature = "vt100-tests")]
use super::rendering::search_line;
use super::row::Row;
use super::row::head_to_row;
use super::row::rows_from_items;
use super::state::BackgroundEvent;
use super::state::PageLoadRequest;
use super::state::PickerState;
use super::*;
use chaos_ipc::ProcessId;
use chaos_kern::Cursor;
use chaos_kern::ProcessItem;
use chaos_kern::ProcessSortKey;
use chaos_kern::ProcessesPage;
use jiff::Timestamp;
#[cfg(feature = "vt100-tests")]
use jiff::ToSpan;

use crate::tui::FrameRequester;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
#[cfg(feature = "vt100-tests")]
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
#[cfg(feature = "vt100-tests")]
use ratatui::layout::Rect;
use std::sync::Arc;
use std::sync::Mutex;

fn make_item(ts: &str, preview: &str) -> ProcessItem {
    make_item_with_id(ProcessId::new(), ts, preview)
}

fn test_picker_state(loader: super::state::PageLoader) -> PickerState {
    PickerState::new(
        FrameRequester::test_dummy(),
        loader,
        String::from("openai"),
        true,
        None,
        SessionPickerAction::Resume,
    )
}

fn make_item_with_id(process_id: ProcessId, ts: &str, preview: &str) -> ProcessItem {
    ProcessItem {
        process_id: Some(process_id),
        first_user_message: Some(preview.to_string()),
        created_at: Some(ts.to_string()),
        updated_at: Some(ts.to_string()),
        ..Default::default()
    }
}

fn cursor_from_str(repr: &str) -> Cursor {
    serde_json::from_str::<Cursor>(&format!("\"{repr}\""))
        .expect("cursor format should deserialize")
}

fn page(
    items: Vec<ProcessItem>,
    next_cursor: Option<Cursor>,
    num_scanned_records: usize,
    reached_scan_limit: bool,
) -> ProcessesPage {
    ProcessesPage {
        items,
        next_cursor,
        num_scanned_records,
        reached_scan_limit,
    }
}

pub(crate) async fn resume_picker_suite() {
    head_to_row_uses_first_user_message();
    rows_from_items_preserves_backend_order();
    row_uses_tail_timestamp_for_updated_at();
    row_display_preview_prefers_process_name();
    #[cfg(feature = "vt100-tests")]
    {
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_path("../snapshots");
        let _snapshot_settings = settings.bind_to_scope();
        resume_table_snapshot();
        resume_search_error_snapshot();
    }
    pageless_scrolling_deduplicates_and_keeps_order();
    ensure_minimum_rows_prefetches_when_underfilled();
    column_visibility_hides_extra_date_column_when_narrow();
    toggle_sort_key_reloads_with_new_sort().await;
    page_navigation_uses_view_rows().await;
    enter_on_row_selects_process_id().await;
    up_at_bottom_does_not_scroll_when_visible().await;
    set_query_loads_until_match_and_respects_scan_cap().await;
}

#[tokio::test]
async fn picker_regressions() {
    resume_picker_suite().await;
}

fn head_to_row_uses_first_user_message() {
    let item = ProcessItem {
        process_id: Some(ProcessId::new()),
        first_user_message: Some("real question".to_string()),
        created_at: Some("2025-01-01T00:00:00Z".into()),
        updated_at: Some("2025-01-01T00:00:00Z".into()),
        ..Default::default()
    };
    let row = head_to_row(&item);
    assert_eq!(row.preview, "real question");
}

fn rows_from_items_preserves_backend_order() {
    let a = ProcessItem {
        process_id: Some(ProcessId::new()),
        first_user_message: Some("A".to_string()),
        created_at: Some("2025-01-01T00:00:00Z".into()),
        updated_at: Some("2025-01-01T00:00:00Z".into()),
        ..Default::default()
    };
    let b = ProcessItem {
        process_id: Some(ProcessId::new()),
        first_user_message: Some("B".to_string()),
        created_at: Some("2025-01-02T00:00:00Z".into()),
        updated_at: Some("2025-01-02T00:00:00Z".into()),
        ..Default::default()
    };
    let rows = rows_from_items(vec![a, b]);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].preview.contains('A'));
    assert!(rows[1].preview.contains('B'));
}

fn row_uses_tail_timestamp_for_updated_at() {
    let item = ProcessItem {
        process_id: Some(ProcessId::new()),
        first_user_message: Some("Hello".to_string()),
        model_provider: Some("xai".to_string()),
        tokens_used: Some(123456),
        created_at: Some("2025-01-01T00:00:00Z".into()),
        updated_at: Some("2025-01-01T01:00:00Z".into()),
        ..Default::default()
    };

    let row = head_to_row(&item);
    let expected_created = "2025-01-01T00:00:00Z".parse::<Timestamp>().unwrap();
    let expected_updated = "2025-01-01T01:00:00Z".parse::<Timestamp>().unwrap();

    assert_eq!(row.created_at, Some(expected_created));
    assert_eq!(row.updated_at, Some(expected_updated));
    assert_eq!(row.model_provider.as_deref(), Some("xai"));
    assert_eq!(row.tokens_used, Some(123456));

    let mut state = PickerState::new(
        FrameRequester::test_dummy(),
        Arc::new(|_: PageLoadRequest| {}),
        "openai".to_string(),
        false,
        None,
        SessionPickerAction::Resume,
    );
    state.filtered_rows = vec![row];
    let [details, notice] = super::rendering::selected_details(&state);
    assert!(details.to_string().contains("Saved provider: xai"));
    assert!(details.to_string().contains("Total tokens: 123456"));
    assert!(
        notice
            .to_string()
            .contains("Opening this switches back to xai")
    );
    assert!(
        notice
            .to_string()
            .contains("Press Tab to keep the current model")
    );
    state.default_provider = "xai".to_string();
    let [_, notice] = super::rendering::selected_details(&state);
    assert!(!notice.to_string().contains("Active provider:"));
    state.filtered_rows[0].tokens_used = None;
    state.filtered_rows[0].model_provider = None;
    let [details, _] = super::rendering::selected_details(&state);
    assert!(details.to_string().contains("Saved provider: unknown"));
    assert!(details.to_string().contains("Total tokens: unknown"));
    state.filtered_rows.clear();
    assert_eq!(
        super::rendering::selected_details(&state),
        [Line::default(), Line::default()]
    );
}

fn row_display_preview_prefers_process_name() {
    let row = Row {
        preview: String::from("first message"),
        process_id: ProcessId::new(),
        process_name: Some(String::from("My session")),
        created_at: None,
        updated_at: None,
        cwd: None,
        git_branch: None,
        model_provider: None,
        tokens_used: None,
    };

    assert_eq!(row.display_preview(), "My session");
}

#[cfg(feature = "vt100-tests")]
fn resume_table_snapshot() {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;
    use ratatui::layout::Constraint;
    use ratatui::layout::Layout;

    let loader = Arc::new(|_: PageLoadRequest| {});
    let mut state = test_picker_state(loader);

    let now = Timestamp::now();
    let rows = vec![
        Row {
            preview: String::from("Fix resume picker timestamps"),
            process_id: ProcessId::new(),
            process_name: None,
            created_at: Some(now.checked_sub(16_i64.minutes()).unwrap()),
            updated_at: Some(now.checked_sub(42_i64.seconds()).unwrap()),
            cwd: None,
            git_branch: None,
            model_provider: None,
            tokens_used: None,
        },
        Row {
            preview: String::from("Investigate lazy pagination cap"),
            process_id: ProcessId::new(),
            process_name: None,
            created_at: Some(now.checked_sub(1_i64.hours()).unwrap()),
            updated_at: Some(now.checked_sub(35_i64.minutes()).unwrap()),
            cwd: None,
            git_branch: None,
            model_provider: None,
            tokens_used: None,
        },
        Row {
            preview: String::from("Explain the codebase"),
            process_id: ProcessId::new(),
            process_name: None,
            created_at: Some(now.checked_sub(2_i64.hours()).unwrap()),
            updated_at: Some(now.checked_sub(2_i64.hours()).unwrap()),
            cwd: None,
            git_branch: None,
            model_provider: None,
            tokens_used: None,
        },
    ];
    state.all_rows = rows.clone();
    state.filtered_rows = rows;
    state.view_rows = Some(3);
    state.selected = 1;
    state.scroll_top = 0;
    state.update_view_rows(3);

    let metrics = calculate_column_metrics(&state.filtered_rows, state.show_all);

    let width: u16 = 80;
    let height: u16 = 6;
    let backend = VT100Backend::new(width, height);
    let mut terminal = Terminal::with_options(backend).expect("terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));

    {
        let mut frame = terminal.get_frame();
        let area = frame.area();
        let segments = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
        render_column_headers(&mut frame, segments[0], &metrics, state.sort_key);
        render_list(&mut frame, segments[1], &state, &metrics);
    }
    terminal.flush().expect("flush");

    let snapshot = terminal.backend().to_string();
    assert_snapshot!("resume_picker_table", snapshot);
}

#[cfg(feature = "vt100-tests")]
fn resume_search_error_snapshot() {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;

    let loader = Arc::new(|_: PageLoadRequest| {});
    let mut state = test_picker_state(loader);
    state.inline_error = Some(String::from(
        "Failed to read session metadata from missing fixture",
    ));

    let width: u16 = 80;
    let height: u16 = 1;
    let backend = VT100Backend::new(width, height);
    let mut terminal = Terminal::with_options(backend).expect("terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));

    {
        let mut frame = terminal.get_frame();
        let line = search_line(&state);
        frame.render_widget(line, frame.area());
    }
    terminal.flush().expect("flush");

    let snapshot = terminal.backend().to_string();
    assert_snapshot!("resume_picker_search_error", snapshot);
}

fn pageless_scrolling_deduplicates_and_keeps_order() {
    let loader = Arc::new(|_: PageLoadRequest| {});
    let mut state = test_picker_state(loader);

    state.reset_pagination();
    let duplicate_process_id = ProcessId::new();
    state.ingest_page(page(
        vec![
            make_item_with_id(duplicate_process_id, "2025-01-03T00:00:00Z", "third"),
            make_item("2025-01-02T00:00:00Z", "second"),
        ],
        Some(cursor_from_str(
            "2025-01-02T00-00-00|00000000-0000-0000-0000-000000000000",
        )),
        2,
        false,
    ));

    state.ingest_page(page(
        vec![
            make_item_with_id(duplicate_process_id, "2025-01-03T00:00:00Z", "duplicate"),
            make_item("2025-01-01T00:00:00Z", "first"),
        ],
        Some(cursor_from_str(
            "2025-01-01T00-00-00|00000000-0000-0000-0000-000000000001",
        )),
        2,
        false,
    ));

    state.ingest_page(page(
        vec![make_item("2024-12-31T23:00:00Z", "very old")],
        None,
        1,
        false,
    ));

    let previews: Vec<_> = state
        .filtered_rows
        .iter()
        .map(|row| row.preview.as_str())
        .collect();
    assert_eq!(previews, vec!["third", "second", "first", "very old"]);

    let unique_process_ids = state
        .filtered_rows
        .iter()
        .map(|row| row.process_id)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(unique_process_ids.len(), 4);
}

fn ensure_minimum_rows_prefetches_when_underfilled() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = Arc::new(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });

    let mut state = test_picker_state(loader);
    state.reset_pagination();
    state.ingest_page(page(
        vec![
            make_item("2025-01-01T00:00:00Z", "one"),
            make_item("2025-01-02T00:00:00Z", "two"),
        ],
        Some(cursor_from_str(
            "2025-01-03T00-00-00|00000000-0000-0000-0000-000000000000",
        )),
        2,
        false,
    ));

    assert!(recorded_requests.lock().unwrap().is_empty());
    state.ensure_minimum_rows_for_view(10);
    let guard = recorded_requests.lock().unwrap();
    assert_eq!(guard.len(), 1);
    assert!(guard[0].search_token.is_none());
}

fn column_visibility_hides_extra_date_column_when_narrow() {
    let metrics = rendering::ColumnMetrics {
        max_created_width: 8,
        max_updated_width: 12,
        max_branch_width: 0,
        max_cwd_width: 0,
        labels: Vec::new(),
    };

    let created = column_visibility(30, &metrics, ProcessSortKey::CreatedAt);
    assert_eq!(
        created,
        ColumnVisibility {
            show_created: true,
            show_updated: false,
            show_branch: false,
            show_cwd: false,
        }
    );

    let updated = column_visibility(30, &metrics, ProcessSortKey::UpdatedAt);
    assert_eq!(
        updated,
        ColumnVisibility {
            show_created: false,
            show_updated: true,
            show_branch: false,
            show_cwd: false,
        }
    );

    let wide = column_visibility(40, &metrics, ProcessSortKey::CreatedAt);
    assert_eq!(
        wide,
        ColumnVisibility {
            show_created: true,
            show_updated: true,
            show_branch: false,
            show_cwd: false,
        }
    );
}

async fn toggle_sort_key_reloads_with_new_sort() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = Arc::new(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });

    let mut state = test_picker_state(loader);

    state.start_initial_load();
    {
        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard[0].sort_key, ProcessSortKey::UpdatedAt);
    }

    state
        .handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))
        .await
        .unwrap();

    let guard = recorded_requests.lock().unwrap();
    assert_eq!(guard.len(), 2);
    assert_eq!(guard[1].sort_key, ProcessSortKey::CreatedAt);
}

async fn page_navigation_uses_view_rows() {
    let loader = Arc::new(|_: PageLoadRequest| {});
    let mut state = test_picker_state(loader);

    let mut items = Vec::new();
    for idx in 0..20 {
        let ts = format!("2025-01-{:02}T00:00:00Z", idx + 1);
        let preview = format!("item-{idx}");
        items.push(make_item(&ts, &preview));
    }

    state.reset_pagination();
    state.ingest_page(page(items, None, 20, false));
    state.update_view_rows(5);

    assert_eq!(state.selected, 0);
    state
        .handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(state.selected, 5);

    state
        .handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(state.selected, 10);

    state
        .handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(state.selected, 5);
}

async fn enter_on_row_selects_process_id() {
    let loader = Arc::new(|_: PageLoadRequest| {});
    let mut state = test_picker_state(loader);

    let process_id = ProcessId::new();
    let row = Row {
        preview: String::from("missing metadata"),
        process_id,
        process_name: None,
        created_at: None,
        updated_at: None,
        cwd: None,
        git_branch: None,
        model_provider: None,
        tokens_used: None,
    };
    state.all_rows = vec![row.clone()];
    state.filtered_rows = vec![row];

    let selection = state
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .expect("enter should not abort the picker");

    assert!(matches!(
        selection,
        Some(SessionSelection::Resume(SessionTarget { process_id: selected, keep_current: false }))
            if selected == process_id
    ));
    assert_eq!(state.inline_error, None);
}

#[tokio::test]
async fn provider_switch_defaults_and_tab_override() {
    for action in [SessionPickerAction::Resume, SessionPickerAction::Fork] {
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            Arc::new(|_: PageLoadRequest| {}),
            "openai".to_string(),
            true,
            None,
            action,
        );
        state.filtered_rows = rows_from_items(vec![
            ProcessItem {
                process_id: Some(ProcessId::new()),
                model_provider: Some("xai".into()),
                ..Default::default()
            },
            ProcessItem {
                process_id: Some(ProcessId::new()),
                model_provider: Some("xai".into()),
                ..Default::default()
            },
        ]);
        for expected in [false, true, false] {
            let selection = state
                .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                .await
                .unwrap()
                .unwrap();
            let target = match selection {
                SessionSelection::Resume(target) | SessionSelection::Fork(target) => target,
                _ => panic!("expected session target"),
            };
            assert_eq!(target.keep_current, expected);
            state
                .handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
                .await
                .unwrap();
            assert_eq!(state.sort_key, ProcessSortKey::UpdatedAt);
        }
        assert!(
            super::rendering::selected_details(&state)[1]
                .to_string()
                .contains("Keeping openai")
        );
        state
            .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(
            super::rendering::selected_details(&state)[1]
                .to_string()
                .contains("Opening this switches back to xai")
        );
        state.filtered_rows[1].model_provider = Some("openai".into());
        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            selection,
            SessionSelection::Resume(SessionTarget {
                keep_current: false,
                ..
            }) | SessionSelection::Fork(SessionTarget {
                keep_current: false,
                ..
            })
        ));
        state
            .handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            selection,
            SessionSelection::Resume(SessionTarget {
                keep_current: true,
                ..
            }) | SessionSelection::Fork(SessionTarget {
                keep_current: true,
                ..
            })
        ));
        let id = state.filtered_rows[1].process_id;
        state
            .handle_background_event(BackgroundEvent::SelectionLoaded {
                process_id: id,
                selection: Ok(Some(chaos_kern::saved_selection::SavedSelection {
                    provider: "last-used-provider".into(),
                    model: "last-used-model".into(),
                    effort: None,
                })),
            })
            .await
            .unwrap();
        let details = super::rendering::selected_details(&state)[0].to_string();
        assert!(details.contains("Saved provider: last-used-provider"));
        assert!(details.contains("Model: last-used-model"));
    }
}

async fn up_at_bottom_does_not_scroll_when_visible() {
    let loader = Arc::new(|_: PageLoadRequest| {});
    let mut state = test_picker_state(loader);

    let mut items = Vec::new();
    for idx in 0..10 {
        let ts = format!("2025-02-{:02}T00:00:00Z", idx + 1);
        let preview = format!("item-{idx}");
        items.push(make_item(&ts, &preview));
    }

    state.reset_pagination();
    state.ingest_page(page(items, None, 10, false));
    state.update_view_rows(5);

    state.selected = state.filtered_rows.len().saturating_sub(1);
    state.ensure_selected_visible();

    let initial_top = state.scroll_top;
    assert_eq!(initial_top, state.filtered_rows.len().saturating_sub(5));

    state
        .handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(state.scroll_top, initial_top);
    assert_eq!(state.selected, state.filtered_rows.len().saturating_sub(2));
}

async fn set_query_loads_until_match_and_respects_scan_cap() {
    let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let request_sink = recorded_requests.clone();
    let loader = Arc::new(move |req: PageLoadRequest| {
        request_sink.lock().unwrap().push(req);
    });

    let mut state = test_picker_state(loader);
    state.reset_pagination();
    state.ingest_page(page(
        vec![make_item("2025-01-01T00:00:00Z", "alpha")],
        Some(cursor_from_str(
            "2025-01-02T00-00-00|00000000-0000-0000-0000-000000000000",
        )),
        1,
        false,
    ));
    recorded_requests.lock().unwrap().clear();

    state.set_query("target".to_string());
    let first_request = {
        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 1);
        guard[0].clone()
    };

    state
        .handle_background_event(BackgroundEvent::PageLoaded {
            request_token: first_request.request_token,
            search_token: first_request.search_token,
            page: Ok(page(
                vec![make_item("2025-01-02T00:00:00Z", "beta")],
                Some(cursor_from_str(
                    "2025-01-03T00-00-00|00000000-0000-0000-0000-000000000001",
                )),
                5,
                false,
            )),
        })
        .await
        .unwrap();

    let second_request = {
        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 2);
        guard[1].clone()
    };
    assert!(state.search_state.is_active());
    assert!(state.filtered_rows.is_empty());

    state
        .handle_background_event(BackgroundEvent::PageLoaded {
            request_token: second_request.request_token,
            search_token: second_request.search_token,
            page: Ok(page(
                vec![make_item("2025-01-03T00:00:00Z", "target log")],
                Some(cursor_from_str(
                    "2025-01-04T00-00-00|00000000-0000-0000-0000-000000000002",
                )),
                7,
                false,
            )),
        })
        .await
        .unwrap();

    assert!(!state.filtered_rows.is_empty());
    assert!(!state.search_state.is_active());

    recorded_requests.lock().unwrap().clear();
    state.set_query("missing".to_string());
    let active_request = {
        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 1);
        guard[0].clone()
    };

    state
        .handle_background_event(BackgroundEvent::PageLoaded {
            request_token: second_request.request_token,
            search_token: second_request.search_token,
            page: Ok(page(Vec::new(), None, 0, false)),
        })
        .await
        .unwrap();
    assert_eq!(recorded_requests.lock().unwrap().len(), 1);

    state
        .handle_background_event(BackgroundEvent::PageLoaded {
            request_token: active_request.request_token,
            search_token: active_request.search_token,
            page: Ok(page(Vec::new(), None, 3, true)),
        })
        .await
        .unwrap();

    assert!(state.filtered_rows.is_empty());
    assert!(!state.search_state.is_active());
    assert!(state.pagination.reached_scan_limit);
}
