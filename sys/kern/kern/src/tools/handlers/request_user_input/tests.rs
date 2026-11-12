use super::*;
use pretty_assertions::assert_eq;

#[test]
fn request_user_input_unavailable_messages_respect_default_mode_feature_flag() {
    assert_eq!(
        request_user_input_unavailable_message(ModeKind::Plan, false),
        None
    );
    assert_eq!(
        request_user_input_unavailable_message(ModeKind::Default, false),
        Some("request_user_input is unavailable in Default mode".to_string())
    );
    assert_eq!(
        request_user_input_unavailable_message(ModeKind::Default, true),
        None
    );
    assert_eq!(
        request_user_input_unavailable_message(ModeKind::Harness, false),
        Some("request_user_input is unavailable in Harness mode".to_string())
    );
    assert_eq!(
        request_user_input_unavailable_message(ModeKind::Harness, true),
        None
    );
    assert_eq!(
        request_user_input_unavailable_message(ModeKind::Execute, false),
        Some("request_user_input is unavailable in Execute mode".to_string())
    );
    assert_eq!(
        request_user_input_unavailable_message(ModeKind::PairProgramming, false),
        Some("request_user_input is unavailable in Pair Programming mode".to_string())
    );
}

#[test]
fn request_user_input_tool_description_mentions_available_modes() {
    for (default_mode_request_user_input, harness_mode, expected) in [
        (false, false, "Plan mode"),
        (false, true, "Plan mode"),
        (true, false, "Default or Plan mode"),
        (true, true, "Default, Plan, Harness modes"),
    ] {
        let description = request_user_input_tool_description(CollaborationModesConfig {
            default_mode_request_user_input,
            harness_mode,
        });
        assert_eq!(
            description,
            format!(
                "Request user input for one to three short questions and wait for the response. \
                 This tool is only available in {expected}."
            )
        );
    }
}
