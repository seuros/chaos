use super::*;

#[test]
fn initial_and_resume_turns_request_review() {
    let mut reflex = SessionTitleReflex::default();
    assert_eq!(
        reflex.claim_for_turn(1_000, false),
        Some(TitleReviewTrigger::Initial)
    );

    let mut resumed = SessionTitleReflex::default();
    assert_eq!(
        resumed.claim_for_turn(1_000, true),
        Some(TitleReviewTrigger::Resume)
    );
}

#[test]
fn completed_reconnect_review_suppresses_duplicate_resume_reminder() {
    let mut reflex = SessionTitleReflex::default();
    reflex.mark_reconnect_reviewed(1_000);
    assert_eq!(reflex.claim_for_turn(1_000, true), None);
}

#[test]
fn reconnect_title_decision_parses_plain_and_wrapped_json() {
    assert_eq!(
        parse_reconnect_title_decision(r#"{"title":"Souls House Work"}"#)
            .expect("plain JSON")
            .title,
        "Souls House Work"
    );
    assert_eq!(
        parse_reconnect_title_decision(
            "Here is the decision:\n```json\n{\"title\":\"Souls House Work\"}\n```"
        )
        .expect("wrapped JSON")
        .title,
        "Souls House Work"
    );
}

#[test]
fn review_repeats_after_turn_interval() {
    let mut reflex = SessionTitleReflex::default();
    assert!(reflex.claim_for_turn(0, false).is_some());
    for _ in 1..TITLE_REVIEW_TURN_INTERVAL {
        assert_eq!(reflex.claim_for_turn(0, false), None);
    }
    assert_eq!(
        reflex.claim_for_turn(0, false),
        Some(TitleReviewTrigger::TurnInterval)
    );
}

#[test]
fn review_repeats_after_token_interval() {
    let mut reflex = SessionTitleReflex::default();
    assert!(reflex.claim_for_turn(10_000, false).is_some());
    assert_eq!(
        reflex.claim_for_turn(10_000 + TITLE_REVIEW_TOKEN_INTERVAL - 1, false),
        None
    );
    assert_eq!(
        reflex.claim_for_turn(10_000 + TITLE_REVIEW_TOKEN_INTERVAL, false),
        Some(TitleReviewTrigger::TokenInterval)
    );
}

#[test]
fn title_change_and_compaction_reset_the_cadence() {
    let mut reflex = SessionTitleReflex::default();
    assert!(reflex.claim_for_turn(0, false).is_some());
    reflex.mark_title_changed(5_000);
    assert_eq!(reflex.claim_for_compaction(6_000), None);

    assert_eq!(reflex.claim_for_turn(6_000, false), None);
    assert_eq!(
        reflex.claim_for_compaction(6_000),
        Some(TitleReviewTrigger::Compaction)
    );
}

#[test]
fn lower_token_count_rebases_after_compaction() {
    let mut reflex = SessionTitleReflex::default();
    assert!(reflex.claim_for_turn(300_000, false).is_some());
    assert_eq!(reflex.claim_for_turn(20_000, false), None);
    assert_eq!(
        reflex.claim_for_turn(20_000 + TITLE_REVIEW_TOKEN_INTERVAL, false),
        Some(TitleReviewTrigger::TokenInterval)
    );
}

#[test]
fn instructions_surface_current_title_without_forcing_a_rename() {
    let instructions = title_review_instructions(
        Some("Compaction Control Testing"),
        TitleReviewTrigger::Resume,
    );
    assert!(instructions.contains("Compaction Control Testing"));
    assert!(instructions.contains("If it remains accurate, deliberately retain it."));
    assert!(instructions.contains("trigger=\"resume\""));
}

#[test]
fn title_review_is_limited_to_agent_managed_root_sessions() {
    assert!(title_review_enabled(
        crate::config::TerminalTitleMode::Agent,
        &SessionSource::Cli,
    ));
    assert!(!title_review_enabled(
        crate::config::TerminalTitleMode::Off,
        &SessionSource::Cli,
    ));
    assert!(!title_review_enabled(
        crate::config::TerminalTitleMode::Agent,
        &SessionSource::SubAgent(chaos_ipc::protocol::SubAgentSource::Other(
            "test".to_string()
        ),),
    ));
}
