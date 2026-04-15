//! The bottom pane is the interactive footer of the chat UI.
//!
//! The pane owns the [`ChatComposer`] (editable prompt input) and a stack of transient
//! [`BottomPaneView`]s (popups/modals) that temporarily replace the composer for focused
//! interactions like selection lists.
//!
//! Input routing is layered: `BottomPane` decides which local surface receives a key (view vs
//! composer), while higher-level intent such as "interrupt" or "quit" is decided by the parent
//! widget (`ChatWidget`). This split matters for Ctrl+C/Ctrl+D: the bottom pane gives the active
//! view the first chance to consume Ctrl+C (typically to dismiss itself), and `ChatWidget` may
//! treat an unhandled Ctrl+C as an interrupt or as the first press of a double-press quit
//! shortcut.
//!
//! Some UI is time-based rather than input-based, such as the transient "press again to quit"
//! hint. The pane schedules redraws so those hints can expire even when the UI is otherwise idle.
use std::path::PathBuf;

use crate::app_event::ConnectorsSnapshot;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::pending_input_preview::PendingInputPreview;
use crate::bottom_pane::pending_process_approvals::PendingProcessApprovals;
use crate::bottom_pane::unified_exec_footer::UnifiedExecFooter;
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::render::renderable::FlexRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableItem;
use crate::tui::FrameRequester;
use bottom_pane_view::BottomPaneView;
use chaos_ipc::request_user_input::RequestUserInputEvent;
use chaos_ipc::user_input::TextElement;
use chaos_kern::features::Features;
use chaos_locate::FileMatch;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use std::time::Duration;

mod app_link_view;
mod approval_overlay;
mod mcp_add_form;
mod mcp_server_elicitation;
mod multi_select_picker;
mod request_user_input;
mod status_line_setup;
pub use app_link_view::AppLinkElicitationTarget;
pub use app_link_view::AppLinkSuggestionType;
pub use app_link_view::AppLinkView;
pub use app_link_view::AppLinkViewParams;
pub use approval_overlay::ApprovalOverlay;
pub use approval_overlay::ApprovalRequest;
pub use approval_overlay::format_requested_permissions_rule;
pub use mcp_add_form::McpAddForm;
pub use mcp_server_elicitation::McpServerElicitationFormRequest;
pub use mcp_server_elicitation::McpServerElicitationOverlay;
pub use request_user_input::RequestUserInputOverlay;
mod bottom_pane_view;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalImageAttachment {
    pub placeholder: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionBinding {
    /// Mention token text without the leading `$`.
    pub mention: String,
    /// Canonical mention target (for example `app://...` or absolute SKILL.md path).
    pub path: String,
}
mod chat_composer;
mod chat_composer_history;
mod command_popup;
pub mod custom_prompt_view;
mod file_search_popup;
mod footer;
mod list_selection_view;
mod prompt_args;
mod slash_commands;
pub use footer::CollaborationModeIndicator;
pub use list_selection_view::ColumnWidthMode;
pub use list_selection_view::SelectionViewParams;
pub use list_selection_view::SideContentWidth;
pub use list_selection_view::popup_content_width;
pub use list_selection_view::side_by_side_layout_widths;
pub use status_line_setup::StatusLineItem;
pub use status_line_setup::StatusLinePreviewData;
pub use status_line_setup::StatusLineSetupView;
mod paste_burst;
mod pending_input_preview;
mod pending_process_approvals;
pub mod popup_consts;
mod scroll_state;
mod selection_popup_common;
mod textarea;
mod unified_exec_footer;

/// How long the "press again to quit" hint stays visible.
///
/// This is shared between:
/// - `ChatWidget`: arming the double-press quit shortcut.
/// - `BottomPane`/`ChatComposer`: rendering and expiring the footer hint.
///
/// Keeping a single value ensures Ctrl+C and Ctrl+D behave identically.
pub const QUIT_SHORTCUT_TIMEOUT: Duration = Duration::from_secs(1);

/// Whether Ctrl+C/Ctrl+D require a second press to quit.
///
/// This UX experiment was enabled by default, but requiring a double press to quit feels janky in
/// practice (especially for users accustomed to shells and other TUIs). Disable it for now while we
/// rethink a better quit/interrupt design.
pub const DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED: bool = false;

/// The result of offering a cancellation key to a bottom-pane surface.
///
/// This is primarily used for Ctrl+C routing: active views can consume the key to dismiss
/// themselves, and the caller can decide what higher-level action (if any) to take when the key is
/// not handled locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancellationEvent {
    Handled,
    NotHandled,
}

