use super::*;

use chaos_ipc::product::OS_NAME;

use crate::app_event::AppEvent;
use crate::status_indicator_widget::STATUS_DETAILS_DEFAULT_MAX_LINES;
use crate::status_indicator_widget::StatusDetailsCapitalization;
use crate::test_render::buffer_to_first_char_string;
use crate::test_render::render_to_first_char_string;
use crate::test_support::make_app_event_sender;
use crate::test_support::make_app_event_sender_with_rx;
use chaos_ipc::protocol::Op;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
macro_rules! assert_snapshot {
    ($($arg:tt)*) => {
        insta::with_settings!({ snapshot_path => "../snapshots" }, {
            insta::assert_snapshot!($($arg)*);
        })
    };
}
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::cell::Cell;
use std::rc::Rc;

fn test_pane_params(tx: AppEventSender) -> BottomPaneParams {
    BottomPaneParams {
        app_event_tx: tx,
        frame_requester: FrameRequester::test_dummy(),
        has_input_focus: true,
        enhanced_keys_supported: false,
        placeholder_text: format!("Ask Agent of {OS_NAME} to do something."),
        disable_paste_burst: false,
        animations_enabled: true,
    }
}

fn make_test_pane() -> BottomPane {
    BottomPane::new(test_pane_params(make_app_event_sender()))
}

fn make_test_pane_with_rx() -> (BottomPane, tokio::sync::mpsc::UnboundedReceiver<AppEvent>) {
    let (tx, rx) = make_app_event_sender_with_rx();
    (BottomPane::new(test_pane_params(tx)), rx)
}

fn exec_request() -> ApprovalRequest {
    ApprovalRequest::Exec {
        process_id: chaos_ipc::ProcessId::new(),
        process_label: None,
        model_name: OS_NAME.to_string(),
        id: "1".to_string(),
        command: vec!["echo".into(), "ok".into()],
        reason: None,
        available_decisions: vec![
            chaos_ipc::protocol::ReviewDecision::Approved,
            chaos_ipc::protocol::ReviewDecision::Abort,
        ],
        network_approval_context: None,
        additional_permissions: None,
    }
}

// The composer suites dominate this crate's test time, so they are exposed
// separately for the runner to schedule rather than folded in below.
pub(crate) fn chat_composer_input_suite() {
    super::chat_composer::tests::chat_composer_input_suite();
}

pub(crate) fn chat_composer_slash_suite() {
    super::chat_composer::tests::chat_composer_slash_suite();
}

pub(crate) fn chat_composer_paste_suite() {
    super::chat_composer::tests::chat_composer_paste_suite();
}

pub(crate) fn chat_composer_prompt_suite() {
    super::chat_composer::tests::chat_composer_prompt_suite();
}

pub(crate) fn bottom_pane_suite() {
    super::app_link_view::tests::app_link_view_suite();
    super::approval_overlay::tests::approval_overlay_suite();
    super::chat_composer_history::tests::chat_composer_history_suite();
    super::command_popup::tests::command_popup_suite();
    super::footer::tests::footer_suite();
    super::list_selection_view::tests::list_selection_view_suite();
    super::mcp_add_form::tests::mcp_add_form_suite();
    super::mcp_server_elicitation::tests::mcp_server_elicitation_suite();
    super::paste_burst::tests::paste_burst_suite();
    super::pending_input_preview::tests::pending_input_preview_suite();
    super::pending_process_approvals::tests::pending_process_approvals_suite();
    super::prompt_args::tests::prompt_args_suite();
    super::request_user_input::tests::request_user_input_suite();
    super::scroll_state::tests::wrap_navigation_and_visibility();
    super::selection_popup_common::tests::one_cell_width_falls_back_without_panic_for_wrapped_two_column_rows();
    super::slash_commands::tests::slash_commands_suite();
    super::textarea::tests::textarea_suite();
    super::unified_exec_footer::tests::unified_exec_footer_suite();

    ctrl_c_on_modal_consumes_without_showing_quit_hint();
    overlay_not_shown_above_approval_modal();
    composer_shown_after_denied_while_task_running();
    status_indicator_visible_during_command_execution();
    status_and_composer_fill_height_without_bottom_padding();
    status_only_snapshot();
    unified_exec_summary_does_not_increase_height_when_status_visible();
    status_with_details_and_queued_messages_snapshot();
    queued_messages_visible_when_status_hidden_snapshot();
    status_and_queued_messages_snapshot();
    remote_images_render_above_composer_text();
    drain_pending_submission_state_clears_remote_image_urls();
    esc_with_slash_command_popup_does_not_interrupt_task();
    esc_with_agent_command_without_popup_does_not_interrupt_task();
    esc_repeat_and_release_after_dismissing_agent_picker_do_not_interrupt_task();
    esc_interrupts_running_task_when_no_popup();
    esc_routes_to_handle_key_event_when_requested();
    release_events_are_ignored_for_active_view();
}

