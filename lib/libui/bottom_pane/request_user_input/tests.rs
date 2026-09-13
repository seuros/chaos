use super::*;
use crate::app_event::AppEvent;
use crate::bottom_pane::bottom_pane_view::BottomPaneView;
use crate::bottom_pane::selection_popup_common::menu_surface_inset;
use crate::render::renderable::Renderable;
use crate::test_support::make_app_event_sender_with_rx;
use crate::test_support::renderable_first_char_string;
use crate::test_support::renderable_first_char_string_with_size;
use chaos_ipc::protocol::Op;
use chaos_ipc::request_user_input::RequestUserInputAnswer;
use chaos_ipc::request_user_input::RequestUserInputQuestion;
use chaos_ipc::request_user_input::RequestUserInputQuestionOption;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
macro_rules! assert_snapshot {
    ($($arg:tt)*) => {
        insta::with_settings!({ snapshot_path => "../snapshots" }, {
            insta::assert_snapshot!($($arg)*);
        })
    };
}
use pretty_assertions::assert_eq;
use ratatui::layout::Rect;
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

fn test_sender() -> (
    AppEventSender,
    tokio::sync::mpsc::UnboundedReceiver<crate::app_event::AppEvent>,
) {
    make_app_event_sender_with_rx()
}

fn expect_interrupt_only(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) {
    let event = rx.try_recv().expect("expected interrupt AppEvent");
    let AppEvent::ChaosOp(op) = event else {
        panic!("expected ChaosOp");
    };
    assert_eq!(op, Op::Interrupt);
    assert!(
        rx.try_recv().is_err(),
        "unexpected AppEvents before interrupt completion"
    );
}

fn question_with_options(id: &str, header: &str) -> RequestUserInputQuestion {
    RequestUserInputQuestion {
        id: id.to_string(),
        header: header.to_string(),
        question: "Choose an option.".to_string(),
        is_other: false,
        is_secret: false,
        options: Some(vec![
            RequestUserInputQuestionOption {
                label: "Option 1".to_string(),
                description: "First choice.".to_string(),
            },
            RequestUserInputQuestionOption {
                label: "Option 2".to_string(),
                description: "Second choice.".to_string(),
            },
            RequestUserInputQuestionOption {
                label: "Option 3".to_string(),
                description: "Third choice.".to_string(),
            },
        ]),
    }
}

fn question_with_options_and_other(id: &str, header: &str) -> RequestUserInputQuestion {
    RequestUserInputQuestion {
        id: id.to_string(),
        header: header.to_string(),
        question: "Choose an option.".to_string(),
        is_other: true,
        is_secret: false,
        options: Some(vec![
            RequestUserInputQuestionOption {
                label: "Option 1".to_string(),
                description: "First choice.".to_string(),
            },
            RequestUserInputQuestionOption {
                label: "Option 2".to_string(),
                description: "Second choice.".to_string(),
            },
            RequestUserInputQuestionOption {
                label: "Option 3".to_string(),
                description: "Third choice.".to_string(),
            },
        ]),
    }
}

fn question_with_wrapped_options(id: &str, header: &str) -> RequestUserInputQuestion {
    RequestUserInputQuestion {
        id: id.to_string(),
        header: header.to_string(),
        question: "Choose the next step for this task.".to_string(),
        is_other: false,
        is_secret: false,
        options: Some(vec![
            RequestUserInputQuestionOption {
                label: "Discuss a code change".to_string(),
                description: "Walk through a plan, then implement it together with careful checks."
                    .to_string(),
            },
            RequestUserInputQuestionOption {
                label: "Run targeted tests".to_string(),
                description:
                    "Pick the most relevant crate and validate the current behavior first."
                        .to_string(),
            },
            RequestUserInputQuestionOption {
                label: "Review the diff".to_string(),
                description:
                    "Summarize the changes and highlight the most important risks and gaps."
                        .to_string(),
            },
        ]),
    }
}

fn question_with_very_long_option_text(id: &str, header: &str) -> RequestUserInputQuestion {
    RequestUserInputQuestion {
            id: id.to_string(),
            header: header.to_string(),
            question: "Choose one option.".to_string(),
            is_other: false,
            is_secret: false,
            options: Some(vec![
                RequestUserInputQuestionOption {
                    label: "Job: running/completed/failed/expired; Run/Experiment: succeeded/failed/unknown (Recommended when triaging long-running background work and status transitions)".to_string(),
                    description: "Keep async job statuses for progress tracking and include enough context for debugging retries, stale workers, and unexpected expiration paths.".to_string(),
                },
                RequestUserInputQuestionOption {
                    label: "Add a short status model".to_string(),
                    description: "Simpler labels with less detail for quick rollouts.".to_string(),
                },
            ]),
        }
}

