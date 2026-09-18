use super::*;

#[test]
fn compaction_reflex_uses_the_soft_to_hard_limit_gap() {
    assert_eq!(compaction_reflex_reserve(400_000, 350_000), 50_000);
    assert!(!compaction_reflex_due(50_001, 400_000, 350_000));
    assert!(compaction_reflex_due(50_000, 400_000, 350_000));
}

#[test]
fn compaction_reflex_keeps_a_minimum_reserve() {
    assert_eq!(compaction_reflex_reserve(100, 100), 1);
    assert!(compaction_reflex_due(1, 100, 100));
}

#[test]
fn compaction_reflex_does_not_fire_immediately_for_a_low_soft_limit() {
    assert_eq!(compaction_reflex_reserve(400_000, 100_000), 25_000);
    assert!(!compaction_reflex_due(100_000, 400_000, 100_000));
    assert!(compaction_reflex_due(25_000, 400_000, 100_000));
}

#[test]
fn compaction_reflex_follow_up_stays_below_the_fixed_ceiling() {
    assert!(compaction_reflex_follow_up_allowed(359_999, 360_000));
    assert!(!compaction_reflex_follow_up_allowed(360_000, 360_000));
    assert!(!compaction_reflex_follow_up_allowed(400_000, 360_000));
}

#[test]
fn compaction_reflex_addresses_the_agent_and_preserves_choice() {
    let instructions = compaction_reflex_instructions(
        "window-1", 2, 300_000, 50_000, 350_000, 400_000, true, None,
    );

    assert!(instructions.contains("for you, the continuing agent"));
    assert!(instructions.contains("Use your normal tools now"));
    assert!(instructions.contains("Do not manufacture memories"));
    assert!(instructions.contains("window_id=\"window-1\""));
    assert!(instructions.contains("window_number=\"2\""));
    assert!(instructions.contains("compaction_control"));
    assert!(instructions.contains("defer_once"));
}

#[test]
fn compaction_reflex_can_include_title_review_guidance() {
    let instructions = compaction_reflex_instructions(
        "window-1",
        2,
        300_000,
        50_000,
        350_000,
        400_000,
        true,
        Some("<session_title_reflex>review title</session_title_reflex>"),
    );

    assert!(instructions.contains("<session_title_reflex>review title</session_title_reflex>"));
}
