//! Core types and helper types for `ChatWidget`.
//!
//! This module holds all supporting structs, enums, and free functions that
//! back `ChatWidget` without being part of the widget's rendering or event
//! dispatch logic.  Keeping them here reduces noise in the main module file
//! while keeping the type definitions easy to find.

use std::collections::HashMap;
use std::path::PathBuf;

use chaos_ipc::openai_models::ReasoningEffort as ReasoningEffortConfig;
use chaos_ipc::parse_command::ParsedCommand;
use chaos_ipc::protocol::ExecCommandSource;
use chaos_ipc::user_input::TextElement;
use chaos_kern::config::types::Notifications;

use crate::bottom_pane::LocalImageAttachment;
use crate::diff_render::display_path_for;
use crate::status_indicator_widget::STATUS_DETAILS_DEFAULT_MAX_LINES;
use crate::text_formatting::truncate_text;

// ── Internal helper types ─────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub(super) struct RenderedUserMessageEvent {
    pub(super) message: String,
    pub(super) remote_image_urls: Vec<String>,
    pub(super) local_images: Vec<PathBuf>,
    pub(super) text_elements: Vec<TextElement>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingSteerCompareKey {
    pub(super) message: String,
    pub(super) image_count: usize,
}

// Track information about an in-flight exec command.
pub(super) struct RunningCommand {
    pub(super) command: Vec<String>,
    pub(super) parsed_cmd: Vec<ParsedCommand>,
    pub(super) source: ExecCommandSource,
}

pub(super) struct UnifiedExecProcessSummary {
    pub(super) key: String,
    pub(super) call_id: String,
    pub(super) command_display: String,
    pub(super) recent_chunks: Vec<String>,
}

pub(super) struct UnifiedExecWaitState {
    pub(super) command_display: String,
}

impl UnifiedExecWaitState {
    pub(super) fn new(command_display: String) -> Self {
        Self { command_display }
    }

    pub(super) fn is_duplicate(&self, command_display: &str) -> bool {
        self.command_display == command_display
    }
}

#[derive(Clone, Debug)]
pub(super) struct UnifiedExecWaitStreak {
    pub(super) process_id: String,
    pub(super) command_display: Option<String>,
}

impl UnifiedExecWaitStreak {
    pub(super) fn new(process_id: String, command_display: Option<String>) -> Self {
        Self {
            process_id,
            command_display: command_display.filter(|display| !display.is_empty()),
        }
    }

    pub(super) fn update_command_display(&mut self, command_display: Option<String>) {
        if self.command_display.is_some() {
            return;
        }
        self.command_display = command_display.filter(|display| !display.is_empty());
    }
}

pub(super) fn is_unified_exec_source(source: ExecCommandSource) -> bool {
    matches!(
        source,
        ExecCommandSource::UnifiedExecStartup | ExecCommandSource::UnifiedExecInteraction
    )
}

pub(super) fn is_standard_tool_call(parsed_cmd: &[ParsedCommand]) -> bool {
    !parsed_cmd.is_empty()
        && parsed_cmd
            .iter()
            .all(|parsed| !matches!(parsed, ParsedCommand::Unknown { .. }))
}

// ── Rate-limit helpers ────────────────────────────────────────────────────────

pub(super) const RATE_LIMIT_WARNING_THRESHOLDS: [f64; 3] = [75.0, 90.0, 95.0];
pub(super) const NUDGE_MODEL_SLUG: &str = "gpt-5.1-codex-mini";
pub(super) const RATE_LIMIT_SWITCH_PROMPT_THRESHOLD: f64 = 90.0;

#[derive(Default)]
pub(super) struct RateLimitWarningState {
    pub(super) secondary_index: usize,
    pub(super) primary_index: usize,
}