fn question_with_long_scroll_options(id: &str, header: &str) -> RequestUserInputQuestion {
    RequestUserInputQuestion {
            id: id.to_string(),
            header: header.to_string(),
            question:
                "Choose one option; each hint is intentionally very long to test wrapped scrolling."
                    .to_string(),
            is_other: false,
            is_secret: false,
            options: Some(vec![
                RequestUserInputQuestionOption {
                    label: "Use Detailed Hint A (Recommended)".to_string(),
                    description: "Select this if you want a deliberately overextended explanatory hint that reads like a miniature specification, including context, rationale, expected behavior, and an explicit statement that this choice is mainly for testing how gracefully the interface wraps, truncates, and preserves readability under unusually verbose helper text conditions.".to_string(),
                },
                RequestUserInputQuestionOption {
                    label: "Use Detailed Hint B".to_string(),
                    description: "Select this if you want an equally verbose but differently phrased guidance block that emphasizes user-facing clarity, spacing tolerance, multiline wrapping, visual hierarchy interactions, and whether long descriptive metadata remains understandable when scanned quickly in a constrained layout where cognitive load is already high.".to_string(),
                },
                RequestUserInputQuestionOption {
                    label: "Use Detailed Hint C".to_string(),
                    description: "Select this when you specifically want to verify that navigating downward will keep the currently highlighted option visible, even when previous options consume many wrapped lines and would otherwise push the selection out of the viewport.".to_string(),
                },
                RequestUserInputQuestionOption {
                    label: "None of the above".to_string(),
                    description:
                        "Use this only if the previous long-form options do not apply.".to_string(),
                },
            ]),
        }
}

fn question_without_options(id: &str, header: &str) -> RequestUserInputQuestion {
    RequestUserInputQuestion {
        id: id.to_string(),
        header: header.to_string(),
        question: "Share details.".to_string(),
        is_other: false,
        is_secret: false,
        options: None,
    }
}

fn request_event(turn_id: &str, questions: Vec<RequestUserInputQuestion>) -> RequestUserInputEvent {
    RequestUserInputEvent {
        call_id: "call-1".to_string(),
        turn_id: turn_id.to_string(),
        questions,
    }
}

pub(crate) fn request_user_input_suite() {
    queued_requests_are_fifo();
    interrupt_discards_queued_requests_and_emits_interrupt();
    options_can_submit_empty_when_unanswered();
    enter_commits_default_selection_on_last_option_question();
    enter_commits_default_selection_on_non_last_option_question();
    number_keys_select_and_submit_options();
    vim_keys_move_option_selection();
    typing_in_options_does_not_open_notes();
    h_l_move_between_questions_in_options();
    left_right_move_between_questions_in_options();
    options_notes_focus_hides_question_navigation_tip();
    freeform_shows_ctrl_p_and_ctrl_n_question_navigation_tip();
    tab_opens_notes_when_option_selected();
    switching_to_options_resets_notes_focus_when_notes_hidden();
    switching_from_freeform_with_text_resets_focus_and_keeps_last_option_empty();
    esc_in_notes_mode_without_options_interrupts();
    esc_in_options_mode_interrupts();
    esc_in_notes_mode_clears_notes_and_hides_ui();
    esc_in_notes_mode_with_text_clears_notes_and_hides_ui();
    esc_drops_committed_answers();
    backspace_in_options_clears_selection();
    backspace_on_empty_notes_closes_notes_ui();
    tab_in_notes_clears_notes_and_hides_ui();
    skipped_option_questions_count_as_unanswered();
    highlighted_option_questions_are_unanswered();
    freeform_requires_enter_with_text_to_mark_answered();
    freeform_enter_with_empty_text_is_unanswered();
    freeform_questions_submit_empty_when_empty();
    freeform_draft_is_not_submitted_without_enter();
    freeform_commit_resets_when_draft_changes();
    notes_are_captured_for_selected_option();
    notes_submission_commits_selected_option();
    is_other_adds_none_of_the_above_and_submits_it();
    large_paste_is_preserved_when_switching_questions();
    pending_paste_placeholder_survives_submission_and_back_navigation();
    request_user_input_options_snapshot();
    request_user_input_options_notes_visible_snapshot();
    request_user_input_tight_height_snapshot();
    layout_allocates_all_wrapped_options_when_space_allows();
    desired_height_keeps_spacers_and_preferred_options_visible();
    footer_wraps_tips_without_splitting_individual_tips();
    request_user_input_wrapped_options_snapshot();
    request_user_input_long_option_text_snapshot();
    selected_long_wrapped_option_stays_visible();
    request_user_input_footer_wrap_snapshot();
    request_user_input_scroll_options_snapshot();
    request_user_input_hidden_options_footer_snapshot();
    request_user_input_freeform_snapshot();
    request_user_input_multi_question_first_snapshot();
    request_user_input_multi_question_last_snapshot();
    request_user_input_unanswered_confirmation_snapshot();
    options_scroll_while_editing_notes();
}

