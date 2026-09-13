use super::*;
use chaos_test_fixtures::TEST_MODEL;
use pretty_assertions::assert_eq;

#[test]
fn apply_mask_can_clear_optional_fields() {
    let mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model: TEST_MODEL.to_string(),
            reasoning_effort: Some(ReasoningEffort::High),
            developer_instructions: Some("stay focused".to_string()),
        },
    };
    let mask = CollaborationModeMask {
        name: "Clear".to_string(),
        mode: None,
        model: None,
        reasoning_effort: Some(None),
        developer_instructions: Some(None),
    };

    let expected = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model: TEST_MODEL.to_string(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };
    assert_eq!(expected, mode.apply_mask(&mask));
}

#[test]
fn mode_kind_rejects_legacy_alias_values() {
    for alias in ["code", "pair_programming", "execute", "custom"] {
        let json = format!("\"{alias}\"");
        let err = serde_json::from_str::<ModeKind>(&json).expect_err("legacy alias should fail");
        assert!(
            err.to_string().contains("unknown variant"),
            "unexpected error for {alias}: {err}"
        );
    }
}

#[test]
fn tui_visible_collaboration_modes_match_mode_kind_visibility() {
    let expected = [ModeKind::Default, ModeKind::Plan];
    assert_eq!(expected, TUI_VISIBLE_COLLABORATION_MODES);

    for mode in TUI_VISIBLE_COLLABORATION_MODES {
        assert!(mode.is_tui_visible());
    }

    assert!(!ModeKind::PairProgramming.is_tui_visible());
    assert!(!ModeKind::Execute.is_tui_visible());
}

#[test]
fn web_search_location_merge_prefers_overlay_values() {
    let base = WebSearchLocation {
        country: Some("US".to_string()),
        region: Some("CA".to_string()),
        city: None,
        timezone: Some("America/Los_Angeles".to_string()),
    };
    let overlay = WebSearchLocation {
        country: None,
        region: Some("WA".to_string()),
        city: Some("Seattle".to_string()),
        timezone: None,
    };

    let expected = WebSearchLocation {
        country: Some("US".to_string()),
        region: Some("WA".to_string()),
        city: Some("Seattle".to_string()),
        timezone: Some("America/Los_Angeles".to_string()),
    };

    assert_eq!(expected, base.merge(&overlay));
}

#[test]
fn web_search_tool_config_merge_prefers_overlay_values() {
    let base = WebSearchToolConfig {
        context_size: Some(WebSearchContextSize::Low),
        allowed_domains: Some(vec!["openai.com".to_string()]),
        location: Some(WebSearchLocation {
            country: Some("US".to_string()),
            region: Some("CA".to_string()),
            city: None,
            timezone: Some("America/Los_Angeles".to_string()),
        }),
    };
    let overlay = WebSearchToolConfig {
        context_size: Some(WebSearchContextSize::High),
        allowed_domains: None,
        location: Some(WebSearchLocation {
            country: None,
            region: Some("WA".to_string()),
            city: Some("Seattle".to_string()),
            timezone: None,
        }),
    };

    let expected = WebSearchToolConfig {
        context_size: Some(WebSearchContextSize::High),
        allowed_domains: Some(vec!["openai.com".to_string()]),
        location: Some(WebSearchLocation {
            country: Some("US".to_string()),
            region: Some("WA".to_string()),
            city: Some("Seattle".to_string()),
            timezone: Some("America/Los_Angeles".to_string()),
        }),
    };

    assert_eq!(expected, base.merge(&overlay));
}