fn ctrl_c_on_modal_consumes_without_showing_quit_hint() {
    let mut pane = make_test_pane();
    pane.push_approval_request(exec_request());
    assert_eq!(CancellationEvent::Handled, pane.on_ctrl_c());
    assert!(!pane.quit_shortcut_hint_visible());
    assert_eq!(CancellationEvent::NotHandled, pane.on_ctrl_c());
}

// live ring removed; related tests deleted.

fn overlay_not_shown_above_approval_modal() {
    let mut pane = make_test_pane();

    // Create an approval modal (active view).
    pane.push_approval_request(exec_request());

    // Render and verify the top row does not include an overlay.
    let area = Rect::new(0, 0, 60, 6);
    let mut buf = Buffer::empty(area);
    pane.render(area, &mut buf);

    let mut r0 = String::new();
    for x in 0..area.width {
        r0.push(buf[(x, 0)].symbol().chars().next().unwrap_or(' '));
    }
    assert!(
        !r0.contains("Working"),
        "overlay should not render above modal"
    );
}

fn composer_shown_after_denied_while_task_running() {
    let mut pane = make_test_pane();

    // Start a running task so the status indicator is active above the composer.
    pane.set_task_running(true);

    // Push an approval modal (e.g., command approval) which should hide the status view.
    pane.push_approval_request(exec_request());

    // Simulate pressing 'n' (No) on the modal.
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;
    pane.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

    // After denial, since the task is still running, the status indicator should be
    // visible above the composer. The modal should be gone.
    assert!(
        pane.view_stack.is_empty(),
        "no active modal view after denial"
    );

    // Render and ensure the top row includes the Working header and a composer line below.
    // Give the animation thread a moment to tick.
    std::thread::sleep(Duration::from_millis(120));
    let area = Rect::new(0, 0, 40, 6);
    let mut buf = Buffer::empty(area);
    pane.render(area, &mut buf);
    let mut row0 = String::new();
    for x in 0..area.width {
        row0.push(buf[(x, 0)].symbol().chars().next().unwrap_or(' '));
    }
    assert!(
        row0.contains("Working"),
        "expected Working header after denial on row 0: {row0:?}"
    );

    // Composer placeholder should be visible somewhere below.
    let mut found_composer = false;
    for y in 1..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            row.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
        }
        if row.contains("Ask Agent") {
            found_composer = true;
            break;
        }
    }
    assert!(
        found_composer,
        "expected composer visible under status line"
    );
}

fn status_indicator_visible_during_command_execution() {
    let mut pane = make_test_pane();

    // Begin a task: show initial status.
    pane.set_task_running(true);

    // Use a height that allows the status line to be visible above the composer.
    let area = Rect::new(0, 0, 40, 6);
    let mut buf = Buffer::empty(area);
    pane.render(area, &mut buf);

    let bufs = buffer_to_first_char_string(&buf);
    assert!(bufs.contains("• Working"), "expected Working header");
}

fn status_and_composer_fill_height_without_bottom_padding() {
    let mut pane = make_test_pane();

    // Activate spinner (status view replaces composer) with no live ring.
    pane.set_task_running(true);

    // Use height == desired_height; expect spacer + status + composer rows without trailing padding.
    let height = pane.desired_height(30);
    assert!(
        height >= 3,
        "expected at least 3 rows to render spacer, status, and composer; got {height}"
    );
    let area = Rect::new(0, 0, 30, height);
    assert_snapshot!(
        "status_and_composer_fill_height_without_bottom_padding",
        render_to_first_char_string(&pane, area)
    );

    // The padding adapter must measure, paint and position the composer cursor
    // using the same inner area, including panes smaller than their padding.
    use crate::render::renderable::{InsetRenderable, RenderableItem};
    use ratatui::widgets::Padding;
    pane.set_task_running(false);
    let padded = InsetRenderable::new(RenderableItem::Borrowed(&pane), Padding::new(2, 1, 1, 2));
    assert!(pane.cursor_pos(Rect::new(7, 8, 27, 7)).is_some());
    for (width, height) in [(30u16, 10u16), (3, 10), (2, 2), (0, 0)] {
        let area = Rect::new(5, 7, width, height);
        let inner = Rect::new(7, 8, width.saturating_sub(3), height.saturating_sub(3));
        assert_eq!(
            padded.desired_height(width),
            pane.desired_height(inner.width).saturating_add(3),
        );
        let mut actual = Buffer::empty(area);
        let mut expected = Buffer::empty(area);
        padded.render(area, &mut actual);
        let cursor = if inner.is_empty() {
            None
        } else {
            pane.render(inner, &mut expected);
            pane.cursor_pos(inner)
        };
        assert_eq!(actual, expected);
        assert_eq!(padded.cursor_pos(area), cursor);
    }
}