fn queued_requests_are_fifo() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "First")]),
        tx,
        true,
        false,
        false,
    );
    overlay.try_consume_user_input_request(request_event(
        "turn-2",
        vec![question_with_options("q2", "Second")],
    ));
    overlay.try_consume_user_input_request(request_event(
        "turn-3",
        vec![question_with_options("q3", "Third")],
    ));

    overlay.submit_answers();
    assert_eq!(overlay.request.turn_id, "turn-2");

    overlay.submit_answers();
    assert_eq!(overlay.request.turn_id, "turn-3");
}

fn interrupt_discards_queued_requests_and_emits_interrupt() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "First")]),
        tx,
        true,
        false,
        false,
    );
    overlay.try_consume_user_input_request(RequestUserInputEvent {
        call_id: "call-2".to_string(),
        turn_id: "turn-2".to_string(),
        questions: vec![question_with_options("q2", "Second")],
    });
    overlay.try_consume_user_input_request(RequestUserInputEvent {
        call_id: "call-3".to_string(),
        turn_id: "turn-3".to_string(),
        questions: vec![question_with_options("q3", "Third")],
    });

    overlay.handle_key_event(KeyEvent::from(KeyCode::Esc));

    assert!(overlay.done, "expected overlay to be done");
    expect_interrupt_only(&mut rx);
}

fn options_can_submit_empty_when_unanswered() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );

    overlay.submit_answers();

    let event = rx.try_recv().expect("expected AppEvent");
    let AppEvent::ChaosOp(Op::UserInputAnswer { id, response, .. }) = event else {
        panic!("expected UserInputAnswer");
    };
    assert_eq!(id, "turn-1");
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, Vec::<String>::new());
}

fn enter_commits_default_selection_on_last_option_question() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let event = rx.try_recv().expect("expected AppEvent");
    let AppEvent::ChaosOp(Op::UserInputAnswer { response, .. }) = event else {
        panic!("expected UserInputAnswer");
    };
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, vec!["Option 1".to_string()]);
}

fn enter_commits_default_selection_on_non_last_option_question() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_eq!(overlay.current_index(), 1);
    let first_answer = &overlay.answers[0];
    assert!(first_answer.answer_committed);
    assert_eq!(first_answer.options_state.selected_idx, Some(0));
    assert!(
        rx.try_recv().is_err(),
        "unexpected AppEvent before full submission"
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let event = rx.try_recv().expect("expected AppEvent");
    let AppEvent::ChaosOp(Op::UserInputAnswer { response, .. }) = event else {
        panic!("expected UserInputAnswer");
    };
    let mut expected = HashMap::new();
    expected.insert(
        "q1".to_string(),
        RequestUserInputAnswer {
            answers: vec!["Option 1".to_string()],
        },
    );
    expected.insert(
        "q2".to_string(),
        RequestUserInputAnswer {
            answers: vec!["Option 1".to_string()],
        },
    );
    assert_eq!(response.answers, expected);
}

fn number_keys_select_and_submit_options() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('2')));

    let event = rx.try_recv().expect("expected AppEvent");
    let AppEvent::ChaosOp(Op::UserInputAnswer { response, .. }) = event else {
        panic!("expected UserInputAnswer");
    };
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, vec!["Option 2".to_string()]);
}

fn vim_keys_move_option_selection() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );
    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(answer.options_state.selected_idx, Some(0));

    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('j')));
    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(answer.options_state.selected_idx, Some(1));

    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('k')));
    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(answer.options_state.selected_idx, Some(0));
}

