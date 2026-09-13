use super::*;
use crate::app_event::AppEvent;
use crate::render::renderable::Renderable;
use crate::test_support::make_app_event_sender_with_rx;
use crate::test_support::renderable_first_char_string;
use crate::test_support::renderable_trim_end_string_at_desired_height;
macro_rules! assert_snapshot {
    ($($arg:tt)*) => {
        insta::with_settings!({ snapshot_path => "../snapshots" }, {
            insta::assert_snapshot!($($arg)*);
        })
    };
}

fn suggestion_target() -> AppLinkElicitationTarget {
    AppLinkElicitationTarget {
        process_id: ProcessId::try_from("00000000-0000-0000-0000-000000000001")
            .expect("valid thread id"),
        server_name: "codex_apps".to_string(),
        request_id: McpRequestId::String("request-1".to_string()),
    }
}

pub(crate) fn app_link_view_suite() {
    installed_app_toggle_action_updates_labels();
    install_confirmation_keeps_url_like_tokens_and_long_url_tail_visible();
    tool_suggestions_resolve_install_decline_and_enable_paths();
    suggestion_with_reason_snapshots();
}

fn installed_app_toggle_action_updates_labels() {
    let (tx, _rx) = make_app_event_sender_with_rx();
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_1".to_string(),
            title: "Notion".to_string(),
            description: None,
            instructions: "Manage app".to_string(),
            url: "https://example.test/notion".to_string(),
            is_installed: true,
            is_enabled: true,
            suggest_reason: None,
            suggestion_type: None,
            elicitation_target: None,
        },
        tx,
    );
    assert_eq!(
        view.action_labels(),
        vec!["Manage on ChatGPT", "Disable app", "Back"]
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));

    assert_eq!(
        view.action_labels(),
        vec!["Manage on ChatGPT", "Enable app", "Back"]
    );
}

fn install_confirmation_keeps_url_like_tokens_and_long_url_tail_visible() {
    let tx = crate::test_support::make_app_event_sender();
    let url_like = "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890";
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_1".to_string(),
            title: "Notion".to_string(),
            description: None,
            instructions: "Manage app".to_string(),
            url: url_like.to_string(),
            is_installed: true,
            is_enabled: true,
            suggest_reason: None,
            suggestion_type: None,
            elicitation_target: None,
        },
        tx,
    );
    view.screen = AppLinkScreen::InstallConfirmation;

    let rendered: Vec<String> = view
        .content_lines(40)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect();

    assert_eq!(
        rendered
            .iter()
            .filter(|line| line.contains(url_like))
            .count(),
        1,
        "expected full URL-like token in one rendered line, got: {rendered:?}"
    );

    let tx = crate::test_support::make_app_event_sender();
    let url = "https://example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/artifacts/reports/performance/summary/detail/with/a/very/long/path/tail42";
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_1".to_string(),
            title: "Notion".to_string(),
            description: None,
            instructions: "Manage app".to_string(),
            url: url.to_string(),
            is_installed: true,
            is_enabled: true,
            suggest_reason: None,
            suggestion_type: None,
            elicitation_target: None,
        },
        tx,
    );
    view.screen = AppLinkScreen::InstallConfirmation;

    let width: u16 = 36;
    let area = Rect::new(0, 0, width, view.desired_height(width));
    let rendered_blob = renderable_first_char_string(&view, area);

    assert!(
        rendered_blob.contains("tail42"),
        "expected wrapped setup URL tail to remain visible in narrow pane, got:\n{rendered_blob}"
    );
}

