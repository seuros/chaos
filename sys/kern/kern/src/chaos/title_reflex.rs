use chaos_ipc::protocol::SessionSource;

pub(crate) const TITLE_REVIEW_TURN_INTERVAL: u64 = 12;
pub(crate) const TITLE_REVIEW_TOKEN_INTERVAL: i64 = 50_000;

const SESSION_TITLE_REFLEX_MARKER: &str = "<session_title_reflex";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TitleReviewTrigger {
    Initial,
    Resume,
    TurnInterval,
    TokenInterval,
    Compaction,
}

impl TitleReviewTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Resume => "resume",
            Self::TurnInterval => "turn_interval",
            Self::TokenInterval => "token_interval",
            Self::Compaction => "compaction",
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct SessionTitleReflex {
    reviewed_once: bool,
    reviewed_this_turn: bool,
    turns_since_review: u64,
    tokens_at_last_review: Option<i64>,
}

impl SessionTitleReflex {
    pub(crate) fn claim_for_turn(
        &mut self,
        active_tokens: i64,
        resumed: bool,
    ) -> Option<TitleReviewTrigger> {
        self.reviewed_this_turn = false;
        self.turns_since_review = self.turns_since_review.saturating_add(1);
        self.rebase_after_compaction(active_tokens);

        let trigger = if resumed {
            Some(TitleReviewTrigger::Resume)
        } else if !self.reviewed_once {
            Some(TitleReviewTrigger::Initial)
        } else if self.turns_since_review >= TITLE_REVIEW_TURN_INTERVAL {
            Some(TitleReviewTrigger::TurnInterval)
        } else if self.tokens_since_review(active_tokens) >= TITLE_REVIEW_TOKEN_INTERVAL {
            Some(TitleReviewTrigger::TokenInterval)
        } else {
            None
        };

        if trigger.is_some() {
            self.mark_reviewed(active_tokens);
        }
        trigger
    }

    pub(crate) fn claim_for_compaction(
        &mut self,
        active_tokens: i64,
    ) -> Option<TitleReviewTrigger> {
        if self.reviewed_this_turn {
            return None;
        }
        self.mark_reviewed(active_tokens);
        Some(TitleReviewTrigger::Compaction)
    }

    pub(crate) fn mark_title_changed(&mut self, active_tokens: i64) {
        self.mark_reviewed(active_tokens);
    }

    fn mark_reviewed(&mut self, active_tokens: i64) {
        self.reviewed_once = true;
        self.reviewed_this_turn = true;
        self.turns_since_review = 0;
        self.tokens_at_last_review = Some(active_tokens);
    }

    fn rebase_after_compaction(&mut self, active_tokens: i64) {
        if self
            .tokens_at_last_review
            .is_some_and(|previous| active_tokens < previous)
        {
            self.tokens_at_last_review = Some(active_tokens);
        }
    }

    fn tokens_since_review(&self, active_tokens: i64) -> i64 {
        self.tokens_at_last_review
            .map(|previous| active_tokens.saturating_sub(previous))
            .unwrap_or_default()
    }
}

pub(crate) fn title_review_enabled(
    terminal_title: crate::config::TerminalTitleMode,
    session_source: &SessionSource,
) -> bool {
    terminal_title == crate::config::TerminalTitleMode::Agent
        && !matches!(session_source, SessionSource::SubAgent(_))
}

pub(crate) fn title_review_instructions(
    process_name: Option<&str>,
    trigger: TitleReviewTrigger,
) -> String {
    let title = process_name.unwrap_or("new session");
    format!(
        "{SESSION_TITLE_REFLEX_MARKER} trigger=\"{}\">\n\
         Current session title: {title:?}. Briefly review whether it still accurately names the session's primary work. If it has become materially stale, call `set_session_title` with a short, concrete, distinctive 2-6 word replacement. If it remains accurate, deliberately retain it. Do not rename merely because this reminder appeared. User-authored names cannot be replaced.\n\
         </session_title_reflex>",
        trigger.as_str()
    )
}

#[cfg(test)]
mod tests {
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
}