fn status_only_snapshot() {
    let mut pane = make_test_pane();

    pane.set_task_running(true);

    let width = 48;
    let height = pane.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    assert_snapshot!(
        "status_only_snapshot",
        render_to_first_char_string(&pane, area)
    );
}

fn unified_exec_summary_does_not_increase_height_when_status_visible() {
    let mut pane = make_test_pane();

    pane.set_task_running(true);
    let width = 120;
    let before = pane.desired_height(width);

    pane.set_unified_exec_processes(vec!["sleep 5".to_string()]);
    let after = pane.desired_height(width);

    assert_eq!(after, before);

    let area = Rect::new(0, 0, width, after);
    let rendered = render_to_first_char_string(&pane, area);
    assert!(rendered.contains("background terminal running · /ps to view"));
}

fn status_with_details_and_queued_messages_snapshot() {
    let mut pane = make_test_pane();

    pane.set_task_running(true);
    pane.update_status(
        "Working".to_string(),
        Some("First detail line\nSecond detail line".to_string()),
        StatusDetailsCapitalization::CapitalizeFirst,
        STATUS_DETAILS_DEFAULT_MAX_LINES,
    );
    pane.set_pending_input_preview(vec!["Queued follow-up question".to_string()], Vec::new());

    let width = 48;
    let height = pane.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    assert_snapshot!(
        "status_with_details_and_queued_messages_snapshot",
        render_to_first_char_string(&pane, area)
    );
}

fn queued_messages_visible_when_status_hidden_snapshot() {
    let mut pane = make_test_pane();

    pane.set_task_running(true);
    pane.set_pending_input_preview(vec!["Queued follow-up question".to_string()], Vec::new());
    pane.hide_status_indicator();

    let width = 48;
    let height = pane.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    assert_snapshot!(
        "queued_messages_visible_when_status_hidden_snapshot",
        render_to_first_char_string(&pane, area)
    );
}

fn status_and_queued_messages_snapshot() {
    let mut pane = make_test_pane();

    pane.set_task_running(true);
    pane.set_pending_input_preview(vec!["Queued follow-up question".to_string()], Vec::new());

    let width = 48;
    let height = pane.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    assert_snapshot!(
        "status_and_queued_messages_snapshot",
        render_to_first_char_string(&pane, area)
    );
}

fn remote_images_render_above_composer_text() {
    let mut pane = make_test_pane();

    pane.set_remote_image_urls(vec![
        "https://example.com/one.png".to_string(),
        "data:image/png;base64,aGVsbG8=".to_string(),
    ]);

    assert_eq!(pane.composer_text(), "");
    let width = 48;
    let height = pane.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let snapshot = render_to_first_char_string(&pane, area);
    assert!(snapshot.contains("[Image #1]"));
    assert!(snapshot.contains("[Image #2]"));
}

fn drain_pending_submission_state_clears_remote_image_urls() {
    let mut pane = make_test_pane();

    pane.set_remote_image_urls(vec!["https://example.com/one.png".to_string()]);
    assert_eq!(pane.remote_image_urls().len(), 1);

    pane.drain_pending_submission_state();

    assert!(pane.remote_image_urls().is_empty());
}

fn esc_with_slash_command_popup_does_not_interrupt_task() {
    let (mut pane, mut rx) = make_test_pane_with_rx();

    pane.set_task_running(true);

    // Repro: a running task + slash-command popup + Esc should not interrupt the task.
    pane.insert_str("/");
    assert!(
        pane.composer.popup_active(),
        "expected command popup after typing `/`"
    );

    pane.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    while let Ok(ev) = rx.try_recv() {
        assert!(
            !matches!(ev, AppEvent::ChaosOp(Op::Interrupt)),
            "expected Esc to not send Op::Interrupt while command popup is active"
        );
    }
    assert_eq!(pane.composer_text(), "/");
}

