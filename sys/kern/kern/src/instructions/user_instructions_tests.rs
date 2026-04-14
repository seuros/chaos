use super::*;
use chaos_ipc::models::ContentItem;
use pretty_assertions::assert_eq;

#[test]
fn test_user_instructions() {
    let user_instructions = UserInstructions {
        directory: "test_directory".to_string(),
        text: "test_text".to_string(),
    };
    let response_item: ResponseItem = user_instructions.into();

    let ResponseItem::Message { role, content, .. } = response_item else {
        panic!("expected ResponseItem::Message");
    };

    assert_eq!(role, "user");

    let [ContentItem::InputText { text }] = content.as_slice() else {
        panic!("expected one InputText content item");
    };

    assert_eq!(
        text,
        "# AGENTS.md instructions for test_directory\n\n<INSTRUCTIONS>\ntest_text\n</INSTRUCTIONS>",
    );
}

#[test]
fn test_is_user_instructions() {
    assert!(AGENTS_MD_FRAGMENT.matches_text(
        "# AGENTS.md instructions for test_directory\n\n<INSTRUCTIONS>\ntest_text\n</INSTRUCTIONS>"
    ));
    assert!(!AGENTS_MD_FRAGMENT.matches_text("test_text"));
}
