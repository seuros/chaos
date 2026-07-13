use super::parse_turn_item;
use chaos_ipc::items::AgentMessageContent;
use chaos_ipc::items::TurnItem;
use chaos_ipc::items::WebSearchItem;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ReasoningItemContent;
use chaos_ipc::models::ReasoningItemReasoningSummary;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::models::WebSearchAction;
use chaos_ipc::user_input::UserInput;
use pretty_assertions::assert_eq;

#[test]
fn parses_user_message_with_text_and_two_images() {
    let img1 = "https://example.com/one.png".to_string();
    let img2 = "https://example.com/two.jpg".to_string();

    let item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![
            ContentItem::InputText {
                text: "Hello world".to_string(),
            },
            ContentItem::InputImage {
                image_url: img1.clone(),
            },
            ContentItem::InputImage {
                image_url: img2.clone(),
            },
        ],
        end_turn: None,
        phase: None,
    };

    let turn_item = parse_turn_item(&item).expect("expected user message turn item");

    match turn_item {
        TurnItem::UserMessage(user) => {
            let expected_content = vec![
                UserInput::Text {
                    text: "Hello world".to_string(),
                    text_elements: Vec::new(),
                },
                UserInput::Image { image_url: img1 },
                UserInput::Image { image_url: img2 },
            ];
            assert_eq!(user.content, expected_content);
        }
        other => panic!("expected TurnItem::UserMessage, got {other:?}"),
    }
}

#[test]
fn skips_local_image_label_text() {
    let image_url = "data:image/png;base64,abc".to_string();
    let label = chaos_ipc::models::local_image_open_tag_text(1);
    let user_text = "Please review this image.".to_string();

    let item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![
            ContentItem::InputText { text: label },
            ContentItem::InputImage {
                image_url: image_url.clone(),
            },
            ContentItem::InputText {
                text: "</image>".to_string(),
            },
            ContentItem::InputText {
                text: user_text.clone(),
            },
        ],
        end_turn: None,
        phase: None,
    };

    let turn_item = parse_turn_item(&item).expect("expected user message turn item");

    match turn_item {
        TurnItem::UserMessage(user) => {
            let expected_content = vec![
                UserInput::Image { image_url },
                UserInput::Text {
                    text: user_text,
                    text_elements: Vec::new(),
                },
            ];
            assert_eq!(user.content, expected_content);
        }
        other => panic!("expected TurnItem::UserMessage, got {other:?}"),
    }
}

#[test]
fn skips_unnamed_image_label_text() {
    let image_url = "data:image/png;base64,abc".to_string();
    let label = chaos_ipc::models::image_open_tag_text();
    let user_text = "Please review this image.".to_string();

    let item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![
            ContentItem::InputText { text: label },
            ContentItem::InputImage {
                image_url: image_url.clone(),
            },
            ContentItem::InputText {
                text: chaos_ipc::models::image_close_tag_text(),
            },
            ContentItem::InputText {
                text: user_text.clone(),
            },
        ],
        end_turn: None,
        phase: None,
    };

    let turn_item = parse_turn_item(&item).expect("expected user message turn item");

    match turn_item {
        TurnItem::UserMessage(user) => {
            let expected_content = vec![
                UserInput::Image { image_url },
                UserInput::Text {
                    text: user_text,
                    text_elements: Vec::new(),
                },
            ];
            assert_eq!(user.content, expected_content);
        }
        other => panic!("expected TurnItem::UserMessage, got {other:?}"),
    }
}

#[test]
fn parses_agent_message() {
    let item = ResponseItem::Message {
        id: Some("msg-1".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "Hello from Chaos".to_string(),
        }],
        end_turn: None,
        phase: None,
    };

    let turn_item = parse_turn_item(&item).expect("expected agent message turn item");

    match turn_item {
        TurnItem::AgentMessage(message) => {
            let Some(AgentMessageContent::Text { text }) = message.content.first() else {
                panic!("expected agent message text content");
            };
            assert_eq!(text, "Hello from Chaos");
        }
        other => panic!("expected TurnItem::AgentMessage, got {other:?}"),
    }
}

