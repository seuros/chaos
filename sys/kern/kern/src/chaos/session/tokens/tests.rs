use super::*;

#[test]
fn observed_400k_window_keeps_distillation_reserve() {
    assert_eq!(
        deferral_ceiling_for_windows(400_000, 380_000),
        Some(360_000)
    );
}

#[test]
fn raw_window_safety_fraction_can_be_the_binding_ceiling() {
    assert_eq!(
        deferral_ceiling_for_windows(300_000, 295_000),
        Some(270_000)
    );
}

#[test]
fn deferral_ceiling_is_one_reserve_past_a_low_compaction_limit() {
    // 1M window, 400k limit: reserve = min(950k - 400k, 400k / 4) = 100k, so a
    // deferral may reach 500k, not the 900k window-derived ceiling.
    assert_eq!(
        deferral_ceiling_for_limits(1_000_000, 950_000, Some(400_000)),
        Some(500_000)
    );
}

#[test]
fn deferral_ceiling_keeps_the_window_bound_when_the_limit_is_near_it() {
    // Observed ChatGPT preset: 400k window, 350k limit.
    assert_eq!(
        deferral_ceiling_for_limits(400_000, 380_000, Some(350_000)),
        Some(360_000)
    );
    // Catalog default: an 80% limit under a 95% effective window.
    assert_eq!(
        deferral_ceiling_for_limits(200_000, 190_000, Some(160_000)),
        Some(170_000)
    );
}

#[test]
fn deferral_ceiling_without_a_compaction_limit_is_the_window_ceiling() {
    assert_eq!(
        deferral_ceiling_for_limits(400_000, 380_000, None),
        Some(360_000)
    );
}

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