use crate::bottom_pane::prompt_args::parse_slash_name;
use chaos_ipc::custom_prompts::CustomPrompt;
pub use chat_composer::ChatComposer;
pub use chat_composer::ChatComposerConfig;
pub use chat_composer::InputResult;

use crate::status_indicator_widget::StatusDetailsCapitalization;
use crate::status_indicator_widget::StatusIndicatorWidget;
pub use list_selection_view::SelectionAction;
pub use list_selection_view::SelectionItem;

/// Pane displayed in the lower half of the chat UI.
///
/// This is the owning container for the prompt input (`ChatComposer`) and the view stack
/// (`BottomPaneView`). It performs local input routing and renders time-based hints, while leaving
/// process-level decisions (quit, interrupt, shutdown) to `ChatWidget`.
pub struct BottomPane {
    /// Composer is retained even when a BottomPaneView is displayed so the
    /// input state is retained when the view is closed.
    composer: ChatComposer,

    /// Stack of views displayed instead of the composer (e.g. popups/modals).
    view_stack: Vec<Box<dyn BottomPaneView>>,

    app_event_tx: AppEventSender,
    frame_requester: FrameRequester,

    has_input_focus: bool,
    enhanced_keys_supported: bool,
    disable_paste_burst: bool,
    is_task_running: bool,
    esc_backtrack_hint: bool,
    animations_enabled: bool,

    /// Inline status indicator shown above the composer while a task is running.
    status: Option<StatusIndicatorWidget>,
    /// Unified exec session summary source.
    ///
    /// When a status row exists, this summary is mirrored inline in that row;
    /// when no status row exists, it renders as its own footer row.
    unified_exec_footer: UnifiedExecFooter,
    /// Preview of pending steers and queued drafts shown above the composer.
    pending_input_preview: PendingInputPreview,
    /// Inactive threads with pending approval requests.
    pending_process_approvals: PendingProcessApprovals,
    context_window_percent: Option<i64>,
    context_window_used_tokens: Option<i64>,
}

pub struct BottomPaneParams {
    pub app_event_tx: AppEventSender,
    pub frame_requester: FrameRequester,
    pub has_input_focus: bool,
    pub enhanced_keys_supported: bool,
    pub placeholder_text: String,
    pub disable_paste_burst: bool,
    pub animations_enabled: bool,
}

mod state;

#[cfg(test)]
mod tests {
    use super::*;

    const PLACEHOLDER_TEXT: &str = "Ask Agent of Chaos to do something.";
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
    use insta::assert_snapshot;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use std::cell::Cell;
    use std::rc::Rc;

    fn exec_request() -> ApprovalRequest {
        ApprovalRequest::Exec {
            process_id: chaos_ipc::ProcessId::new(),
            process_label: None,
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

    #[test]
    fn ctrl_c_on_modal_consumes_without_showing_quit_hint() {
        let tx = make_app_event_sender();
        let features = Features::with_defaults();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });
        pane.push_approval_request(exec_request(), &features);
        assert_eq!(CancellationEvent::Handled, pane.on_ctrl_c());
        assert!(!pane.quit_shortcut_hint_visible());
        assert_eq!(CancellationEvent::NotHandled, pane.on_ctrl_c());
    }

    // live ring removed; related tests deleted.