fn tool_suggestions_resolve_install_decline_and_enable_paths() {
    let (tx, mut rx) = make_app_event_sender_with_rx();
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: Some("Plan events and schedules.".to_string()),
            instructions: "Install this app in your browser, then return here.".to_string(),
            url: "https://example.test/google-calendar".to_string(),
            is_installed: false,
            is_enabled: false,
            suggest_reason: Some("Plan and reference events from your calendar".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Install),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match rx.try_recv() {
        Ok(AppEvent::OpenUrlInBrowser { url }) => {
            assert_eq!(url, "https://example.test/google-calendar".to_string());
        }
        Ok(other) => panic!("unexpected app event: {other:?}"),
        Err(err) => panic!("missing app event: {err}"),
    }
    assert_eq!(view.screen, AppLinkScreen::InstallConfirmation);

    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match rx.try_recv() {
        Ok(AppEvent::SubmitProcessOp { process_id, op }) => {
            assert_eq!(process_id, suggestion_target().process_id);
            assert_eq!(
                op,
                Op::ResolveElicitation {
                    server_name: "codex_apps".to_string(),
                    request_id: McpRequestId::String("request-1".to_string()),
                    decision: ElicitationAction::Accept,
                    content: None,
                    meta: None,
                }
            );
        }
        Ok(other) => panic!("unexpected app event: {other:?}"),
        Err(err) => panic!("missing app event: {err}"),
    }
    assert!(view.is_complete());

    let (tx, mut rx) = make_app_event_sender_with_rx();
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: None,
            instructions: "Install this app in your browser, then return here.".to_string(),
            url: "https://example.test/google-calendar".to_string(),
            is_installed: false,
            is_enabled: false,
            suggest_reason: Some("Plan and reference events from your calendar".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Install),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));

    match rx.try_recv() {
        Ok(AppEvent::SubmitProcessOp { process_id, op }) => {
            assert_eq!(process_id, suggestion_target().process_id);
            assert_eq!(
                op,
                Op::ResolveElicitation {
                    server_name: "codex_apps".to_string(),
                    request_id: McpRequestId::String("request-1".to_string()),
                    decision: ElicitationAction::Decline,
                    content: None,
                    meta: None,
                }
            );
        }
        Ok(other) => panic!("unexpected app event: {other:?}"),
        Err(err) => panic!("missing app event: {err}"),
    }
    assert!(view.is_complete());

    let (tx, mut rx) = make_app_event_sender_with_rx();
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: Some("Plan events and schedules.".to_string()),
            instructions: "Enable this app to use it for the current request.".to_string(),
            url: "https://example.test/google-calendar".to_string(),
            is_installed: true,
            is_enabled: false,
            suggest_reason: Some("Plan and reference events from your calendar".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Enable),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));

    match rx.try_recv() {
        Ok(AppEvent::SubmitProcessOp { process_id, op }) => {
            assert_eq!(process_id, suggestion_target().process_id);
            assert_eq!(
                op,
                Op::ResolveElicitation {
                    server_name: "codex_apps".to_string(),
                    request_id: McpRequestId::String("request-1".to_string()),
                    decision: ElicitationAction::Accept,
                    content: None,
                    meta: None,
                }
            );
        }
        Ok(other) => panic!("unexpected app event: {other:?}"),
        Err(err) => panic!("missing app event: {err}"),
    }
    assert!(view.is_complete());
}

fn suggestion_with_reason_snapshots() {
    let tx = crate::test_support::make_app_event_sender();
    let view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: Some("Plan events and schedules.".to_string()),
            instructions: "Install this app in your browser, then return here.".to_string(),
            url: "https://example.test/google-calendar".to_string(),
            is_installed: false,
            is_enabled: false,
            suggest_reason: Some("Plan and reference events from your calendar".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Install),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    assert_snapshot!(
        "app_link_view_install_suggestion_with_reason",
        renderable_trim_end_string_at_desired_height(&view, 72)
    );

    let tx = crate::test_support::make_app_event_sender();
    let view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: Some("Plan events and schedules.".to_string()),
            instructions: "Enable this app to use it for the current request.".to_string(),
            url: "https://example.test/google-calendar".to_string(),
            is_installed: true,
            is_enabled: false,
            suggest_reason: Some("Plan and reference events from your calendar".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Enable),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    assert_snapshot!(
        "app_link_view_enable_suggestion_with_reason",
        renderable_trim_end_string_at_desired_height(&view, 72)
    );
}