fn typing_in_options_does_not_open_notes() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    assert_eq!(overlay.current_index(), 0);
    assert_eq!(overlay.notes_ui_visible(), false);
    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('x')));
    assert_eq!(overlay.current_index(), 0);
    assert_eq!(overlay.notes_ui_visible(), false);
    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.composer.current_text_with_pending(), "");
}

fn h_l_move_between_questions_in_options() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    assert_eq!(overlay.current_index(), 0);
    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('l')));
    assert_eq!(overlay.current_index(), 1);
    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('h')));
    assert_eq!(overlay.current_index(), 0);
}

fn left_right_move_between_questions_in_options() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    assert_eq!(overlay.current_index(), 0);
    overlay.handle_key_event(KeyEvent::from(KeyCode::Right));
    assert_eq!(overlay.current_index(), 1);
    overlay.handle_key_event(KeyEvent::from(KeyCode::Left));
    assert_eq!(overlay.current_index(), 0);
}

fn options_notes_focus_hides_question_navigation_tip() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );
    let tips = overlay.footer_tips();
    let tip_texts = tips.iter().map(|tip| tip.text.as_str()).collect::<Vec<_>>();
    assert_eq!(
        tip_texts,
        vec![
            "tab to add notes",
            "enter to submit answer",
            "←/→ to navigate questions",
            "esc to interrupt",
        ]
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    let tips = overlay.footer_tips();
    let tip_texts = tips.iter().map(|tip| tip.text.as_str()).collect::<Vec<_>>();
    assert_eq!(
        tip_texts,
        vec!["tab or esc to clear notes", "enter to submit answer",]
    );
}

fn freeform_shows_ctrl_p_and_ctrl_n_question_navigation_tip() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Area"),
                question_without_options("q2", "Goal"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );
    overlay.move_question(true);

    let tips = overlay.footer_tips();
    let tip_texts = tips.iter().map(|tip| tip.text.as_str()).collect::<Vec<_>>();
    assert_eq!(
        tip_texts,
        vec![
            "enter to submit all",
            "ctrl + p / ctrl + n change question",
            "esc to interrupt",
        ]
    );
}

fn tab_opens_notes_when_option_selected() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(1);

    assert_eq!(overlay.notes_ui_visible(), false);
    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    assert_eq!(overlay.notes_ui_visible(), true);
    assert!(matches!(overlay.focus, Focus::Notes));
}

fn switching_to_options_resets_notes_focus_when_notes_hidden() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "Notes"),
                question_with_options("q2", "Pick one"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    assert!(matches!(overlay.focus, Focus::Notes));
    overlay.move_question(true);

    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.notes_ui_visible(), false);
}

fn switching_from_freeform_with_text_resets_focus_and_keeps_last_option_empty() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "Notes"),
                question_with_options("q2", "Pick one"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    overlay
        .composer
        .set_text_content("freeform notes".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();

    overlay.move_question(true);

    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.notes_ui_visible(), false);

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert!(overlay.confirm_unanswered_active());
    assert!(
        rx.try_recv().is_err(),
        "unexpected AppEvent before confirmation submit"
    );
    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('1')));
    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let event = rx.try_recv().expect("expected AppEvent");
    let AppEvent::ChaosOp(Op::UserInputAnswer { response, .. }) = event else {
        panic!("expected UserInputAnswer");
    };
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, Vec::<String>::new());
    let answer = response.answers.get("q2").expect("answer missing");
    assert_eq!(answer.answers, vec!["Option 1".to_string()]);
}

fn esc_in_notes_mode_without_options_interrupts() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_without_options("q1", "Notes")]),
        tx,
        true,
        false,
        false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Esc));

    assert_eq!(overlay.done, true);
    expect_interrupt_only(&mut rx);
}

fn esc_in_options_mode_interrupts() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Esc));

    assert_eq!(overlay.done, true);
    expect_interrupt_only(&mut rx);
}

fn esc_in_notes_mode_clears_notes_and_hides_ui() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(0);
    answer.answer_committed = true;

    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    overlay.handle_key_event(KeyEvent::from(KeyCode::Esc));

    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(overlay.done, false);
    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.notes_ui_visible(), false);
    assert_eq!(overlay.composer.current_text_with_pending(), "");
    assert_eq!(answer.draft.text, "");
    assert_eq!(answer.options_state.selected_idx, Some(0));
    assert_eq!(answer.answer_committed, false);
    assert!(rx.try_recv().is_err());
}