#[test]
fn parses_reasoning_summary_and_raw_content() {
    let item = ResponseItem::Reasoning {
        id: "reasoning_1".to_string(),
        summary: vec![
            ReasoningItemReasoningSummary::SummaryText {
                text: "Step 1".to_string(),
            },
            ReasoningItemReasoningSummary::SummaryText {
                text: "Step 2".to_string(),
            },
        ],
        content: Some(vec![ReasoningItemContent::ReasoningText {
            text: "raw details".to_string(),
        }]),
        encrypted_content: None,
    };

    let turn_item = parse_turn_item(&item).expect("expected reasoning turn item");

    match turn_item {
        TurnItem::Reasoning(reasoning) => {
            assert_eq!(
                reasoning.summary_text,
                vec!["Step 1".to_string(), "Step 2".to_string()]
            );
            assert_eq!(reasoning.raw_content, vec!["raw details".to_string()]);
        }
        other => panic!("expected TurnItem::Reasoning, got {other:?}"),
    }
}

#[test]
fn parses_reasoning_including_raw_content() {
    let item = ResponseItem::Reasoning {
        id: "reasoning_2".to_string(),
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "Summarized step".to_string(),
        }],
        content: Some(vec![
            ReasoningItemContent::ReasoningText {
                text: "raw step".to_string(),
            },
            ReasoningItemContent::Text {
                text: "final thought".to_string(),
            },
        ]),
        encrypted_content: None,
    };

    let turn_item = parse_turn_item(&item).expect("expected reasoning turn item");

    match turn_item {
        TurnItem::Reasoning(reasoning) => {
            assert_eq!(reasoning.summary_text, vec!["Summarized step".to_string()]);
            assert_eq!(
                reasoning.raw_content,
                vec!["raw step".to_string(), "final thought".to_string()]
            );
        }
        other => panic!("expected TurnItem::Reasoning, got {other:?}"),
    }
}

#[test]
fn parses_web_search_call() {
    let item = ResponseItem::WebSearchCall {
        id: Some("ws_1".to_string()),
        status: Some("completed".to_string()),
        action: Some(WebSearchAction::Search {
            query: Some("weather".to_string()),
            queries: None,
        }),
    };

    let turn_item = parse_turn_item(&item).expect("expected web search turn item");

    match turn_item {
        TurnItem::WebSearch(search) => assert_eq!(
            search,
            WebSearchItem {
                id: "ws_1".to_string(),
                query: "weather".to_string(),
                action: WebSearchAction::Search {
                    query: Some("weather".to_string()),
                    queries: None,
                },
            }
        ),
        other => panic!("expected TurnItem::WebSearch, got {other:?}"),
    }
}

#[test]
fn parses_web_search_open_page_call() {
    let item = ResponseItem::WebSearchCall {
        id: Some("ws_open".to_string()),
        status: Some("completed".to_string()),
        action: Some(WebSearchAction::OpenPage {
            url: Some("https://example.com".to_string()),
        }),
    };

    let turn_item = parse_turn_item(&item).expect("expected web search turn item");

    match turn_item {
        TurnItem::WebSearch(search) => assert_eq!(
            search,
            WebSearchItem {
                id: "ws_open".to_string(),
                query: "https://example.com".to_string(),
                action: WebSearchAction::OpenPage {
                    url: Some("https://example.com".to_string()),
                },
            }
        ),
        other => panic!("expected TurnItem::WebSearch, got {other:?}"),
    }
}

#[test]
fn parses_web_search_find_in_page_call() {
    let item = ResponseItem::WebSearchCall {
        id: Some("ws_find".to_string()),
        status: Some("completed".to_string()),
        action: Some(WebSearchAction::FindInPage {
            url: Some("https://example.com".to_string()),
            pattern: Some("needle".to_string()),
        }),
    };

    let turn_item = parse_turn_item(&item).expect("expected web search turn item");

    match turn_item {
        TurnItem::WebSearch(search) => assert_eq!(
            search,
            WebSearchItem {
                id: "ws_find".to_string(),
                query: "'needle' in https://example.com".to_string(),
                action: WebSearchAction::FindInPage {
                    url: Some("https://example.com".to_string()),
                    pattern: Some("needle".to_string()),
                },
            }
        ),
        other => panic!("expected TurnItem::WebSearch, got {other:?}"),
    }
}

#[test]
fn parses_partial_web_search_call_without_action_as_other() {
    let item = ResponseItem::WebSearchCall {
        id: Some("ws_partial".to_string()),
        status: Some("in_progress".to_string()),
        action: None,
    };

    let turn_item = parse_turn_item(&item).expect("expected web search turn item");
    match turn_item {
        TurnItem::WebSearch(search) => assert_eq!(
            search,
            WebSearchItem {
                id: "ws_partial".to_string(),
                query: String::new(),
                action: WebSearchAction::Other,
            }
        ),
        other => panic!("expected TurnItem::WebSearch, got {other:?}"),
    }
}
