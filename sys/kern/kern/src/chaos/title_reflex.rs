use std::sync::Arc;

use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::InitialHistory;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::SubAgentSource;
use chaos_ipc::user_input::UserInput;
use serde::Deserialize;
use tracing::warn;

use super::Session;
use crate::chaos_delegate::run_chaos_process_one_shot;
use crate::config::Constrained;
use crate::config::TerminalTitleMode;
use crate::rollout::process_names;
use crate::rollout::process_names::ProcessNameSource;

pub(crate) const TITLE_REVIEW_TURN_INTERVAL: u64 = 12;
pub(crate) const TITLE_REVIEW_TOKEN_INTERVAL: i64 = 50_000;

const SESSION_TITLE_REFLEX_MARKER: &str = "<session_title_reflex";
const RECONNECT_TITLE_REVIEW_SOURCE: &str = "session_title_review";
const RECONNECT_TITLE_REVIEW_PROMPT: &str = "\
You have just reconnected to an existing session. Review the restored conversation and decide \
whether its current title still accurately names the primary work. Return JSON with exactly one \
field, `title`, containing a short, concrete, distinctive 2-6 word title. Retain the current title \
when it remains accurate. If it is absent or generic, infer a useful title from the session's actual \
work. Do not explain your decision.";

#[derive(Debug, Deserialize)]
struct ReconnectTitleDecision {
    title: String,
}

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

        let trigger = if resumed && !self.reviewed_once {
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

    pub(crate) fn mark_reconnect_reviewed(&mut self, active_tokens: i64) {
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

pub(crate) async fn review_title_after_reconnect(session: Arc<Session>, history: Vec<RolloutItem>) {
    let (config, session_source, current_title) = {
        let state = session.state.lock().await;
        (
            Arc::clone(&state.session_configuration.original_config_do_not_use),
            state.session_configuration.session_source.clone(),
            state.session_configuration.process_name.clone(),
        )
    };
    if !title_review_enabled(config.terminal_title, &session_source) {
        return;
    }

    match process_names::find_process_name_record_by_id(&session.conversation_id).await {
        Ok(Some(record)) if record.source == ProcessNameSource::User => {
            mark_reconnect_reviewed(&session).await;
            return;
        }
        Ok(_) => {}
        Err(err) => {
            warn!(%err, "failed to inspect process title before reconnect review");
            return;
        }
    }

    let parent_ctx = session
        .new_default_turn_with_sub_id("reconnect-title-review".to_string())
        .await;
    let mut reviewer_config = config.as_ref().clone();
    reviewer_config.ephemeral = true;
    reviewer_config.minion_jobs_allowed = false;
    reviewer_config.collab_enabled = false;
    reviewer_config.terminal_title = TerminalTitleMode::Off;
    if let Err(err) = reviewer_config
        .web_search_mode
        .set(chaos_ipc::config_types::WebSearchMode::Disabled)
    {
        warn!(%err, "failed to disable web search for reconnect title review");
        return;
    }
    reviewer_config.permissions.approval_policy =
        Constrained::allow_only(chaos_ipc::protocol::ApprovalPolicy::Headless);

    let input = vec![UserInput::Text {
        text: format!(
            "{RECONNECT_TITLE_REVIEW_PROMPT}\n\nCurrent title: {:?}",
            current_title.as_deref().unwrap_or("new session")
        ),
        text_elements: Vec::new(),
    }];
    let output_schema =
        if crate::model_provider_info::is_anthropic_wire(parent_ctx.provider.base_url.as_deref()) {
            None
        } else {
            Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" }
                },
                "required": ["title"],
                "additionalProperties": false
            }))
        };
    let reviewer = match run_chaos_process_one_shot(
        reviewer_config,
        Arc::clone(&session.services.auth_manager),
        Arc::clone(&session.services.models_manager),
        input,
        Arc::clone(&session),
        parent_ctx,
        tokio_util::sync::CancellationToken::new(),
        SubAgentSource::Other(RECONNECT_TITLE_REVIEW_SOURCE.to_string()),
        output_schema,
        Some(InitialHistory::Forked(history)),
    )
    .await
    {
        Ok(reviewer) => reviewer,
        Err(err) => {
            warn!(%err, "failed to start reconnect title review");
            return;
        }
    };

    let mut decision = None;
    while let Ok(event) = reviewer.next_event().await {
        match event.msg {
            EventMsg::TurnComplete(event) => {
                decision = event
                    .last_agent_message
                    .as_deref()
                    .and_then(parse_reconnect_title_decision);
                break;
            }
            EventMsg::TurnAborted(_) => break,
            _ => {}
        }
    }
    let Some(title) =
        decision.and_then(|decision| crate::util::normalize_process_name(&decision.title))
    else {
        return;
    };
    if title.chars().count() > 80 {
        return;
    }
    if current_title.as_deref() == Some(title.as_str()) {
        mark_reconnect_reviewed(&session).await;
        return;
    }

    match process_names::find_process_name_record_by_id(&session.conversation_id).await {
        Ok(Some(record)) if record.source == ProcessNameSource::User => return,
        Ok(_) => {}
        Err(err) => {
            warn!(%err, "failed to recheck process title after reconnect review");
            return;
        }
    }
    match process_names::find_unarchived_process_id_by_name(&title).await {
        Ok(Some(other_process_id)) if other_process_id != session.conversation_id => {
            warn!(%title, "reconnect title review chose a title already used by another session");
            return;
        }
        Ok(_) => {}
        Err(err) => {
            warn!(%err, "failed to check reconnect title uniqueness");
            return;
        }
    }
    if let Err(err) = crate::chaos::submission_loop::handlers::persist_process_name(
        &session,
        "reconnect-title-review".to_string(),
        title,
        ProcessNameSource::Agent,
    )
    .await
    {
        warn!(%err, "failed to persist reconnect title review");
    }
}

async fn mark_reconnect_reviewed(session: &Session) {
    let mut state = session.state.lock().await;
    let active_tokens = state.get_total_token_usage(state.server_reasoning_included());
    state
        .session_title_reflex
        .mark_reconnect_reviewed(active_tokens);
}

fn parse_reconnect_title_decision(text: &str) -> Option<ReconnectTitleDecision> {
    serde_json::from_str(text).ok().or_else(|| {
        let start = text.find('{')?;
        let end = text.rfind('}')?;
        (start < end)
            .then(|| serde_json::from_str(&text[start..=end]).ok())
            .flatten()
    })
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
}