impl RateLimitWarningState {
    pub(super) fn take_warnings(
        &mut self,
        secondary_used_percent: Option<f64>,
        secondary_window_minutes: Option<i64>,
        primary_used_percent: Option<f64>,
        primary_window_minutes: Option<i64>,
    ) -> Vec<String> {
        let reached_secondary_cap =
            matches!(secondary_used_percent, Some(percent) if percent == 100.0);
        let reached_primary_cap = matches!(primary_used_percent, Some(percent) if percent == 100.0);
        if reached_secondary_cap || reached_primary_cap {
            return Vec::new();
        }

        let mut warnings = Vec::new();

        if let Some(secondary_used_percent) = secondary_used_percent {
            let mut highest_secondary: Option<f64> = None;
            while self.secondary_index < RATE_LIMIT_WARNING_THRESHOLDS.len()
                && secondary_used_percent >= RATE_LIMIT_WARNING_THRESHOLDS[self.secondary_index]
            {
                highest_secondary = Some(RATE_LIMIT_WARNING_THRESHOLDS[self.secondary_index]);
                self.secondary_index += 1;
            }
            if let Some(threshold) = highest_secondary {
                let limit_label = secondary_window_minutes
                    .map(get_limits_duration)
                    .unwrap_or_else(|| "weekly".to_string());
                let remaining_percent = 100.0 - threshold;
                warnings.push(format!(
                    "Heads up, you have less than {remaining_percent:.0}% of your {limit_label} limit left. Run /status for a breakdown."
                ));
            }
        }

        if let Some(primary_used_percent) = primary_used_percent {
            let mut highest_primary: Option<f64> = None;
            while self.primary_index < RATE_LIMIT_WARNING_THRESHOLDS.len()
                && primary_used_percent >= RATE_LIMIT_WARNING_THRESHOLDS[self.primary_index]
            {
                highest_primary = Some(RATE_LIMIT_WARNING_THRESHOLDS[self.primary_index]);
                self.primary_index += 1;
            }
            if let Some(threshold) = highest_primary {
                let limit_label = primary_window_minutes
                    .map(get_limits_duration)
                    .unwrap_or_else(|| "5h".to_string());
                let remaining_percent = 100.0 - threshold;
                warnings.push(format!(
                    "Heads up, you have less than {remaining_percent:.0}% of your {limit_label} limit left. Run /status for a breakdown."
                ));
            }
        }

        warnings
    }
}

pub fn get_limits_duration(windows_minutes: i64) -> String {
    const MINUTES_PER_HOUR: i64 = 60;
    const MINUTES_PER_DAY: i64 = 24 * MINUTES_PER_HOUR;
    const MINUTES_PER_WEEK: i64 = 7 * MINUTES_PER_DAY;
    const MINUTES_PER_MONTH: i64 = 30 * MINUTES_PER_DAY;
    const ROUNDING_BIAS_MINUTES: i64 = 3;

    let windows_minutes = windows_minutes.max(0);

    if windows_minutes <= MINUTES_PER_DAY.saturating_add(ROUNDING_BIAS_MINUTES) {
        let adjusted = windows_minutes.saturating_add(ROUNDING_BIAS_MINUTES);
        let hours = std::cmp::max(1, adjusted / MINUTES_PER_HOUR);
        format!("{hours}h")
    } else if windows_minutes <= MINUTES_PER_WEEK.saturating_add(ROUNDING_BIAS_MINUTES) {
        "weekly".to_string()
    } else if windows_minutes <= MINUTES_PER_MONTH.saturating_add(ROUNDING_BIAS_MINUTES) {
        "monthly".to_string()
    } else {
        "annual".to_string()
    }
}

// ── State machine enums ───────────────────────────────────────────────────────

#[derive(Default)]
pub(super) enum RateLimitSwitchPromptState {
    #[default]
    Idle,
    Pending,
    Shown,
}

