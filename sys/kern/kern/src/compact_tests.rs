use super::*;
use chaos_ipc::models::ContentItem;
use pretty_assertions::assert_eq;

async fn process_compacted_history_with_test_session(
    compacted_history: Vec<ResponseItem>,
    previous_turn_settings: Option<&PreviousTurnSettings>,
) -> (Vec<ResponseItem>, Vec<ResponseItem>) {
    let (session, turn_context) = crate::chaos::make_session_and_context().await;
    session
        .set_previous_turn_settings(previous_turn_settings.cloned())
        .await;
    let initial_context = session.build_initial_context(&turn_context).await;
    let refreshed = crate::compact_remote::process_compacted_history(
        &session,
        &turn_context,
        compacted_history,
        InitialContextInjection::BeforeLastUserMessage,
    )
    .await;
    (refreshed, initial_context)
}

#[tokio::test]
async fn process_compacted_history_replaces_developer_messages() {
    let compacted_history = vec![
        ResponseItem::Message {
            id: None,
            role: "system".to_string(),
            content: vec![ContentItem::InputText {
                text: "stale permissions".to_string(),
            }],
            end_turn: None,
            phase: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "summary".to_string(),
            }],
            end_turn: None,
            phase: None,
        },
        ResponseItem::Message {
            id: None,
            role: "system".to_string(),
            content: vec![ContentItem::InputText {
                text: "stale personality".to_string(),
            }],
            end_turn: None,
            phase: None,
        },
    ];
    let (refreshed, mut expected) =
        process_compacted_history_with_test_session(compacted_history, None).await;
    expected.push(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "summary".to_string(),
        }],
        end_turn: None,
        phase: None,
    });
    assert_eq!(refreshed, expected);
}

#[tokio::test]
async fn process_compacted_history_reinjects_full_initial_context() {
    let compacted_history = vec![ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "summary".to_string(),
        }],
        end_turn: None,
        phase: None,
    }];
    let (refreshed, mut expected) =
        process_compacted_history_with_test_session(compacted_history, None).await;
    expected.push(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "summary".to_string(),
        }],
        end_turn: None,
        phase: None,
    });
    assert_eq!(refreshed, expected);
}

#[tokio::test]
async fn process_compacted_history_drops_non_user_content_messages() {
    let compacted_history = vec![
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: r#"<environment_context>
  <cwd>/repo</cwd>
  <shell>zsh</shell>
</environment_context>"#
                    .to_string(),
            }],
            end_turn: None,
            phase: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: r#"<turn_aborted>
  <turn_id>turn-1</turn_id>
  <reason>interrupted</reason>
</turn_aborted>"#
                    .to_string(),
            }],
            end_turn: None,
            phase: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "summary".to_string(),
            }],
            end_turn: None,
            phase: None,
        },
        ResponseItem::Message {
            id: None,
            role: "system".to_string(),
            content: vec![ContentItem::InputText {
                text: "stale developer instructions".to_string(),
            }],
            end_turn: None,
            phase: None,
        },
    ];
    let (refreshed, mut expected) =
        process_compacted_history_with_test_session(compacted_history, None).await;
    expected.push(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "summary".to_string(),
        }],
        end_turn: None,
        phase: None,
    });
    assert_eq!(refreshed, expected);
}

#[tokio::test]
async fn process_compacted_history_inserts_context_before_last_real_user_message_only() {
    let compacted_history = vec![
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "older user".to_string(),
            }],
            end_turn: None,
            phase: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: format!("{SUMMARY_PREFIX}\nsummary text"),
            }],
            end_turn: None,
            phase: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "latest user".to_string(),
            }],
            end_turn: None,
            phase: None,
        },
    ];

    let (refreshed, initial_context) =
        process_compacted_history_with_test_session(compacted_history, None).await;
    let mut expected = vec![
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "older user".to_string(),
            }],
            end_turn: None,
            phase: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: format!("{SUMMARY_PREFIX}\nsummary text"),
            }],
            end_turn: None,
            phase: None,
        },
    ];
    expected.extend(initial_context);
    expected.push(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "latest user".to_string(),
        }],
        end_turn: None,
        phase: None,
    });
    assert_eq!(refreshed, expected);
}

#[tokio::test]
async fn process_compacted_history_reinjects_model_switch_message() {
    let compacted_history = vec![ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "summary".to_string(),
        }],
        end_turn: None,
        phase: None,
    }];
    let previous_turn_settings = PreviousTurnSettings {
        model: "previous-regular-model".to_string(),
    };

    let (refreshed, initial_context) = process_compacted_history_with_test_session(
        compacted_history,
        Some(&previous_turn_settings),
    )
    .await;

    let ResponseItem::Message { role, content, .. } = &initial_context[0] else {
        panic!("expected system message");
    };
    assert_eq!(role, "system");
    let [ContentItem::InputText { text }, ..] = content.as_slice() else {
        panic!("expected system text");
    };
    assert!(text.contains("<model_switch>"));

    let mut expected = initial_context;
    expected.push(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "summary".to_string(),
        }],
        end_turn: None,
        phase: None,
    });
    assert_eq!(refreshed, expected);
}