fn esc_in_notes_mode_with_text_clears_notes_and_hides_ui() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(0);
    answer.answer_committed = true;

    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('a')));
    overlay.handle_key_event(KeyEvent::from(KeyCode::Esc));

    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(overlay.done, false);
    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.notes_ui_visible(), false);
    assert_eq!(overlay.composer.current_text_with_pending(), "");
    assert_eq!(answer.draft.text, "");
    assert_eq!(answer.options_state.selected_idx, Some(0));
    assert_eq!(answer.answer_committed, false);
    assert!(rx.try_recv().is_err());
}

fn esc_drops_committed_answers() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "First"),
                question_without_options("q2", "Second"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert!(
        rx.try_recv().is_err(),
        "unexpected AppEvent before interruption"
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Esc));

    expect_interrupt_only(&mut rx);
}

fn backspace_in_options_clears_selection() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(1);

    overlay.handle_key_event(KeyEvent::from(KeyCode::Backspace));

    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(answer.options_state.selected_idx, None);
    assert_eq!(overlay.notes_ui_visible(), false);
    assert!(rx.try_recv().is_err());
}

fn backspace_on_empty_notes_closes_notes_ui() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(0);

    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    assert!(matches!(overlay.focus, Focus::Notes));
    assert_eq!(overlay.notes_ui_visible(), true);

    overlay.handle_key_event(KeyEvent::from(KeyCode::Backspace));

    let answer = overlay.current_answer().expect("answer missing");
    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.notes_ui_visible(), false);
    assert_eq!(answer.options_state.selected_idx, Some(0));
    assert!(rx.try_recv().is_err());
}

fn tab_in_notes_clears_notes_and_hides_ui() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(0);

    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    overlay
        .composer
        .set_text_content("Some notes".to_string(), Vec::new(), Vec::new());

    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));

    let answer = overlay.current_answer().expect("answer missing");
    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.notes_ui_visible(), false);
    assert_eq!(overlay.composer.current_text_with_pending(), "");
    assert_eq!(answer.draft.text, "");
    assert_eq!(answer.options_state.selected_idx, Some(0));
    assert!(rx.try_recv().is_err());
}

fn skipped_option_questions_count_as_unanswered() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );

    assert_eq!(overlay.unanswered_count(), 1);
}

fn highlighted_option_questions_are_unanswered() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(0);

    assert_eq!(overlay.unanswered_count(), 1);
}

fn freeform_requires_enter_with_text_to_mark_answered() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "Notes"),
                question_without_options("q2", "More"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    overlay
        .composer
        .set_text_content("Draft".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();
    assert_eq!(overlay.unanswered_count(), 2);

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_eq!(overlay.answers[0].answer_committed, true);
    assert_eq!(overlay.unanswered_count(), 1);
}

fn freeform_enter_with_empty_text_is_unanswered() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "Notes"),
                question_without_options("q2", "More"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_eq!(overlay.answers[0].answer_committed, false);
    assert_eq!(overlay.unanswered_count(), 2);
}

fn freeform_questions_submit_empty_when_empty() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_without_options("q1", "Notes")]),
        tx,
        true,
        false,
        false,
    );

    overlay.submit_answers();

    let event = rx.try_recv().expect("expected AppEvent");
    let AppEvent::ChaosOp(Op::UserInputAnswer { response, .. }) = event else {
        panic!("expected UserInputAnswer");
    };
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, Vec::<String>::new());
}

fn freeform_draft_is_not_submitted_without_enter() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_without_options("q1", "Notes")]),
        tx,
        true,
        false,
        false,
    );
    overlay
        .composer
        .set_text_content("Draft text".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();

    overlay.submit_answers();

    let event = rx.try_recv().expect("expected AppEvent");
    let AppEvent::ChaosOp(Op::UserInputAnswer { response, .. }) = event else {
        panic!("expected UserInputAnswer");
    };
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, Vec::<String>::new());
}