#[derive(Debug, Clone, Default)]
pub(super) enum ConnectorsCacheState {
    #[default]
    Uninitialized,
    #[expect(dead_code)]
    Loading,
    Ready(crate::app_event::ConnectorsSnapshot),
    Failed(#[allow(dead_code)] String),
}

#[derive(Debug)]
pub(super) enum RateLimitErrorKind {
    ServerOverloaded,
    UsageLimit,
    Generic,
}

pub(super) fn rate_limit_error_kind(
    info: &chaos_ipc::protocol::ChaosErrorInfo,
) -> Option<RateLimitErrorKind> {
    match info {
        chaos_ipc::protocol::ChaosErrorInfo::ServerOverloaded => {
            Some(RateLimitErrorKind::ServerOverloaded)
        }
        chaos_ipc::protocol::ChaosErrorInfo::UsageLimitExceeded => {
            Some(RateLimitErrorKind::UsageLimit)
        }
        chaos_ipc::protocol::ChaosErrorInfo::ResponseTooManyFailedAttempts {
            http_status_code: Some(429),
        } => Some(RateLimitErrorKind::Generic),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExternalEditorState {
    #[default]
    Closed,
    Requested,
    Active,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StatusIndicatorState {
    pub(super) header: String,
    pub(super) details: Option<String>,
    pub(super) details_max_lines: usize,
}

impl StatusIndicatorState {
    pub(super) fn working() -> Self {
        Self {
            header: String::from("Working"),
            details: None,
            details_max_lines: STATUS_DETAILS_DEFAULT_MAX_LINES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PreClampSelection {
    pub(super) model: String,
    pub(super) reasoning_effort: Option<ReasoningEffortConfig>,
    pub(super) plan_mode_reasoning_effort: Option<ReasoningEffortConfig>,
}

// ── Public data types ─────────────────────────────────────────────────────────

/// Snapshot of active-cell state that affects transcript overlay rendering.
///
/// The overlay keeps a cached "live tail" for the in-flight cell; this key lets
/// it cheaply decide when to recompute that tail as the active cell evolves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveCellTranscriptKey {
    /// Cache-busting revision for in-place updates.
    pub revision: u64,
    /// Whether the active cell continues the prior stream.
    pub is_stream_continuation: bool,
    /// Optional animation tick for time-dependent transcript output.
    pub animation_tick: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserMessage {
    pub(super) text: String,
    pub(super) local_images: Vec<LocalImageAttachment>,
    /// Remote image attachments represented as URLs.
    pub(super) remote_image_urls: Vec<String>,
    pub(super) text_elements: Vec<TextElement>,
    pub(super) mention_bindings: Vec<crate::bottom_pane::MentionBinding>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct ProcessComposerState {
    pub(super) text: String,
    pub(super) local_images: Vec<LocalImageAttachment>,
    pub(super) remote_image_urls: Vec<String>,
    pub(super) text_elements: Vec<TextElement>,
    pub(super) mention_bindings: Vec<crate::bottom_pane::MentionBinding>,
    pub(super) pending_pastes: Vec<(String, String)>,
}

impl ProcessComposerState {
    pub(super) fn has_content(&self) -> bool {
        !self.text.is_empty()
            || !self.local_images.is_empty()
            || !self.remote_image_urls.is_empty()
            || !self.text_elements.is_empty()
            || !self.mention_bindings.is_empty()
            || !self.pending_pastes.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessInputState {
    pub(super) composer: Option<ProcessComposerState>,
    pub(super) pending_steers: std::collections::VecDeque<UserMessage>,
    pub(super) queued_user_messages: std::collections::VecDeque<UserMessage>,
    pub(super) current_collaboration_mode: chaos_ipc::config_types::CollaborationMode,
    pub(super) active_collaboration_mask: Option<chaos_ipc::config_types::CollaborationModeMask>,
    pub(super) agent_turn_running: bool,
}

impl From<String> for UserMessage {
    fn from(text: String) -> Self {
        Self {
            text,
            local_images: Vec::new(),
            remote_image_urls: Vec::new(),
            text_elements: Vec::new(),
            mention_bindings: Vec::new(),
        }
    }
}

impl From<&str> for UserMessage {
    fn from(text: &str) -> Self {
        Self {
            text: text.to_string(),
            local_images: Vec::new(),
            remote_image_urls: Vec::new(),
            text_elements: Vec::new(),
            mention_bindings: Vec::new(),
        }
    }
}

pub(super) struct PendingSteer {
    pub(super) user_message: UserMessage,
    pub(super) compare_key: PendingSteerCompareKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReplayKind {
    ResumeInitialMessages,
    ProcessSnapshot,
}

// ── Message creation helpers ──────────────────────────────────────────────────

pub fn create_initial_user_message(
    text: Option<String>,
    local_image_paths: Vec<PathBuf>,
    text_elements: Vec<TextElement>,
) -> Option<UserMessage> {
    use chaos_ipc::models::local_image_label_text;
    let text = text.unwrap_or_default();
    if text.is_empty() && local_image_paths.is_empty() {
        None
    } else {
        let local_images = local_image_paths
            .into_iter()
            .enumerate()
            .map(|(idx, path)| LocalImageAttachment {
                placeholder: local_image_label_text(idx + 1),
                path,
            })
            .collect();
        Some(UserMessage {
            text,
            local_images,
            remote_image_urls: Vec::new(),
            text_elements,
            mention_bindings: Vec::new(),
        })
    }
}

pub(super) fn append_text_with_rebased_elements(
    target_text: &mut String,
    target_text_elements: &mut Vec<TextElement>,
    text: &str,
    text_elements: impl IntoIterator<Item = TextElement>,
) {
    let offset = target_text.len();
    target_text.push_str(text);
    target_text_elements.extend(text_elements.into_iter().map(|mut element| {
        element.byte_range.start += offset;
        element.byte_range.end += offset;
        element
    }));
}

pub(super) fn remap_placeholders_for_message(
    message: UserMessage,
    next_label: &mut usize,
) -> UserMessage {
    use chaos_ipc::models::local_image_label_text;
    let UserMessage {
        text,
        text_elements,
        local_images,
        remote_image_urls,
        mention_bindings,
    } = message;
    if local_images.is_empty() {
        return UserMessage {
            text,
            text_elements,
            local_images,
            remote_image_urls,
            mention_bindings,
        };
    }

    let mut mapping: HashMap<String, String> = HashMap::new();
    let mut remapped_images = Vec::new();
    for attachment in local_images {
        let new_placeholder = local_image_label_text(*next_label);
        *next_label += 1;
        mapping.insert(attachment.placeholder.clone(), new_placeholder.clone());
        remapped_images.push(LocalImageAttachment {
            placeholder: new_placeholder,
            path: attachment.path,
        });
    }

    let mut elements = text_elements;
    elements.sort_by_key(|elem| elem.byte_range.start);

    let mut cursor = 0usize;
    let mut rebuilt = String::new();
    let mut rebuilt_elements = Vec::new();
    for mut elem in elements {
        let start = elem.byte_range.start.min(text.len());
        let end = elem.byte_range.end.min(text.len());
        if let Some(segment) = text.get(cursor..start) {
            rebuilt.push_str(segment);
        }

        let original = text.get(start..end).unwrap_or("");
        let placeholder = elem.placeholder(&text);
        let replacement = placeholder
            .and_then(|ph| mapping.get(ph))
            .map(String::as_str)
            .unwrap_or(original);

        let elem_start = rebuilt.len();
        rebuilt.push_str(replacement);
        let elem_end = rebuilt.len();

        if let Some(remapped) = placeholder.and_then(|ph| mapping.get(ph)) {
            elem.set_placeholder(Some(remapped.clone()));
        }
        elem.byte_range = (elem_start..elem_end).into();
        rebuilt_elements.push(elem);
        cursor = end;
    }
    if let Some(segment) = text.get(cursor..) {
        rebuilt.push_str(segment);
    }

    UserMessage {
        text: rebuilt,
        local_images: remapped_images,
        remote_image_urls,
        text_elements: rebuilt_elements,
        mention_bindings,
    }
}

pub(super) fn merge_user_messages(messages: Vec<UserMessage>) -> UserMessage {
    let mut combined = UserMessage {
        text: String::new(),
        text_elements: Vec::new(),
        local_images: Vec::new(),
        remote_image_urls: Vec::new(),
        mention_bindings: Vec::new(),
    };
    let total_remote_images = messages
        .iter()
        .map(|message| message.remote_image_urls.len())
        .sum::<usize>();
    let mut next_image_label = total_remote_images + 1;

    for (idx, message) in messages.into_iter().enumerate() {
        if idx > 0 {
            combined.text.push('\n');
        }
        let UserMessage {
            text,
            text_elements,
            local_images,
            remote_image_urls,
            mention_bindings,
        } = remap_placeholders_for_message(message, &mut next_image_label);
        append_text_with_rebased_elements(
            &mut combined.text,
            &mut combined.text_elements,
            &text,
            text_elements,
        );
        combined.local_images.extend(local_images);
        combined.remote_image_urls.extend(remote_image_urls);
        combined.mention_bindings.extend(mention_bindings);
    }

    combined
}

// ── Notification ──────────────────────────────────────────────────────────────

pub(super) const AGENT_NOTIFICATION_PREVIEW_GRAPHEMES: usize = 200;

#[derive(Debug)]
pub(super) enum Notification {
    AgentTurnComplete {
        response: String,
    },
    ExecApprovalRequested {
        command: String,
    },
    EditApprovalRequested {
        cwd: PathBuf,
        changes: Vec<PathBuf>,
    },
    ElicitationRequested {
        server_name: String,
    },
    PlanModePrompt {
        title: String,
    },
    UserInputRequested {
        question_count: usize,
        summary: Option<String>,
    },
}

impl Notification {
    pub(super) fn display(&self) -> String {
        match self {
            Notification::AgentTurnComplete { response } => {
                Notification::agent_turn_preview(response)
                    .unwrap_or_else(|| "Agent turn complete".to_string())
            }
            Notification::ExecApprovalRequested { command } => {
                format!(
                    "Approval requested: {}",
                    truncate_text(command, /*max_graphemes*/ 30)
                )
            }
            Notification::EditApprovalRequested { cwd, changes } => {
                format!(
                    "Chaos wants to edit {}",
                    if changes.len() == 1 {
                        #[allow(clippy::unwrap_used)]
                        display_path_for(changes.first().unwrap(), cwd)
                    } else {
                        format!("{} files", changes.len())
                    }
                )
            }
            Notification::ElicitationRequested { server_name } => {
                format!("Approval requested by {server_name}")
            }
            Notification::PlanModePrompt { title } => {
                format!("Plan mode prompt: {title}")
            }
            Notification::UserInputRequested {
                question_count,
                summary,
            } => match (*question_count, summary.as_deref()) {
                (1, Some(summary)) => format!("Question requested: {summary}"),
                (1, None) => "Question requested".to_string(),
                (count, _) => format!("Questions requested: {count}"),
            },
        }
    }

    pub(super) fn type_name(&self) -> &str {
        match self {
            Notification::AgentTurnComplete { .. } => "agent-turn-complete",
            Notification::ExecApprovalRequested { .. }
            | Notification::EditApprovalRequested { .. }
            | Notification::ElicitationRequested { .. } => "approval-requested",
            Notification::PlanModePrompt { .. } => "plan-mode-prompt",
            Notification::UserInputRequested { .. } => "user-input-requested",
        }
    }

    pub(super) fn priority(&self) -> u8 {
        match self {
            Notification::AgentTurnComplete { .. } => 0,
            Notification::ExecApprovalRequested { .. }
            | Notification::EditApprovalRequested { .. }
            | Notification::ElicitationRequested { .. }
            | Notification::PlanModePrompt { .. }
            | Notification::UserInputRequested { .. } => 1,
        }
    }

    pub(super) fn allowed_for(&self, settings: &Notifications) -> bool {
        match settings {
            Notifications::Enabled(enabled) => *enabled,
            Notifications::Custom(allowed) => allowed.iter().any(|a| a == self.type_name()),
        }
    }

    pub(super) fn agent_turn_preview(response: &str) -> Option<String> {
        let mut normalized = String::new();
        for part in response.split_whitespace() {
            if !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push_str(part);
        }
        let trimmed = normalized.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(truncate_text(trimmed, AGENT_NOTIFICATION_PREVIEW_GRAPHEMES))
        }
    }

    pub(super) fn user_input_request_summary(
        questions: &[chaos_ipc::request_user_input::RequestUserInputQuestion],
    ) -> Option<String> {
        let first_question = questions.first()?;
        let summary = if first_question.header.trim().is_empty() {
            first_question.question.trim()
        } else {
            first_question.header.trim()
        };
        if summary.is_empty() {
            None
        } else {
            Some(truncate_text(summary, /*max_graphemes*/ 30))
        }
    }
}

// ── Free-function helpers ─────────────────────────────────────────────────────

/// Extract the first bold (Markdown) element in the form `**...**` from `s`.
/// Returns the inner text if found; otherwise `None`.
pub(super) fn extract_first_bold(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'*' {
            let start = i + 2;
            let mut j = start;
            while j + 1 < bytes.len() {
                if bytes[j] == b'*' && bytes[j + 1] == b'*' {
                    let inner = &s[start..j];
                    let trimmed = inner.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    } else {
                        return None;
                    }
                }
                j += 1;
            }
            return None;
        }
        i += 1;
    }
    None
}

pub(super) fn hook_event_label(event_name: chaos_ipc::protocol::HookEventName) -> &'static str {
    match event_name {
        chaos_ipc::protocol::HookEventName::SessionStart => "SessionStart",
        chaos_ipc::protocol::HookEventName::Stop => "Stop",
    }
}

pub(super) fn has_websocket_timing_metrics(summary: chaos_syslog::RuntimeMetricsSummary) -> bool {
    summary.responses_api_overhead_ms > 0
        || summary.responses_api_inference_time_ms > 0
        || summary.responses_api_engine_iapi_ttft_ms > 0
        || summary.responses_api_engine_service_ttft_ms > 0
        || summary.responses_api_engine_iapi_tbt_ms > 0
        || summary.responses_api_engine_service_tbt_ms > 0
}