fn esc_with_agent_command_without_popup_does_not_interrupt_task() {
    let (mut pane, mut rx) = make_test_pane_with_rx();

    pane.set_task_running(true);

    // Repro: `/agent ` hides the popup (cursor past command name). Esc should
    // keep editing command text instead of interrupting the running task.
    pane.insert_str("/agent ");
    assert!(
        !pane.composer.popup_active(),
        "expected command popup to be hidden after entering `/agent `"
    );

    pane.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    while let Ok(ev) = rx.try_recv() {
        assert!(
            !matches!(ev, AppEvent::ChaosOp(Op::Interrupt)),
            "expected Esc to not send Op::Interrupt while typing `/agent`"
        );
    }
    assert_eq!(pane.composer_text(), "/agent ");
}

fn esc_repeat_and_release_after_dismissing_agent_picker_do_not_interrupt_task() {
    let (mut pane, mut rx) = make_test_pane_with_rx();

    pane.set_task_running(true);
    pane.show_selection_view(SelectionViewParams {
        title: Some("Agents".to_string()),
        items: vec![SelectionItem {
            name: "Main".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    });

    pane.handle_key_event(KeyEvent::new_with_kind(
        KeyCode::Esc,
        KeyModifiers::NONE,
        KeyEventKind::Press,
    ));
    for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
        pane.handle_key_event(KeyEvent::new_with_kind(
            KeyCode::Esc,
            KeyModifiers::NONE,
            kind,
        ));
    }

    while let Ok(ev) = rx.try_recv() {
        assert!(
            !matches!(ev, AppEvent::ChaosOp(Op::Interrupt)),
            "expected held Esc after dismissing agent picker to not interrupt"
        );
    }
    assert!(
        pane.no_modal_or_popup_active(),
        "expected Esc press to dismiss the agent picker"
    );
    pane.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(rx.try_recv(), Ok(AppEvent::ChaosOp(Op::Interrupt))),
        "a fresh Esc press should still interrupt"
    );
}

fn esc_interrupts_running_task_when_no_popup() {
    let (mut pane, mut rx) = make_test_pane_with_rx();

    pane.set_task_running(true);

    pane.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(
        matches!(rx.try_recv(), Ok(AppEvent::ChaosOp(Op::Interrupt))),
        "expected Esc to send Op::Interrupt while a task is running"
    );
}

fn esc_routes_to_handle_key_event_when_requested() {
    #[derive(Default)]
    struct EscRoutingView {
        on_ctrl_c_calls: Rc<Cell<usize>>,
        handle_calls: Rc<Cell<usize>>,
    }

    impl Renderable for EscRoutingView {
        fn render(&self, _area: Rect, _buf: &mut Buffer) {}

        fn desired_height(&self, _width: u16) -> u16 {
            0
        }
    }

    impl BottomPaneView for EscRoutingView {
        fn handle_key_event(&mut self, _key_event: KeyEvent) {
            self.handle_calls
                .set(self.handle_calls.get().saturating_add(1));
        }

        fn on_ctrl_c(&mut self) -> CancellationEvent {
            self.on_ctrl_c_calls
                .set(self.on_ctrl_c_calls.get().saturating_add(1));
            CancellationEvent::Handled
        }

        fn prefer_esc_to_handle_key_event(&self) -> bool {
            true
        }
    }

    let mut pane = make_test_pane();

    let on_ctrl_c_calls = Rc::new(Cell::new(0));
    let handle_calls = Rc::new(Cell::new(0));
    pane.push_view(Box::new(EscRoutingView {
        on_ctrl_c_calls: Rc::clone(&on_ctrl_c_calls),
        handle_calls: Rc::clone(&handle_calls),
    }));

    pane.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(on_ctrl_c_calls.get(), 0);
    assert_eq!(handle_calls.get(), 1);
}

fn release_events_are_ignored_for_active_view() {
    #[derive(Default)]
    struct CountingView {
        handle_calls: Rc<Cell<usize>>,
    }

    impl Renderable for CountingView {
        fn render(&self, _area: Rect, _buf: &mut Buffer) {}

        fn desired_height(&self, _width: u16) -> u16 {
            0
        }
    }

    impl BottomPaneView for CountingView {
        fn handle_key_event(&mut self, _key_event: KeyEvent) {
            self.handle_calls
                .set(self.handle_calls.get().saturating_add(1));
        }
    }

    let mut pane = make_test_pane();

    let handle_calls = Rc::new(Cell::new(0));
    pane.push_view(Box::new(CountingView {
        handle_calls: Rc::clone(&handle_calls),
    }));

    pane.handle_key_event(KeyEvent::new_with_kind(
        KeyCode::Down,
        KeyModifiers::NONE,
        KeyEventKind::Press,
    ));
    pane.handle_key_event(KeyEvent::new_with_kind(
        KeyCode::Down,
        KeyModifiers::NONE,
        KeyEventKind::Release,
    ));

    assert_eq!(handle_calls.get(), 1);
}
