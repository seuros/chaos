use super::*;

#[test]
fn classifies_contextual_fragments_and_regular_user_text() {
    for (text, expected) in [
        (
            "<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>",
            true,
        ),
        ("<SUBAGENT_NOTIFICATION>{}</subagent_notification>", true),
        ("hello", false),
    ] {
        assert_eq!(
            is_contextual_user_fragment(&ContentItem::InputText {
                text: text.to_string(),
            }),
            expected,
            "{text:?}",
        );
    }
}