fn freeform_commit_resets_when_draft_changes() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "Notes"),
                question_without_options("q2", "More"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    overlay
        .composer
        .set_text_content("Committed".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();
    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_eq!(overlay.answers[0].answer_committed, true);
    let _ = rx.try_recv();

    overlay.move_question(false);
    overlay
        .composer
        .set_text_content("Edited".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();
    overlay.move_question(true);
    assert_eq!(overlay.answers[0].answer_committed, false);

    overlay.submit_answers();

    let event = rx.try_recv().expect("expected AppEvent");
    let AppEvent::ChaosOp(Op::UserInputAnswer { response, .. }) = event else {
        panic!("expected UserInputAnswer");
    };
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, Vec::<String>::new());
}

fn notes_are_captured_for_selected_option() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );

    {
        let answer = overlay.current_answer_mut().expect("answer missing");
        answer.options_state.selected_idx = Some(1);
    }
    overlay.select_current_option(false);
    overlay
        .composer
        .set_text_content("Notes for option 2".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();
    let draft = overlay.capture_composer_draft();
    if let Some(answer) = overlay.current_answer_mut() {
        answer.draft = draft;
        answer.answer_committed = true;
    }

    overlay.submit_answers();

    let event = rx.try_recv().expect("expected AppEvent");
    let AppEvent::ChaosOp(Op::UserInputAnswer { response, .. }) = event else {
        panic!("expected UserInputAnswer");
    };
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(
        answer.answers,
        vec![
            "Option 2".to_string(),
            "user_note: Notes for option 2".to_string(),
        ]
    );
}

fn notes_submission_commits_selected_option() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Down));
    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    overlay
        .composer
        .set_text_content("Notes".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_eq!(overlay.current_index(), 1);
    let answer = overlay.answers.first().expect("answer missing");
    assert_eq!(answer.options_state.selected_idx, Some(1));
    assert!(answer.answer_committed);
}

fn is_other_adds_none_of_the_above_and_submits_it() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![question_with_options_and_other("q1", "Pick one")],
        ),
        tx,
        true,
        false,
        false,
    );

    let rows = overlay.option_rows();
    let other_row = rows.last().expect("expected none-of-the-above row");
    assert_eq!(other_row.name, "  4. None of the above");
    assert_eq!(
        other_row.description.as_deref(),
        Some(OTHER_OPTION_DESCRIPTION)
    );

    let other_idx = overlay.options_len().saturating_sub(1);
    {
        let answer = overlay.current_answer_mut().expect("answer missing");
        answer.options_state.selected_idx = Some(other_idx);
    }
    overlay
        .composer
        .set_text_content("Custom answer".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();
    let draft = overlay.capture_composer_draft();
    if let Some(answer) = overlay.current_answer_mut() {
        answer.draft = draft;
        answer.answer_committed = true;
    }

    overlay.submit_answers();

    let event = rx.try_recv().expect("expected AppEvent");
    let AppEvent::ChaosOp(Op::UserInputAnswer { response, .. }) = event else {
        panic!("expected UserInputAnswer");
    };
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(
        answer.answers,
        vec![
            OTHER_OPTION_LABEL.to_string(),
            "user_note: Custom answer".to_string(),
        ]
    );
}

fn large_paste_is_preserved_when_switching_questions() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "First"),
                question_without_options("q2", "Second"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    let large = "x".repeat(1_500);
    overlay.composer.handle_paste(large.clone());
    overlay.move_question(true);

    let draft = &overlay.answers[0].draft;
    assert_eq!(draft.pending_pastes.len(), 1);
    assert_eq!(draft.pending_pastes[0].1, large);
    assert!(draft.text.contains(&draft.pending_pastes[0].0));
    assert_eq!(draft.text_with_pending(), large);
}

fn pending_paste_placeholder_survives_submission_and_back_navigation() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "First"),
                question_with_options("q2", "Second"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    let large = "x".repeat(1_200);
    overlay.focus = Focus::Notes;
    overlay.ensure_selected_for_notes();
    overlay.composer.handle_paste(large.clone());

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    overlay.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));

    let draft = &overlay.answers[0].draft;
    assert_eq!(draft.pending_pastes.len(), 1);
    assert!(draft.text.contains(&draft.pending_pastes[0].0));
    assert_eq!(draft.text_with_pending(), large);
}

fn request_user_input_options_snapshot() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Area")]),
        tx,
        true,
        false,
        false,
    );
    assert_snapshot!(
        "request_user_input_options",
        renderable_first_char_string_with_size(&overlay, 120, 16)
    );
}

fn request_user_input_options_notes_visible_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Area")]),
        tx,
        true,
        false,
        false,
    );
    {
        let answer = overlay.current_answer_mut().expect("answer missing");
        answer.options_state.selected_idx = Some(0);
    }
    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));

    assert_snapshot!(
        "request_user_input_options_notes_visible",
        renderable_first_char_string_with_size(&overlay, 120, 16)
    );
}