    #[test]
    fn overlay_not_shown_above_approval_modal() {
        let tx = make_app_event_sender();
        let features = Features::with_defaults();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

        // Create an approval modal (active view).
        pane.push_approval_request(exec_request(), &features);

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

    #[test]
    fn composer_shown_after_denied_while_task_running() {
        let tx = make_app_event_sender();
        let features = Features::with_defaults();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

        // Start a running task so the status indicator is active above the composer.
        pane.set_task_running(true);

        // Push an approval modal (e.g., command approval) which should hide the status view.
        pane.push_approval_request(exec_request(), &features);

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

    #[test]
    fn status_indicator_visible_during_command_execution() {
        let tx = make_app_event_sender();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

        // Begin a task: show initial status.
        pane.set_task_running(true);

        // Use a height that allows the status line to be visible above the composer.
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        pane.render(area, &mut buf);

        let bufs = buffer_to_first_char_string(&buf);
        assert!(bufs.contains("• Working"), "expected Working header");
    }

    #[test]
    fn status_and_composer_fill_height_without_bottom_padding() {
        let tx = make_app_event_sender();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

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
    }

    #[test]
    fn status_only_snapshot() {
        let tx = make_app_event_sender();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

        pane.set_task_running(true);

        let width = 48;
        let height = pane.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        assert_snapshot!(
            "status_only_snapshot",
            render_to_first_char_string(&pane, area)
        );
    }

    #[test]
    fn unified_exec_summary_does_not_increase_height_when_status_visible() {
        let tx = make_app_event_sender();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

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

    #[test]
    fn status_with_details_and_queued_messages_snapshot() {
        let tx = make_app_event_sender();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

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

    #[test]
    fn queued_messages_visible_when_status_hidden_snapshot() {
        let tx = make_app_event_sender();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

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

    #[test]
    fn status_and_queued_messages_snapshot() {
        let tx = make_app_event_sender();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

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

    #[test]
    fn remote_images_render_above_composer_text() {
        let tx = make_app_event_sender();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

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

    #[test]
    fn drain_pending_submission_state_clears_remote_image_urls() {
        let tx = make_app_event_sender();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

        pane.set_remote_image_urls(vec!["https://example.com/one.png".to_string()]);
        assert_eq!(pane.remote_image_urls().len(), 1);

        pane.drain_pending_submission_state();

        assert!(pane.remote_image_urls().is_empty());
    }

    #[test]
    fn esc_with_slash_command_popup_does_not_interrupt_task() {
        let (tx, mut rx) = make_app_event_sender_with_rx();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

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

    #[test]
    fn esc_with_agent_command_without_popup_does_not_interrupt_task() {
        let (tx, mut rx) = make_app_event_sender_with_rx();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

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

    #[test]
    fn esc_release_after_dismissing_agent_picker_does_not_interrupt_task() {
        let (tx, mut rx) = make_app_event_sender_with_rx();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

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
        pane.handle_key_event(KeyEvent::new_with_kind(
            KeyCode::Esc,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));

        while let Ok(ev) = rx.try_recv() {
            assert!(
                !matches!(ev, AppEvent::ChaosOp(Op::Interrupt)),
                "expected Esc release after dismissing agent picker to not interrupt"
            );
        }
        assert!(
            pane.no_modal_or_popup_active(),
            "expected Esc press to dismiss the agent picker"
        );
    }

    #[test]
    fn esc_interrupts_running_task_when_no_popup() {
        let (tx, mut rx) = make_app_event_sender_with_rx();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

        pane.set_task_running(true);

        pane.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(
            matches!(rx.try_recv(), Ok(AppEvent::ChaosOp(Op::Interrupt))),
            "expected Esc to send Op::Interrupt while a task is running"
        );
    }

    #[test]
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

        let tx = make_app_event_sender();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

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

    #[test]
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

        let tx = make_app_event_sender();
        let mut pane = BottomPane::new(BottomPaneParams {
            app_event_tx: tx,
            frame_requester: FrameRequester::test_dummy(),
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: PLACEHOLDER_TEXT.to_string(),
            disable_paste_burst: false,
            animations_enabled: true,
        });

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
}
