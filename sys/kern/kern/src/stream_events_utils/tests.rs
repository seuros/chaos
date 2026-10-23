use super::handle_non_tool_response_item;
use super::last_assistant_message_from_item;
use super::save_image_generation_result;
use crate::chaos::make_session_and_context;
use crate::error::ChaosErr;
use chaos_ipc::items::TurnItem;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use pretty_assertions::assert_eq;

fn assistant_output_text(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some("msg-1".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        end_turn: Some(true),
        phase: None,
    }
}

#[tokio::test]
async fn handle_non_tool_response_item_strips_citations_from_assistant_message() {
    let (session, turn_context) = make_session_and_context().await;
    let item = assistant_output_text("hello<oai-mem-citation>doc1</oai-mem-citation> world");

    let turn_item = handle_non_tool_response_item(&session, &turn_context, &item, false)
        .await
        .expect("assistant message should parse");

    let TurnItem::AgentMessage(agent_message) = turn_item else {
        panic!("expected agent message");
    };
    let text = agent_message
        .content
        .iter()
        .map(|entry| match entry {
            chaos_ipc::items::AgentMessageContent::Text { text } => text.as_str(),
        })
        .collect::<String>();
    assert_eq!(text, "hello world");
}

#[test]
fn last_assistant_message_filters_hidden_content_and_omits_empty_results() {
    for (text, hide_plan, expected) in [
        (
            "before<oai-mem-citation>doc1</oai-mem-citation>\n<proposed_plan>\n- x\n</proposed_plan>\nafter",
            true,
            Some("before\nafter"),
        ),
        ("<oai-mem-citation>doc1</oai-mem-citation>", false, None),
        ("<proposed_plan>\n- x\n</proposed_plan>", true, None),
    ] {
        assert_eq!(
            last_assistant_message_from_item(&assistant_output_text(text), hide_plan).as_deref(),
            expected,
            "{text}"
        );
    }
}

#[tokio::test]
async fn save_image_generation_result_saves_base64_to_png_in_temp_dir() {
    let expected_path = std::env::temp_dir().join("ig_save_base64.png");
    let _ = std::fs::remove_file(&expected_path);

    let saved_path = save_image_generation_result("ig_save_base64", "Zm9v")
        .await
        .expect("image should be saved");

    assert_eq!(saved_path, expected_path);
    assert_eq!(std::fs::read(&saved_path).expect("saved file"), b"foo");
    let _ = std::fs::remove_file(&saved_path);
}

#[tokio::test]
async fn save_image_generation_result_overwrites_existing_file() {
    let existing_path = std::env::temp_dir().join("ig_overwrite.png");
    std::fs::write(&existing_path, b"existing").expect("seed existing image");

    let saved_path = save_image_generation_result("ig_overwrite", "Zm9v")
        .await
        .expect("image should be saved");

    assert_eq!(saved_path, existing_path);
    assert_eq!(std::fs::read(&saved_path).expect("saved file"), b"foo");
    let _ = std::fs::remove_file(&saved_path);
}

#[tokio::test]
async fn save_image_generation_result_sanitizes_call_id_for_temp_dir_output_path() {
    let expected_path = std::env::temp_dir().join("___ig___.png");
    let _ = std::fs::remove_file(&expected_path);

    let saved_path = save_image_generation_result("../ig/..", "Zm9v")
        .await
        .expect("image should be saved");

    assert_eq!(saved_path, expected_path);
    assert_eq!(std::fs::read(&saved_path).expect("saved file"), b"foo");
    let _ = std::fs::remove_file(&saved_path);
}

#[tokio::test]
async fn save_image_generation_result_requires_standard_base64_without_data_url() {
    for payload in [
        "data:image/jpeg;base64,Zm9v",
        "_-8",
        "data:image/svg+xml,<svg/>",
    ] {
        let err = save_image_generation_result("ig_invalid", payload)
            .await
            .expect_err("invalid image payload should error");
        std::assert_matches!(err, ChaosErr::InvalidRequest(_), "{payload}");
    }
}