fn request_user_input_tight_height_snapshot() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Area")]),
        tx,
        true,
        false,
        false,
    );
    assert_snapshot!(
        "request_user_input_tight_height",
        renderable_first_char_string_with_size(&overlay, 120, 10)
    );
}

fn layout_allocates_all_wrapped_options_when_space_allows() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![question_with_wrapped_options("q1", "Next Step")],
        ),
        tx,
        true,
        false,
        false,
    );

    let width = 48u16;
    let question_height = overlay.wrapped_question_lines(width).len() as u16;
    let options_height = overlay.options_required_height(width);
    let extras = 1u16 // progress
        .saturating_add(DESIRED_SPACERS_BETWEEN_SECTIONS)
        .saturating_add(overlay.footer_required_height(width));
    let height = question_height
        .saturating_add(options_height)
        .saturating_add(extras);
    let sections = overlay.layout_sections(Rect::new(0, 0, width, height));

    assert_eq!(sections.options_area.height, options_height);
}

fn desired_height_keeps_spacers_and_preferred_options_visible() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![question_with_wrapped_options("q1", "Next Step")],
        ),
        tx,
        true,
        false,
        false,
    );

    let width = 110u16;
    let height = overlay.desired_height(width);
    let content_area = menu_surface_inset(Rect::new(0, 0, width, height));
    let sections = overlay.layout_sections(content_area);
    let preferred = overlay.options_preferred_height(content_area.width);

    assert_eq!(sections.options_area.height, preferred);
    let question_bottom = sections.question_area.y + sections.question_area.height;
    let options_bottom = sections.options_area.y + sections.options_area.height;
    let spacer_after_question = sections.options_area.y.saturating_sub(question_bottom);
    let spacer_after_options = sections.notes_area.y.saturating_sub(options_bottom);
    assert_eq!(spacer_after_question, 1);
    assert_eq!(spacer_after_options, 1);
}

fn footer_wraps_tips_without_splitting_individual_tips() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(0);

    let width = 36u16;
    let lines = overlay.footer_tip_lines(width);
    assert!(lines.len() > 1);
    let separator_width = UnicodeWidthStr::width(TIP_SEPARATOR);
    for tips in lines {
        let used = tips.iter().enumerate().fold(0usize, |acc, (idx, tip)| {
            let tip_width = UnicodeWidthStr::width(tip.text.as_str()).min(width as usize);
            let extra = if idx == 0 {
                tip_width
            } else {
                separator_width.saturating_add(tip_width)
            };
            acc.saturating_add(extra)
        });
        assert!(used <= width as usize);
    }
}

fn request_user_input_wrapped_options_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![question_with_wrapped_options("q1", "Next Step")],
        ),
        tx,
        true,
        false,
        false,
    );

    {
        let answer = overlay.current_answer_mut().expect("answer missing");
        answer.options_state.selected_idx = Some(0);
    }

    let width = 110u16;
    let question_height = overlay.wrapped_question_lines(width).len() as u16;
    let options_height = overlay.options_required_height(width);
    let height = 1u16
        .saturating_add(question_height)
        .saturating_add(options_height)
        .saturating_add(8);
    assert_snapshot!(
        "request_user_input_wrapped_options",
        renderable_first_char_string_with_size(&overlay, width, height)
    );
}

fn request_user_input_long_option_text_snapshot() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![question_with_very_long_option_text("q1", "Status")],
        ),
        tx,
        true,
        false,
        false,
    );
    assert_snapshot!(
        "request_user_input_long_option_text",
        renderable_first_char_string_with_size(&overlay, 120, 18)
    );
}

fn selected_long_wrapped_option_stays_visible() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![question_with_long_scroll_options("q1", "Scroll")],
        ),
        tx,
        true,
        false,
        false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(2);

    let rendered = renderable_first_char_string(&overlay, Rect::new(0, 0, 80, 20));
    assert!(
        rendered.contains("› 3. Use Detailed Hint C"),
        "expected selected option to be visible in viewport\n{rendered}"
    );
}

fn request_user_input_footer_wrap_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(1);

    let width = 52u16;
    let height = overlay.desired_height(width);
    assert_snapshot!(
        "request_user_input_footer_wrap",
        renderable_first_char_string_with_size(&overlay, width, height)
    );
}

fn request_user_input_scroll_options_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![RequestUserInputQuestion {
                id: "q1".to_string(),
                header: "Next Step".to_string(),
                question: "What would you like to do next?".to_string(),
                is_other: false,
                is_secret: false,
                options: Some(vec![
                    RequestUserInputQuestionOption {
                        label: "Discuss a code change (Recommended)".to_string(),
                        description: "Walk through a plan and edit code together.".to_string(),
                    },
                    RequestUserInputQuestionOption {
                        label: "Run tests".to_string(),
                        description: "Pick a crate and run its tests.".to_string(),
                    },
                    RequestUserInputQuestionOption {
                        label: "Review a diff".to_string(),
                        description: "Summarize or review current changes.".to_string(),
                    },
                    RequestUserInputQuestionOption {
                        label: "Refactor".to_string(),
                        description: "Tighten structure and remove dead code.".to_string(),
                    },
                    RequestUserInputQuestionOption {
                        label: "Ship it".to_string(),
                        description: "Finalize and open a PR.".to_string(),
                    },
                ]),
            }],
        ),
        tx,
        true,
        false,
        false,
    );
    {
        let answer = overlay.current_answer_mut().expect("answer missing");
        answer.options_state.selected_idx = Some(3);
    }
    assert_snapshot!(
        "request_user_input_scrolling_options",
        renderable_first_char_string_with_size(&overlay, 120, 12)
    );
}

fn request_user_input_hidden_options_footer_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![RequestUserInputQuestion {
                id: "q1".to_string(),
                header: "Next Step".to_string(),
                question: "What would you like to do next?".to_string(),
                is_other: false,
                is_secret: false,
                options: Some(vec![
                    RequestUserInputQuestionOption {
                        label: "Discuss a code change (Recommended)".to_string(),
                        description: "Walk through a plan and edit code together.".to_string(),
                    },
                    RequestUserInputQuestionOption {
                        label: "Run tests".to_string(),
                        description: "Pick a crate and run its tests.".to_string(),
                    },
                    RequestUserInputQuestionOption {
                        label: "Review a diff".to_string(),
                        description: "Summarize or review current changes.".to_string(),
                    },
                    RequestUserInputQuestionOption {
                        label: "Refactor".to_string(),
                        description: "Tighten structure and remove dead code.".to_string(),
                    },
                    RequestUserInputQuestionOption {
                        label: "Ship it".to_string(),
                        description: "Finalize and open a PR.".to_string(),
                    },
                ]),
            }],
        ),
        tx,
        true,
        false,
        false,
    );
    {
        let answer = overlay.current_answer_mut().expect("answer missing");
        answer.options_state.selected_idx = Some(3);
    }
    assert_snapshot!(
        "request_user_input_hidden_options_footer",
        renderable_first_char_string_with_size(&overlay, 80, 10)
    );
}

fn request_user_input_freeform_snapshot() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_without_options("q1", "Goal")]),
        tx,
        true,
        false,
        false,
    );
    assert_snapshot!(
        "request_user_input_freeform",
        renderable_first_char_string_with_size(&overlay, 120, 10)
    );
}

fn request_user_input_multi_question_first_snapshot() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Area"),
                question_without_options("q2", "Goal"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );
    assert_snapshot!(
        "request_user_input_multi_question_first",
        renderable_first_char_string_with_size(&overlay, 120, 15)
    );
}

fn request_user_input_multi_question_last_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Area"),
                question_without_options("q2", "Goal"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );
    overlay.move_question(true);
    assert_snapshot!(
        "request_user_input_multi_question_last",
        renderable_first_char_string_with_size(&overlay, 120, 12)
    );
}

fn request_user_input_unanswered_confirmation_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Area"),
                question_without_options("q2", "Goal"),
            ],
        ),
        tx,
        true,
        false,
        false,
    );

    overlay.open_unanswered_confirmation();

    assert_snapshot!(
        "request_user_input_unanswered_confirmation",
        renderable_first_char_string_with_size(&overlay, 80, 12)
    );
}

fn options_scroll_while_editing_notes() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        true,
        false,
        false,
    );
    overlay.select_current_option(false);
    overlay.focus = Focus::Notes;
    overlay
        .composer
        .set_text_content("Notes".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();

    overlay.handle_key_event(KeyEvent::from(KeyCode::Down));

    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(answer.options_state.selected_idx, Some(1));
    assert!(!answer.answer_committed);
}
