//! The main Chaos TUI chat surface.
//!
//! `ChatWidget` consumes protocol events, builds and updates history cells, and drives rendering
//! for both the main viewport and overlay UIs.
//!
//! The UI has both committed transcript cells (finalized `HistoryCell`s) and an in-flight active
//! cell (`ChatWidget.active_cell`) that can mutate in place while streaming (often representing a
//! coalesced exec/tool group). The transcript overlay (`Ctrl+T`) renders committed cells plus a
//! cached, render-only live tail derived from the current active cell so in-flight tool calls are
//! visible immediately.
//!
//! The transcript overlay is kept in sync by `App::overlay_forward_event`, which syncs a live tail
//! during draws using `active_cell_transcript_key()` and `active_cell_transcript_lines()`. The
//! cache key is designed to change when the active cell mutates in place or when its transcript
//! output is time-dependent so the overlay can refresh its cached tail without rebuilding it on
//! every draw.
//!
//! The bottom pane exposes a single "task running" indicator that drives the spinner and interrupt
//! hints. This module treats that indicator as derived UI-busy state: it is set while an agent turn
//! is in progress and while MCP server startup is in progress. Those lifecycles are tracked
//! independently (`agent_turn_running` and `mcp_startup_status`) and synchronized via
//! `update_task_running_state`.
//!
//! For preamble-capable models, assistant output may include commentary before
//! the final answer. During streaming we hide the status row to avoid duplicate
//! progress indicators; once commentary completes and stream queues drain, we
//! re-show it so users still see turn-in-progress state between output bursts.
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use chaos_amphetamine::SleepInhibitor;
use chaos_ipc::ProcessId;
use chaos_ipc::account::PlanType;
use chaos_ipc::api::AppInfo;
use chaos_ipc::config_types::CollaborationMode;
use chaos_ipc::config_types::CollaborationModeMask;
use chaos_ipc::config_types::ModeKind;
use chaos_ipc::config_types::Personality;

use chaos_ipc::config_types::Settings;
use chaos_ipc::protocol::Event;

use chaos_ipc::protocol::ListCustomPromptsResponseEvent;
use chaos_ipc::protocol::McpListToolsResponseEvent;
use chaos_ipc::protocol::McpStartupStatus;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::RateLimitSnapshot;
use chaos_ipc::protocol::ReviewRequest;
use chaos_ipc::protocol::ReviewTarget;
use chaos_ipc::protocol::TokenUsage;
use chaos_ipc::protocol::TokenUsageInfo;
use chaos_ipc::user_input::TextElement;
use chaos_ipc::user_input::UserInput;
use chaos_kern::config::Config;
use chaos_kern::config::ConstraintResult;
use chaos_kern::config::types::ApprovalsReviewer;
use chaos_kern::features::Feature;
use chaos_kern::find_process_name_by_id;
use chaos_kern::git_info::current_branch_name;
use chaos_kern::git_info::local_git_branches;
use chaos_kern::mcp::McpManager;
use chaos_kern::models_manager::manager::ModelsManager;
use chaos_kern::project_doc::DEFAULT_PROJECT_DOC_FILENAME;
use chaos_kern::terminal::TerminalName;
use chaos_kern::terminal::terminal_info;
use chaos_syslog::RuntimeMetricsSummary;
use chaos_syslog::SessionTelemetry;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use rand::RngExt;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use tracing::debug;
use tracing::warn;

const DEFAULT_MODEL_DISPLAY_NAME: &str = "loading";
const PLAN_IMPLEMENTATION_TITLE: &str = "Implement this plan?";
const PLAN_IMPLEMENTATION_YES: &str = "Yes, implement this plan";
const PLAN_IMPLEMENTATION_NO: &str = "No, stay in Plan mode";
const PLAN_IMPLEMENTATION_CODING_MESSAGE: &str = "Implement the plan.";
const PLAN_MODE_REASONING_SCOPE_TITLE: &str = "Apply reasoning change";
const PLAN_MODE_REASONING_SCOPE_PLAN_ONLY: &str = "Apply to Plan mode override";
const PLAN_MODE_REASONING_SCOPE_ALL_MODES: &str = "Apply to global default and Plan mode override";
const CONNECTORS_SELECTION_VIEW_ID: &str = "connectors-selection";

/// Choose the keybinding used to edit the most-recently queued message.
///
/// Apple Terminal, Warp, and VSCode integrated terminals intercept or silently
/// swallow Alt+Up, so users in those environments would never be able to trigger
/// the edit action.  We fall back to Shift+Left for those terminals while
/// keeping the more discoverable Alt+Up everywhere else.
///
/// The match is exhaustive so that adding a new `TerminalName` variant forces
/// an explicit decision about which binding that terminal should use.
fn queued_message_edit_binding_for_terminal(terminal_name: TerminalName) -> KeyBinding {
    match terminal_name {
        TerminalName::AppleTerminal | TerminalName::WarpTerminal | TerminalName::VsCode => {
            key_hint::shift(KeyCode::Left)
        }
        TerminalName::Ghostty
        | TerminalName::Iterm2
        | TerminalName::WezTerm
        | TerminalName::Kitty
        | TerminalName::Alacritty
        | TerminalName::Konsole
        | TerminalName::GnomeTerminal
        | TerminalName::Vte
        | TerminalName::Dumb
        | TerminalName::Unknown => key_hint::alt(KeyCode::Up),
    }
}

use crate::app_event::AppEvent;
use crate::app_event::ConnectorsSnapshot;
use crate::app_event::ExitMode;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPane;
use crate::bottom_pane::BottomPaneParams;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::CollaborationModeIndicator;
use crate::bottom_pane::ColumnWidthMode;
use crate::bottom_pane::DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED;
use crate::bottom_pane::InputResult;
use crate::bottom_pane::LocalImageAttachment;
use crate::bottom_pane::MentionBinding;
use crate::bottom_pane::QUIT_SHORTCUT_TIMEOUT;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::custom_prompt_view::CustomPromptView;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::clipboard_paste::paste_image_to_temp_png;
use crate::clipboard_text;
use crate::collaboration_modes;
use crate::exec_cell::ExecCell;
use crate::get_git_diff::get_git_diff;
use crate::history_cell;
use crate::history_cell::HistoryCell;
use crate::history_cell::McpToolCallCell;
use crate::history_cell::PlainHistoryCell;
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::multi_agents;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::slash_command::SlashCommand;
use crate::status::RateLimitSnapshotDisplay;
use crate::tui::FrameRequester;
mod interrupts;
use self::interrupts::InterruptManager;
mod agent;
use self::agent::spawn_agent;
use self::agent::spawn_agent_from_existing;
pub use self::agent::spawn_op_forwarder;
mod core;
pub use self::core::ActiveCellTranscriptKey;
use self::core::ConnectorsCacheState;
pub use self::core::ExternalEditorState;
use self::core::NUDGE_MODEL_SLUG;
use self::core::Notification;
use self::core::PendingSteer;
use self::core::PreClampSelection;
use self::core::ProcessComposerState;
pub use self::core::ProcessInputState;
use self::core::RateLimitSwitchPromptState;
use self::core::RateLimitWarningState;
use self::core::RenderedUserMessageEvent;
use self::core::RunningCommand;
use self::core::StatusIndicatorState;
use self::core::UnifiedExecProcessSummary;
use self::core::UnifiedExecWaitState;
use self::core::UnifiedExecWaitStreak;
pub use self::core::UserMessage;
pub use self::core::create_initial_user_message;
pub use self::core::get_limits_duration;
use self::core::merge_user_messages;
mod events;
mod render;
mod session_header;
use self::session_header::SessionHeader;
mod streaming;
use crate::mention_codec::LinkedMention;
use crate::mention_codec::encode_history_mentions;
use crate::streaming::chunking::AdaptiveChunkingPolicy;
use crate::streaming::controller::PlanStreamController;
use crate::streaming::controller::StreamController;

use chaos_ipc::openai_models::InputModality;
use chaos_ipc::openai_models::ModelPreset;
use chaos_ipc::openai_models::ReasoningEffort as ReasoningEffortConfig;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_kern::AuthManager;
use chaos_kern::OPENAI_DEFAULT_BASE_URL;
use chaos_kern::ProcessTable;
use chaos_locate::FileMatch;
use chaos_sudoers::ApprovalPreset;
use chaos_sudoers::builtin_approval_presets;
use jiff::Timestamp;
use strum::IntoEnumIterator;

const USER_SHELL_COMMAND_HELP_TITLE: &str = "Prefix a command with ! to run it locally";
const USER_SHELL_COMMAND_HELP_HINT: &str = "Example: !ls";
const DEFAULT_STATUS_LINE_ITEMS: [&str; 3] =
    ["model-with-reasoning", "context-remaining", "current-dir"];

const PLACEHOLDERS: [&str; 8] = [
    "Explain this codebase",
    "Summarize recent commits",
    "Implement {feature}",
    "Find and fix a bug in @filename",
    "Write tests for @filename",
    "Improve documentation in @filename",
    "Run /review on my current changes",
    "Explore /status to view session info",
];

/// Common initialization parameters shared by all `ChatWidget` constructors.
pub struct ChatWidgetInit {
    pub config: Config,
    pub frame_requester: FrameRequester,
    pub app_event_tx: AppEventSender,
    pub initial_user_message: Option<UserMessage>,
    pub enhanced_keys_supported: bool,
    pub auth_manager: Arc<AuthManager>,
    pub models_manager: Arc<ModelsManager>,
    pub is_first_run: bool,
    pub model: Option<String>,
    // Shared latch so we only warn once about invalid status-line item IDs.
    pub status_line_invalid_items_warned: Arc<AtomicBool>,
    pub session_telemetry: SessionTelemetry,
}

/// Maintains the per-session UI state and interaction state machines for the chat screen.
///
/// `ChatWidget` owns the state derived from the protocol event stream (history cells, streaming
/// buffers, bottom-pane overlays, and transient status text) and turns key presses into user
/// intent (`Op` submissions and `AppEvent` requests).
///
/// It is not responsible for running the agent itself; it reflects progress by updating UI state
/// and by sending requests back to chaos-kern.
///
/// Quit/interrupt behavior intentionally spans layers: the bottom pane owns local input routing
/// (which view gets Ctrl+C), while `ChatWidget` owns process-level decisions such as interrupting
/// active work, arming the double-press quit shortcut, and requesting shutdown-first exit.
pub struct ChatWidget {
    app_event_tx: AppEventSender,
    /// Submit-side handle to the kernel SQ. For session-backed paths the
    /// embedded drop guard is a no-op (the [`chaos_session::ClientSession`]
    /// owns its own DropGuard cascade); for the process-switch
    /// `spawn_op_forwarder` path the guard tears down the drain task and
    /// releases the kernel `Arc<Process>` when this `ChatWidget` is dropped.
    chaos_op_tx: chaos_session::OpForwarder,
    bottom_pane: BottomPane,
    active_cell: Option<Box<dyn HistoryCell>>,
    /// Monotonic-ish counter used to invalidate transcript overlay caching.
    ///
    /// The transcript overlay appends a cached "live tail" for the current active cell. Most
    /// active-cell updates are mutations of the *existing* cell (not a replacement), so pointer
    /// identity alone is not a good cache key.
    ///
    /// Callers bump this whenever the active cell's transcript output could change without
    /// flushing. It is intentionally allowed to wrap, which implies a rare one-time cache collision
    /// where the overlay may briefly treat new tail content as already cached.
    active_cell_revision: u64,
    config: Config,
    /// Session-local state to restore when `/clamp` is toggled off.
    pre_clamp_selection: Option<PreClampSelection>,
    /// The unmasked collaboration mode settings (always Default mode).
    ///
    /// Masks are applied on top of this base mode to derive the effective mode.
    current_collaboration_mode: CollaborationMode,
    /// The currently active collaboration mask, if any.
    active_collaboration_mask: Option<CollaborationModeMask>,
    auth_manager: Arc<AuthManager>,
    models_manager: Arc<ModelsManager>,
    session_telemetry: SessionTelemetry,
    session_header: SessionHeader,
    initial_user_message: Option<UserMessage>,
    token_info: Option<TokenUsageInfo>,
    rate_limit_snapshots_by_limit_id: BTreeMap<String, RateLimitSnapshotDisplay>,
    plan_type: Option<PlanType>,
    rate_limit_warnings: RateLimitWarningState,
    rate_limit_switch_prompt: RateLimitSwitchPromptState,
    adaptive_chunking: AdaptiveChunkingPolicy,
    // Stream lifecycle controller
    stream_controller: Option<StreamController>,
    // Stream lifecycle controller for proposed plan output.
    plan_stream_controller: Option<PlanStreamController>,
    // Latest completed user-visible Chaos output that `/copy` should place on the clipboard.
    last_copyable_output: Option<String>,
    running_commands: HashMap<String, RunningCommand>,
    pending_collab_spawn_requests: HashMap<String, multi_agents::SpawnRequestSummary>,
    suppressed_exec_calls: HashSet<String>,
    last_unified_wait: Option<UnifiedExecWaitState>,
    unified_exec_wait_streak: Option<UnifiedExecWaitStreak>,
    turn_sleep_inhibitor: SleepInhibitor,
    task_complete_pending: bool,
    unified_exec_processes: Vec<UnifiedExecProcessSummary>,
    /// Tracks whether chaos-kern currently considers an agent turn to be in progress.
    ///
    /// This is kept separate from `mcp_startup_status` so that MCP startup progress (or completion)
    /// can update the status header without accidentally clearing the spinner for an active turn.
    agent_turn_running: bool,
    /// Tracks per-server MCP startup state while startup is in progress.
    ///
    /// The map is `Some(_)` from the first `McpStartupUpdate` until `McpStartupComplete`, and the
    /// bottom pane is treated as "running" while this is populated, even if no agent turn is
    /// currently executing.
    mcp_startup_status: Option<HashMap<String, McpStartupStatus>>,
    connectors_cache: ConnectorsCacheState,
    connectors_partial_snapshot: Option<ConnectorsSnapshot>,
    connectors_prefetch_in_flight: bool,
    connectors_force_refetch_pending: bool,
    // Queue of interruptive UI events deferred during an active write cycle
    interrupts: InterruptManager,
    // Accumulates the current reasoning block text to extract a header
    reasoning_buffer: String,
    // Accumulates full reasoning content for transcript-only recording
    full_reasoning_buffer: String,
    // The currently rendered footer state. We keep the already-formatted
    // details here so transient stream interruptions can restore the footer
    // exactly as it was shown.
    current_status: StatusIndicatorState,

    // Previous status header to restore after a transient stream retry.
    retry_status_header: Option<String>,
    // Set when commentary output completes; once stream queues go idle we restore the status row.
    pending_status_indicator_restore: bool,
    suppress_queue_autosend: bool,
    process_id: Option<ProcessId>,
    process_name: Option<String>,
    forked_from: Option<ProcessId>,
    frame_requester: FrameRequester,
    // Whether to include the initial welcome banner on session configured
    show_welcome_banner: bool,
    // When resuming an existing session (selected via resume picker), avoid an
    // immediate redraw on SessionConfigured to prevent a gratuitous UI flicker.
    suppress_session_configured_redraw: bool,
    // User messages queued while a turn is in progress
    queued_user_messages: VecDeque<UserMessage>,
    // Steers already submitted to core but not yet committed into history.
    //
    // The bottom pane shows these above queued drafts until core records the
    // corresponding user message item.
    pending_steers: VecDeque<PendingSteer>,
    // When set, the next interrupt should resubmit all pending steers as one
    // fresh user turn instead of restoring them into the composer.
    submit_pending_steers_after_interrupt: bool,
    /// Terminal-appropriate keybinding for popping the most-recently queued
    /// message back into the composer.  Determined once at construction time via
    /// [`queued_message_edit_binding_for_terminal`] and propagated to
    /// `BottomPane` so the hint text matches the actual shortcut.
    queued_message_edit_binding: KeyBinding,
    // Pending notification to show when unfocused on next Draw
    pending_notification: Option<Notification>,
    /// When `Some`, the user has pressed a quit shortcut and the second press
    /// must occur before `quit_shortcut_expires_at`.
    quit_shortcut_expires_at: Option<Instant>,
    /// Tracks which quit shortcut key was pressed first.
    ///
    /// We require the second press to match this key so `Ctrl+C` followed by
    /// `Ctrl+D` (or vice versa) doesn't quit accidentally.
    quit_shortcut_key: Option<KeyBinding>,
    // Simple review mode flag; used to adjust layout and banners.
    is_review_mode: bool,
    // Snapshot of token usage to restore after review mode exits.
    pre_review_token_info: Option<Option<TokenUsageInfo>>,
    // Whether the next streamed assistant content should be preceded by a final message separator.
    //
    // This is set whenever we insert a visible history cell that conceptually belongs to a turn.
    // The separator itself is only rendered if the turn recorded "work" activity (see
    // `had_work_activity`).
    needs_final_message_separator: bool,
    // Whether the current turn performed "work" (exec commands, MCP tool calls, patch applications).
    //
    // This gates rendering of the "Worked for …" separator so purely conversational turns don't
    // show an empty divider. It is reset when the separator is emitted.
    had_work_activity: bool,
    // Whether the current turn emitted a plan update.
    saw_plan_update_this_turn: bool,
    // Whether the current turn emitted a proposed plan item that has not been superseded by a
    // later steer. This is cleared when the user submits a steer so the plan popup only appears
    // if a newer proposed plan arrives afterward.
    saw_plan_item_this_turn: bool,
    // Incremental buffer for streamed plan content.
    plan_delta_buffer: String,
    // True while a plan item is streaming.
    plan_item_active: bool,
    // Status-indicator elapsed seconds captured at the last emitted final-message separator.
    //
    // This lets the separator show per-chunk work time (since the previous separator) rather than
    // the total task-running time reported by the status indicator.
    last_separator_elapsed_secs: Option<u64>,
    // Runtime metrics accumulated across delta snapshots for the active turn.
    turn_runtime_metrics: RuntimeMetricsSummary,
    last_rendered_width: std::cell::Cell<Option<usize>>,
    // Current working directory (if known)
    current_cwd: Option<PathBuf>,
    // Runtime network proxy bind addresses from SessionConfigured.
    session_network_proxy: Option<chaos_ipc::protocol::SessionNetworkProxyRuntime>,
    // Shared latch so we only warn once about invalid status-line item IDs.
    status_line_invalid_items_warned: Arc<AtomicBool>,
    // Cached git branch name for the status line (None if unknown).
    status_line_branch: Option<String>,
    // CWD used to resolve the cached branch; change resets branch state.
    status_line_branch_cwd: Option<PathBuf>,
    // True while an async branch lookup is in flight.
    status_line_branch_pending: bool,
    // True once we've attempted a branch lookup for the current CWD.
    status_line_branch_lookup_complete: bool,
    external_editor_state: ExternalEditorState,
    last_rendered_user_message_event: Option<RenderedUserMessageEvent>,
}

impl ChatWidget {
    // --- Small event handlers ---

    fn emit_forked_process_event(&self, forked_from_id: ProcessId) {
        let app_event_tx = self.app_event_tx.clone();
        let chaos_home = self.config.chaos_home.clone();
        tokio::spawn(async move {
            let forked_from_id_text = forked_from_id.to_string();
            let send_name_and_id = |name: String| {
                let line: Line<'static> = vec![
                    "• ".dim(),
                    "Process forked from ".into(),
                    name.cyan(),
                    " (".into(),
                    forked_from_id_text.clone().cyan(),
                    ")".into(),
                ]
                .into();
                app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    PlainHistoryCell::new(vec![line]),
                )));
            };
            let send_id_only = || {
                let line: Line<'static> = vec![
                    "• ".dim(),
                    "Process forked from ".into(),
                    forked_from_id_text.clone().cyan(),
                ]
                .into();
                app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    PlainHistoryCell::new(vec![line]),
                )));
            };

            match find_process_name_by_id(&chaos_home, &forked_from_id).await {
                Ok(Some(name)) if !name.trim().is_empty() => {
                    send_name_and_id(name);
                }
                Ok(_) => send_id_only(),
                Err(err) => {
                    tracing::warn!("Failed to read forked process name: {err}");
                    send_id_only();
                }
            }
        });
    }

    pub fn open_app_link_view(&mut self, params: crate::bottom_pane::AppLinkViewParams) {
        let view = crate::bottom_pane::AppLinkView::new(params, self.app_event_tx.clone());
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }

    // Raw reasoning uses the same flow as summarized reasoning

    fn maybe_prompt_plan_implementation(&mut self) {
        if !self.collaboration_modes_enabled() {
            return;
        }
        if !self.queued_user_messages.is_empty() {
            return;
        }
        if self.active_mode_kind() != ModeKind::Plan {
            return;
        }
        if !self.saw_plan_item_this_turn {
            return;
        }
        if !self.bottom_pane.no_modal_or_popup_active() {
            return;
        }

        if matches!(
            self.rate_limit_switch_prompt,
            RateLimitSwitchPromptState::Pending
        ) {
            return;
        }

        self.open_plan_implementation_prompt();
    }

    fn open_plan_implementation_prompt(&mut self) {
        let default_mask = collaboration_modes::default_mode_mask(self.models_manager.as_ref());
        let (implement_actions, implement_disabled_reason) = match default_mask {
            Some(mask) => {
                let user_text = PLAN_IMPLEMENTATION_CODING_MESSAGE.to_string();
                let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                    tx.send(AppEvent::SubmitUserMessageWithMode {
                        text: user_text.clone(),
                        collaboration_mode: mask.clone(),
                    });
                })];
                (actions, None)
            }
            None => (Vec::new(), Some("Default mode unavailable".to_string())),
        };
        let items = vec![
            SelectionItem {
                name: PLAN_IMPLEMENTATION_YES.to_string(),
                description: Some("Switch to Default and start coding.".to_string()),
                selected_description: None,
                is_current: false,
                actions: implement_actions,
                disabled_reason: implement_disabled_reason,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: PLAN_IMPLEMENTATION_NO.to_string(),
                description: Some("Continue planning with the model.".to_string()),
                selected_description: None,
                is_current: false,
                actions: Vec::new(),
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(PLAN_IMPLEMENTATION_TITLE.to_string()),
            subtitle: None,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
        self.notify(Notification::PlanModePrompt {
            title: PLAN_IMPLEMENTATION_TITLE.to_string(),
        });
    }

    /// Merge pending steers, queued drafts, and the current composer state into a single message.
    ///
    /// Each pending message numbers attachments from `[Image #1]` relative to its own remote
    /// images. When we concatenate multiple messages after interrupt, we must renumber local-image
    /// placeholders in a stable order and rebase text element byte ranges so the restored composer
    /// state stays aligned with the merged attachment list. Returns `None` when there is nothing to
    /// restore.
    fn drain_pending_messages_for_restore(&mut self) -> Option<UserMessage> {
        if self.pending_steers.is_empty() && self.queued_user_messages.is_empty() {
            return None;
        }

        let existing_message = UserMessage {
            text: self.bottom_pane.composer_text(),
            text_elements: self.bottom_pane.composer_text_elements(),
            local_images: self.bottom_pane.composer_local_images(),
            remote_image_urls: self.bottom_pane.remote_image_urls(),
            mention_bindings: self.bottom_pane.composer_mention_bindings(),
        };

        let mut to_merge: Vec<UserMessage> = self
            .pending_steers
            .drain(..)
            .map(|steer| steer.user_message)
            .collect();
        to_merge.extend(self.queued_user_messages.drain(..));
        if !existing_message.text.is_empty()
            || !existing_message.local_images.is_empty()
            || !existing_message.remote_image_urls.is_empty()
        {
            to_merge.push(existing_message);
        }

        Some(merge_user_messages(to_merge))
    }

    fn restore_user_message_to_composer(&mut self, user_message: UserMessage) {
        let UserMessage {
            text,
            local_images,
            remote_image_urls,
            text_elements,
            mention_bindings,
        } = user_message;
        let local_image_paths = local_images.into_iter().map(|img| img.path).collect();
        self.set_remote_image_urls(remote_image_urls);
        self.bottom_pane.set_composer_text_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
        );
    }

    pub fn capture_process_input_state(&self) -> Option<ProcessInputState> {
        let composer = ProcessComposerState {
            text: self.bottom_pane.composer_text(),
            text_elements: self.bottom_pane.composer_text_elements(),
            local_images: self.bottom_pane.composer_local_images(),
            remote_image_urls: self.bottom_pane.remote_image_urls(),
            mention_bindings: self.bottom_pane.composer_mention_bindings(),
            pending_pastes: self.bottom_pane.composer_pending_pastes(),
        };
        Some(ProcessInputState {
            composer: composer.has_content().then_some(composer),
            pending_steers: self
                .pending_steers
                .iter()
                .map(|pending| pending.user_message.clone())
                .collect(),
            queued_user_messages: self.queued_user_messages.clone(),
            current_collaboration_mode: self.current_collaboration_mode.clone(),
            active_collaboration_mask: self.active_collaboration_mask.clone(),
            agent_turn_running: self.agent_turn_running,
        })
    }

    pub fn restore_process_input_state(&mut self, input_state: Option<ProcessInputState>) {
        if let Some(input_state) = input_state {
            self.current_collaboration_mode = input_state.current_collaboration_mode;
            self.active_collaboration_mask = input_state.active_collaboration_mask;
            self.agent_turn_running = input_state.agent_turn_running;
            self.update_collaboration_mode_indicator();
            self.refresh_model_display();
            if let Some(composer) = input_state.composer {
                let local_image_paths = composer
                    .local_images
                    .into_iter()
                    .map(|img| img.path)
                    .collect();
                self.set_remote_image_urls(composer.remote_image_urls);
                self.bottom_pane.set_composer_text_with_mention_bindings(
                    composer.text,
                    composer.text_elements,
                    local_image_paths,
                    composer.mention_bindings,
                );
                self.bottom_pane
                    .set_composer_pending_pastes(composer.pending_pastes);
            } else {
                self.set_remote_image_urls(Vec::new());
                self.bottom_pane.set_composer_text_with_mention_bindings(
                    String::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                );
                self.bottom_pane.set_composer_pending_pastes(Vec::new());
            }
            self.pending_steers.clear();
            self.queued_user_messages = input_state.pending_steers;
            self.queued_user_messages
                .extend(input_state.queued_user_messages);
        } else {
            self.agent_turn_running = false;
            self.pending_steers.clear();
            self.set_remote_image_urls(Vec::new());
            self.bottom_pane.set_composer_text_with_mention_bindings(
                String::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            self.bottom_pane.set_composer_pending_pastes(Vec::new());
            self.queued_user_messages.clear();
        }
        self.turn_sleep_inhibitor
            .set_turn_running(self.agent_turn_running);
        self.update_task_running_state();
        self.refresh_pending_input_preview();
        self.request_redraw();
    }

    pub fn set_queue_autosend_suppressed(&mut self, suppressed: bool) {
        self.suppress_queue_autosend = suppressed;
    }

    pub fn new(common: ChatWidgetInit, process_table: Arc<ProcessTable>) -> Self {
        let ChatWidgetInit {
            config,
            frame_requester,
            app_event_tx,
            initial_user_message,
            enhanced_keys_supported,
            auth_manager,
            models_manager,
            is_first_run,
            model,
            status_line_invalid_items_warned,
            session_telemetry,
        } = common;
        let model = model.filter(|m| !m.trim().is_empty());
        let mut config = config;
        config.model = model.clone();
        let prevent_idle_sleep = true;
        let mut rng = rand::rng();
        let placeholder = PLACEHOLDERS[rng.random_range(0..PLACEHOLDERS.len())].to_string();
        let chaos_op_tx = spawn_agent(config.clone(), app_event_tx.clone(), process_table);

        let model_override = model.as_deref();
        let model_for_header = model
            .clone()
            .unwrap_or_else(|| DEFAULT_MODEL_DISPLAY_NAME.to_string());
        let active_collaboration_mask =
            Self::initial_collaboration_mask(&config, models_manager.as_ref(), model_override);
        let header_model = active_collaboration_mask
            .as_ref()
            .and_then(|mask| mask.model.clone())
            .unwrap_or_else(|| model_for_header.clone());
        let fallback_default = Settings {
            model: header_model.clone(),
            reasoning_effort: None,
            minion_instructions: None,
        };
        // Collaboration modes start in Default mode.
        let current_collaboration_mode = CollaborationMode {
            mode: ModeKind::Default,
            settings: fallback_default,
        };

        let active_cell = Some(Self::placeholder_session_header_cell(&config));

        let current_cwd = Some(config.cwd.clone());
        let queued_message_edit_binding =
            queued_message_edit_binding_for_terminal(terminal_info().name);
        let mut widget = Self {
            app_event_tx: app_event_tx.clone(),
            frame_requester: frame_requester.clone(),
            chaos_op_tx,
            bottom_pane: BottomPane::new(BottomPaneParams {
                frame_requester,
                app_event_tx,
                has_input_focus: true,
                enhanced_keys_supported,
                placeholder_text: placeholder,
                disable_paste_burst: config.disable_paste_burst,
                animations_enabled: config.animations,
            }),
            active_cell,
            active_cell_revision: 0,
            config,
            pre_clamp_selection: None,
            current_collaboration_mode,
            active_collaboration_mask,
            auth_manager,
            models_manager,
            session_telemetry,
            session_header: SessionHeader::new(header_model),
            initial_user_message,
            token_info: None,
            rate_limit_snapshots_by_limit_id: BTreeMap::new(),
            plan_type: None,
            rate_limit_warnings: RateLimitWarningState::default(),
            rate_limit_switch_prompt: RateLimitSwitchPromptState::default(),
            adaptive_chunking: AdaptiveChunkingPolicy::default(),
            stream_controller: None,
            plan_stream_controller: None,
            last_copyable_output: None,
            running_commands: HashMap::new(),
            pending_collab_spawn_requests: HashMap::new(),
            suppressed_exec_calls: HashSet::new(),
            last_unified_wait: None,
            unified_exec_wait_streak: None,
            turn_sleep_inhibitor: SleepInhibitor::new(prevent_idle_sleep),
            task_complete_pending: false,
            unified_exec_processes: Vec::new(),
            agent_turn_running: false,
            mcp_startup_status: None,
            connectors_cache: ConnectorsCacheState::default(),
            connectors_partial_snapshot: None,
            connectors_prefetch_in_flight: false,
            connectors_force_refetch_pending: false,
            interrupts: InterruptManager::new(),
            reasoning_buffer: String::new(),
            full_reasoning_buffer: String::new(),
            current_status: StatusIndicatorState::working(),

            retry_status_header: None,
            pending_status_indicator_restore: false,
            suppress_queue_autosend: false,
            process_id: None,
            process_name: None,
            forked_from: None,
            queued_user_messages: VecDeque::new(),
            pending_steers: VecDeque::new(),
            submit_pending_steers_after_interrupt: false,
            queued_message_edit_binding,
            show_welcome_banner: is_first_run,
            suppress_session_configured_redraw: false,
            pending_notification: None,
            quit_shortcut_expires_at: None,
            quit_shortcut_key: None,
            is_review_mode: false,
            pre_review_token_info: None,
            needs_final_message_separator: false,
            had_work_activity: false,
            saw_plan_update_this_turn: false,
            saw_plan_item_this_turn: false,
            plan_delta_buffer: String::new(),
            plan_item_active: false,
            last_separator_elapsed_secs: None,
            turn_runtime_metrics: RuntimeMetricsSummary::default(),
            last_rendered_width: std::cell::Cell::new(None),
            current_cwd,
            session_network_proxy: None,
            status_line_invalid_items_warned,
            status_line_branch: None,
            status_line_branch_cwd: None,
            status_line_branch_pending: false,
            status_line_branch_lookup_complete: false,
            external_editor_state: ExternalEditorState::Closed,
            last_rendered_user_message_event: None,
        };

        widget
            .bottom_pane
            .set_status_line_enabled(!widget.configured_status_line_items().is_empty());
        widget
            .bottom_pane
            .set_collaboration_modes_enabled(/*enabled*/ true);
        widget.sync_personality_command_enabled();
        widget
            .bottom_pane
            .set_queued_message_edit_binding(widget.queued_message_edit_binding);
        widget.update_collaboration_mode_indicator();

        widget
            .bottom_pane
            .set_connectors_enabled(widget.connectors_enabled());

        widget
    }

    pub fn new_with_op_sender(
        common: ChatWidgetInit,
        chaos_op_tx: chaos_session::OpForwarder,
    ) -> Self {
        let ChatWidgetInit {
            config,
            frame_requester,
            app_event_tx,
            initial_user_message,
            enhanced_keys_supported,
            auth_manager,
            models_manager,
            is_first_run,
            model,
            status_line_invalid_items_warned,
            session_telemetry,
        } = common;
        let model = model.filter(|m| !m.trim().is_empty());
        let mut config = config;
        config.model = model.clone();
        let prevent_idle_sleep = true;
        let mut rng = rand::rng();
        let placeholder = PLACEHOLDERS[rng.random_range(0..PLACEHOLDERS.len())].to_string();

        let model_override = model.as_deref();
        let model_for_header = model
            .clone()
            .unwrap_or_else(|| DEFAULT_MODEL_DISPLAY_NAME.to_string());
        let active_collaboration_mask =
            Self::initial_collaboration_mask(&config, models_manager.as_ref(), model_override);
        let header_model = active_collaboration_mask
            .as_ref()
            .and_then(|mask| mask.model.clone())
            .unwrap_or_else(|| model_for_header.clone());
        let fallback_default = Settings {
            model: header_model.clone(),
            reasoning_effort: None,
            minion_instructions: None,
        };
        // Collaboration modes start in Default mode.
        let current_collaboration_mode = CollaborationMode {
            mode: ModeKind::Default,
            settings: fallback_default,
        };

        let active_cell = Some(Self::placeholder_session_header_cell(&config));
        let current_cwd = Some(config.cwd.clone());

        let queued_message_edit_binding =
            queued_message_edit_binding_for_terminal(terminal_info().name);
        let mut widget = Self {
            app_event_tx: app_event_tx.clone(),
            frame_requester: frame_requester.clone(),
            chaos_op_tx,
            bottom_pane: BottomPane::new(BottomPaneParams {
                frame_requester,
                app_event_tx,
                has_input_focus: true,
                enhanced_keys_supported,
                placeholder_text: placeholder,
                disable_paste_burst: config.disable_paste_burst,
                animations_enabled: config.animations,
            }),
            active_cell,
            active_cell_revision: 0,
            config,
            pre_clamp_selection: None,
            current_collaboration_mode,
            active_collaboration_mask,
            auth_manager,
            models_manager,
            session_telemetry,
            session_header: SessionHeader::new(header_model),
            initial_user_message,
            token_info: None,
            rate_limit_snapshots_by_limit_id: BTreeMap::new(),
            plan_type: None,
            rate_limit_warnings: RateLimitWarningState::default(),
            rate_limit_switch_prompt: RateLimitSwitchPromptState::default(),
            adaptive_chunking: AdaptiveChunkingPolicy::default(),
            stream_controller: None,
            plan_stream_controller: None,
            last_copyable_output: None,
            running_commands: HashMap::new(),
            pending_collab_spawn_requests: HashMap::new(),
            suppressed_exec_calls: HashSet::new(),
            last_unified_wait: None,
            unified_exec_wait_streak: None,
            turn_sleep_inhibitor: SleepInhibitor::new(prevent_idle_sleep),
            task_complete_pending: false,
            unified_exec_processes: Vec::new(),
            agent_turn_running: false,
            mcp_startup_status: None,
            connectors_cache: ConnectorsCacheState::default(),
            connectors_partial_snapshot: None,
            connectors_prefetch_in_flight: false,
            connectors_force_refetch_pending: false,
            interrupts: InterruptManager::new(),
            reasoning_buffer: String::new(),
            full_reasoning_buffer: String::new(),
            current_status: StatusIndicatorState::working(),

            retry_status_header: None,
            pending_status_indicator_restore: false,
            suppress_queue_autosend: false,
            process_id: None,
            process_name: None,
            forked_from: None,
            saw_plan_update_this_turn: false,
            saw_plan_item_this_turn: false,
            plan_delta_buffer: String::new(),
            plan_item_active: false,
            queued_user_messages: VecDeque::new(),
            pending_steers: VecDeque::new(),
            submit_pending_steers_after_interrupt: false,
            queued_message_edit_binding,
            show_welcome_banner: is_first_run,
            suppress_session_configured_redraw: false,
            pending_notification: None,
            quit_shortcut_expires_at: None,
            quit_shortcut_key: None,
            is_review_mode: false,
            pre_review_token_info: None,
            needs_final_message_separator: false,
            had_work_activity: false,
            last_separator_elapsed_secs: None,
            turn_runtime_metrics: RuntimeMetricsSummary::default(),
            last_rendered_width: std::cell::Cell::new(None),
            current_cwd,
            session_network_proxy: None,
            status_line_invalid_items_warned,
            status_line_branch: None,
            status_line_branch_cwd: None,
            status_line_branch_pending: false,
            status_line_branch_lookup_complete: false,
            external_editor_state: ExternalEditorState::Closed,
            last_rendered_user_message_event: None,
        };

        widget
            .bottom_pane
            .set_status_line_enabled(!widget.configured_status_line_items().is_empty());
        widget
            .bottom_pane
            .set_collaboration_modes_enabled(/*enabled*/ true);
        widget.sync_personality_command_enabled();
        widget
            .bottom_pane
            .set_queued_message_edit_binding(widget.queued_message_edit_binding);
        widget
            .bottom_pane
            .set_connectors_enabled(widget.connectors_enabled());

        widget
    }

    /// Create a ChatWidget attached to an existing conversation (e.g., a fork).
    pub fn new_from_existing(
        common: ChatWidgetInit,
        conversation: std::sync::Arc<chaos_kern::Process>,
        session_configured: chaos_ipc::protocol::SessionConfiguredEvent,
    ) -> Self {
        let ChatWidgetInit {
            config,
            frame_requester,
            app_event_tx,
            initial_user_message,
            enhanced_keys_supported,
            auth_manager,
            models_manager,
            is_first_run: _,
            model,
            status_line_invalid_items_warned,
            session_telemetry,
        } = common;
        let model = model.filter(|m| !m.trim().is_empty());
        let prevent_idle_sleep = true;
        let mut rng = rand::rng();
        let placeholder = PLACEHOLDERS[rng.random_range(0..PLACEHOLDERS.len())].to_string();

        let model_override = model.as_deref();
        let header_model = model
            .clone()
            .unwrap_or_else(|| session_configured.model.clone());
        let active_collaboration_mask =
            Self::initial_collaboration_mask(&config, models_manager.as_ref(), model_override);
        let header_model = active_collaboration_mask
            .as_ref()
            .and_then(|mask| mask.model.clone())
            .unwrap_or(header_model);

        let current_cwd = Some(session_configured.cwd.clone());
        let chaos_op_tx =
            spawn_agent_from_existing(conversation, session_configured, app_event_tx.clone());

        let fallback_default = Settings {
            model: header_model.clone(),
            reasoning_effort: None,
            minion_instructions: None,
        };
        // Collaboration modes start in Default mode.
        let current_collaboration_mode = CollaborationMode {
            mode: ModeKind::Default,
            settings: fallback_default,
        };

        let queued_message_edit_binding =
            queued_message_edit_binding_for_terminal(terminal_info().name);
        let mut widget = Self {
            app_event_tx: app_event_tx.clone(),
            frame_requester: frame_requester.clone(),
            chaos_op_tx,
            bottom_pane: BottomPane::new(BottomPaneParams {
                frame_requester,
                app_event_tx,
                has_input_focus: true,
                enhanced_keys_supported,
                placeholder_text: placeholder,
                disable_paste_burst: config.disable_paste_burst,
                animations_enabled: config.animations,
            }),
            active_cell: None,
            active_cell_revision: 0,
            config,
            pre_clamp_selection: None,
            current_collaboration_mode,
            active_collaboration_mask,
            auth_manager,
            models_manager,
            session_telemetry,
            session_header: SessionHeader::new(header_model),
            initial_user_message,
            token_info: None,
            rate_limit_snapshots_by_limit_id: BTreeMap::new(),
            plan_type: None,
            rate_limit_warnings: RateLimitWarningState::default(),
            rate_limit_switch_prompt: RateLimitSwitchPromptState::default(),
            adaptive_chunking: AdaptiveChunkingPolicy::default(),
            stream_controller: None,
            plan_stream_controller: None,
            last_copyable_output: None,
            running_commands: HashMap::new(),
            pending_collab_spawn_requests: HashMap::new(),
            suppressed_exec_calls: HashSet::new(),
            last_unified_wait: None,
            unified_exec_wait_streak: None,
            turn_sleep_inhibitor: SleepInhibitor::new(prevent_idle_sleep),
            task_complete_pending: false,
            unified_exec_processes: Vec::new(),
            agent_turn_running: false,
            mcp_startup_status: None,
            connectors_cache: ConnectorsCacheState::default(),
            connectors_partial_snapshot: None,
            connectors_prefetch_in_flight: false,
            connectors_force_refetch_pending: false,
            interrupts: InterruptManager::new(),
            reasoning_buffer: String::new(),
            full_reasoning_buffer: String::new(),
            current_status: StatusIndicatorState::working(),

            retry_status_header: None,
            pending_status_indicator_restore: false,
            suppress_queue_autosend: false,
            process_id: None,
            process_name: None,
            forked_from: None,
            queued_user_messages: VecDeque::new(),
            pending_steers: VecDeque::new(),
            submit_pending_steers_after_interrupt: false,
            queued_message_edit_binding,
            show_welcome_banner: false,
            suppress_session_configured_redraw: true,
            pending_notification: None,
            quit_shortcut_expires_at: None,
            quit_shortcut_key: None,
            is_review_mode: false,
            pre_review_token_info: None,
            needs_final_message_separator: false,
            had_work_activity: false,
            saw_plan_update_this_turn: false,
            saw_plan_item_this_turn: false,
            plan_delta_buffer: String::new(),
            plan_item_active: false,
            last_separator_elapsed_secs: None,
            turn_runtime_metrics: RuntimeMetricsSummary::default(),
            last_rendered_width: std::cell::Cell::new(None),
            current_cwd,
            session_network_proxy: None,
            status_line_invalid_items_warned,
            status_line_branch: None,
            status_line_branch_cwd: None,
            status_line_branch_pending: false,
            status_line_branch_lookup_complete: false,
            external_editor_state: ExternalEditorState::Closed,
            last_rendered_user_message_event: None,
        };

        widget
            .bottom_pane
            .set_status_line_enabled(!widget.configured_status_line_items().is_empty());
        widget
            .bottom_pane
            .set_collaboration_modes_enabled(/*enabled*/ true);
        widget.sync_personality_command_enabled();
        widget
            .bottom_pane
            .set_queued_message_edit_binding(widget.queued_message_edit_binding);
        widget.update_collaboration_mode_indicator();
        widget
            .bottom_pane
            .set_connectors_enabled(widget.connectors_enabled());

        widget
    }

    pub fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                kind: KeyEventKind::Press,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && c.eq_ignore_ascii_case(&'c') => {
                self.on_ctrl_c();
                return;
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                kind: KeyEventKind::Press,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && c.eq_ignore_ascii_case(&'d') => {
                if self.on_ctrl_d() {
                    return;
                }
                self.bottom_pane.clear_quit_shortcut_hint();
                self.quit_shortcut_expires_at = None;
                self.quit_shortcut_key = None;
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                kind: KeyEventKind::Press,
                ..
            } if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && c.eq_ignore_ascii_case(&'v') =>
            {
                match paste_image_to_temp_png() {
                    Ok((path, info)) => {
                        tracing::debug!(
                            "pasted image size={}x{} format={}",
                            info.width,
                            info.height,
                            info.encoded_format.label()
                        );
                        self.attach_image(path);
                    }
                    Err(err) => {
                        tracing::warn!("failed to paste image: {err}");
                        self.add_to_history(history_cell::new_error_event(format!(
                            "Failed to paste image: {err}",
                        )));
                    }
                }
                return;
            }
            other if other.kind == KeyEventKind::Press => {
                self.bottom_pane.clear_quit_shortcut_hint();
                self.quit_shortcut_expires_at = None;
                self.quit_shortcut_key = None;
            }
            _ => {}
        }

        if key_event.kind == KeyEventKind::Press
            && self.queued_message_edit_binding.is_press(key_event)
            && !self.queued_user_messages.is_empty()
        {
            if let Some(user_message) = self.queued_user_messages.pop_back() {
                self.restore_user_message_to_composer(user_message);
                self.refresh_pending_input_preview();
                self.request_redraw();
            }
            return;
        }

        if matches!(key_event.code, KeyCode::Esc)
            && matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && !self.pending_steers.is_empty()
            && self.bottom_pane.is_task_running()
            && self.bottom_pane.no_modal_or_popup_active()
        {
            self.submit_pending_steers_after_interrupt = true;
            if !self.submit_op(Op::Interrupt) {
                self.submit_pending_steers_after_interrupt = false;
            }
            return;
        }

        match key_event {
            KeyEvent {
                code: KeyCode::BackTab,
                kind: KeyEventKind::Press,
                ..
            } if self.collaboration_modes_enabled()
                && !self.bottom_pane.is_task_running()
                && self.bottom_pane.no_modal_or_popup_active() =>
            {
                self.cycle_collaboration_mode();
            }
            _ => match self.bottom_pane.handle_key_event(key_event) {
                InputResult::Submitted {
                    text,
                    text_elements,
                } => {
                    let local_images = self
                        .bottom_pane
                        .take_recent_submission_images_with_placeholders();
                    let remote_image_urls = self.take_remote_image_urls();
                    let user_message = UserMessage {
                        text,
                        local_images,
                        remote_image_urls,
                        text_elements,
                        mention_bindings: self
                            .bottom_pane
                            .take_recent_submission_mention_bindings(),
                    };
                    if user_message.text.is_empty()
                        && user_message.local_images.is_empty()
                        && user_message.remote_image_urls.is_empty()
                    {
                        return;
                    }
                    let should_submit_now =
                        self.is_session_configured() && !self.is_plan_streaming_in_tui();
                    if should_submit_now {
                        // Submitted is emitted when user submits.
                        // Reset any reasoning header only when we are actually submitting a turn.
                        self.reasoning_buffer.clear();
                        self.full_reasoning_buffer.clear();
                        self.set_status_header(String::from("Working"));
                        self.submit_user_message(user_message);
                    } else {
                        self.queue_user_message(user_message);
                    }
                }
                InputResult::Queued {
                    text,
                    text_elements,
                } => {
                    let local_images = self
                        .bottom_pane
                        .take_recent_submission_images_with_placeholders();
                    let remote_image_urls = self.take_remote_image_urls();
                    let user_message = UserMessage {
                        text,
                        local_images,
                        remote_image_urls,
                        text_elements,
                        mention_bindings: self
                            .bottom_pane
                            .take_recent_submission_mention_bindings(),
                    };
                    self.queue_user_message(user_message);
                }
                InputResult::Command(cmd) => {
                    self.dispatch_command(cmd);
                }
                InputResult::CommandWithArgs(cmd, args, text_elements) => {
                    self.dispatch_command_with_args(cmd, args, text_elements);
                }
                InputResult::None => {}
            },
        }
    }

    /// Attach a local image to the composer when the active model supports image inputs.
    ///
    /// When the model does not advertise image support, we keep the draft unchanged and surface a
    /// warning event so users can switch models or remove attachments.
    pub fn attach_image(&mut self, path: PathBuf) {
        if !self.current_model_supports_images() {
            self.add_to_history(history_cell::new_warning_event(
                self.image_inputs_not_supported_message(),
            ));
            self.request_redraw();
            return;
        }
        tracing::info!("attach_image path={path:?}");
        self.bottom_pane.attach_image(path);
        self.request_redraw();
    }

    pub fn composer_text_with_pending(&self) -> String {
        self.bottom_pane.composer_text_with_pending()
    }

    pub fn apply_external_edit(&mut self, text: String) {
        self.bottom_pane.apply_external_edit(text);
        self.request_redraw();
    }

    pub fn external_editor_state(&self) -> ExternalEditorState {
        self.external_editor_state
    }

    pub fn set_external_editor_state(&mut self, state: ExternalEditorState) {
        self.external_editor_state = state;
    }

    pub fn set_footer_hint_override(&mut self, items: Option<Vec<(String, String)>>) {
        self.bottom_pane.set_footer_hint_override(items);
    }

    pub fn show_selection_view(&mut self, params: SelectionViewParams) {
        self.bottom_pane.show_selection_view(params);
        self.request_redraw();
    }

    pub fn no_modal_or_popup_active(&self) -> bool {
        self.bottom_pane.no_modal_or_popup_active()
    }

    pub fn can_launch_external_editor(&self) -> bool {
        self.bottom_pane.can_launch_external_editor()
    }

    pub fn can_run_ctrl_l_clear_now(&mut self) -> bool {
        // Ctrl+L is not a slash command, but it follows /clear's rule:
        // block while a task is running.
        if !self.bottom_pane.is_task_running() {
            return true;
        }

        let message = "Ctrl+L is disabled while a task is in progress.".to_string();
        self.add_to_history(history_cell::new_error_event(message));
        self.request_redraw();
        false
    }

    fn dispatch_command(&mut self, cmd: SlashCommand) {
        if !cmd.available_during_task() && self.bottom_pane.is_task_running() {
            let message = format!(
                "'/{}' is disabled while a task is in progress.",
                cmd.command()
            );
            self.add_to_history(history_cell::new_error_event(message));
            self.bottom_pane.drain_pending_submission_state();
            self.request_redraw();
            return;
        }
        match cmd {
            SlashCommand::New => {
                self.app_event_tx.send(AppEvent::NewSession);
            }
            SlashCommand::Clear => {
                self.app_event_tx.send(AppEvent::ClearUi);
            }
            SlashCommand::Resume => {
                self.app_event_tx.send(AppEvent::OpenResumePicker);
            }
            SlashCommand::Fork => {
                self.app_event_tx.send(AppEvent::ForkCurrentSession);
            }
            SlashCommand::Init => {
                let init_target = self.config.cwd.join(DEFAULT_PROJECT_DOC_FILENAME);
                if init_target.exists() {
                    let message = format!(
                        "{DEFAULT_PROJECT_DOC_FILENAME} already exists here. Skipping /init to avoid overwriting it."
                    );
                    self.add_info_message(message, /*hint*/ None);
                    return;
                }
                const INIT_PROMPT: &str = include_str!("prompt_for_init_command.md");
                self.submit_user_message(INIT_PROMPT.to_string().into());
            }
            SlashCommand::Compact => {
                self.clear_token_usage();
                self.app_event_tx.send(AppEvent::ChaosOp(Op::Compact));
            }
            SlashCommand::Review => {
                self.open_review_popup();
            }
            SlashCommand::Rename => {
                self.session_telemetry
                    .counter("chaos.process.rename", /*inc*/ 1, &[]);
                self.show_rename_prompt();
            }
            SlashCommand::Model => {
                self.open_model_popup();
            }
            SlashCommand::Personality => {
                self.open_personality_popup();
            }
            SlashCommand::Plan => {
                if !self.collaboration_modes_enabled() {
                    self.add_info_message(
                        "Collaboration modes are disabled.".to_string(),
                        Some("Enable collaboration modes to use /plan.".to_string()),
                    );
                    return;
                }
                if let Some(mask) = collaboration_modes::plan_mask(self.models_manager.as_ref()) {
                    self.set_collaboration_mask(mask);
                } else {
                    self.add_info_message(
                        "Plan mode unavailable right now.".to_string(),
                        /*hint*/ None,
                    );
                }
            }
            SlashCommand::Collab => {
                if !self.collaboration_modes_enabled() {
                    self.add_info_message(
                        "Collaboration modes are disabled.".to_string(),
                        Some("Enable collaboration modes to use /collab.".to_string()),
                    );
                    return;
                }
                self.open_collaboration_modes_popup();
            }
            SlashCommand::Agent | SlashCommand::MultiAgents => {
                self.app_event_tx.send(AppEvent::OpenAgentPicker);
            }
            SlashCommand::Approvals => {
                self.open_permissions_popup();
            }
            SlashCommand::Permissions => {
                self.open_permissions_popup();
            }
            SlashCommand::ElevateSandbox => {
                // Not supported on Linux.
            }
            SlashCommand::SandboxReadRoot => {
                self.add_error_message(
                    "Usage: /sandbox-add-read-dir <absolute-directory-path>".to_string(),
                );
            }
            SlashCommand::Quit | SlashCommand::Exit => {
                self.request_quit_without_confirmation();
            }
            SlashCommand::Login => {
                self.app_event_tx.send(AppEvent::OpenLoginPopup);
            }
            SlashCommand::Logout => {
                if let Err(e) = chaos_kern::auth::logout(
                    &self.config.chaos_home,
                    self.config.cli_auth_credentials_store_mode,
                ) {
                    tracing::error!("failed to logout: {e}");
                }
                self.request_quit_without_confirmation();
            }
            // SlashCommand::Undo => {
            //     self.app_event_tx.send(AppEvent::ChaosOp(Op::Undo));
            // }
            SlashCommand::Diff => {
                self.add_diff_in_progress();
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let text = match get_git_diff().await {
                        Ok((is_git_repo, diff_text)) => {
                            if is_git_repo {
                                diff_text
                            } else {
                                "`/diff` — _not inside a git repository_".to_string()
                            }
                        }
                        Err(e) => format!("Failed to compute diff: {e}"),
                    };
                    tx.send(AppEvent::DiffResult(text));
                });
            }
            SlashCommand::Copy => {
                let Some(text) = self.last_copyable_output.as_deref() else {
                    self.add_info_message(
                        "`/copy` is unavailable before the first Chaos output or right after a rollback."
                            .to_string(),
                        /*hint*/ None,
                    );
                    return;
                };

                let copy_result = clipboard_text::copy_text_to_clipboard(text);

                match copy_result {
                    Ok(()) => {
                        let hint = self.agent_turn_running.then_some(
                            "Current turn is still running; copied the latest completed output (not the in-progress response)."
                                .to_string(),
                        );
                        self.add_info_message(
                            "Copied latest Chaos output to clipboard.".to_string(),
                            hint,
                        );
                    }
                    Err(err) => {
                        self.add_error_message(format!("Failed to copy to clipboard: {err}"))
                    }
                }
            }
            SlashCommand::Mention => {
                self.insert_str("@");
            }
            SlashCommand::Status => {
                self.add_status_output();
            }
            SlashCommand::DebugConfig => {
                self.add_debug_config_output();
            }
            SlashCommand::Statusline => {
                self.open_status_line_setup();
            }
            SlashCommand::Theme => {
                self.open_theme_picker();
            }
            SlashCommand::Ps => {
                self.add_ps_output();
            }
            SlashCommand::Stop => {
                self.clean_background_terminals();
            }
            SlashCommand::MemoryDrop => {
                self.submit_op(Op::DropMemories);
            }
            SlashCommand::MemoryUpdate => {
                self.submit_op(Op::UpdateMemories);
            }
            SlashCommand::Mcp => {
                self.add_mcp_output();
            }
            SlashCommand::McpAdd => {
                self.open_mcp_add_form();
            }
            SlashCommand::Tools => {
                self.submit_op(Op::ListAllTools);
            }
            SlashCommand::Clamp => {
                // Toggle clamped mode — Claude Code subprocess as transport.
                let is_clamped = crate::theme::is_clamped();
                let new_state = !is_clamped;
                crate::theme::set_clamped(new_state);
                self.app_event_tx
                    .send(AppEvent::ChaosOp(Op::SetClamped { enabled: new_state }));
                if new_state {
                    // Save the current direct-API selection before clamping so we can
                    // restore it when switching transports back off.
                    self.capture_pre_clamp_selection();
                    self.add_info_message(
                        "Clamped: using Claude Code MAX subscription as transport.".to_string(),
                        Some("Type /clamp again to switch back".to_string()),
                    );
                } else {
                    self.restore_pre_clamp_selection();
                    self.add_info_message(
                        "Unclamped: using direct API transport.".to_string(),
                        None,
                    );
                    // Force a full screen repaint so the green theme takes effect
                    // on all chrome (borders, status bar, input prompt).
                }
            }
            SlashCommand::TestApproval => {
                use chaos_ipc::protocol::EventMsg;
                use std::collections::HashMap;

                use chaos_ipc::protocol::ApplyPatchApprovalRequestEvent;
                use chaos_ipc::protocol::FileChange;

                self.app_event_tx.send(AppEvent::ChaosEvent(Event {
                    id: "1".to_string(),
                    // msg: EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
                    //     call_id: "1".to_string(),
                    //     command: vec!["git".into(), "apply".into()],
                    //     cwd: self.config.cwd.clone(),
                    //     reason: Some("test".to_string()),
                    // }),
                    msg: EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
                        call_id: "1".to_string(),
                        turn_id: "turn-1".to_string(),
                        changes: HashMap::from([
                            (
                                PathBuf::from("/tmp/test.txt"),
                                FileChange::Add {
                                    content: "test".to_string(),
                                },
                            ),
                            (
                                PathBuf::from("/tmp/test2.txt"),
                                FileChange::Update {
                                    unified_diff: "+test\n-test2".to_string(),
                                    move_path: None,
                                },
                            ),
                        ]),
                        reason: None,
                        grant_root: Some(PathBuf::from("/tmp")),
                    }),
                }));
            }
        }
    }

    fn dispatch_command_with_args(
        &mut self,
        cmd: SlashCommand,
        args: String,
        _text_elements: Vec<TextElement>,
    ) {
        if !cmd.supports_inline_args() {
            self.dispatch_command(cmd);
            return;
        }
        if !cmd.available_during_task() && self.bottom_pane.is_task_running() {
            let message = format!(
                "'/{}' is disabled while a task is in progress.",
                cmd.command()
            );
            self.add_to_history(history_cell::new_error_event(message));
            self.request_redraw();
            return;
        }

        let trimmed = args.trim();
        match cmd {
            SlashCommand::Rename if !trimmed.is_empty() => {
                self.session_telemetry
                    .counter("chaos.process.rename", /*inc*/ 1, &[]);
                let Some((prepared_args, _prepared_elements)) = self
                    .bottom_pane
                    .prepare_inline_args_submission(/*record_history*/ false)
                else {
                    return;
                };
                let Some(name) = chaos_kern::util::normalize_process_name(&prepared_args) else {
                    self.add_error_message("Process name cannot be empty.".to_string());
                    return;
                };
                let cell = Self::rename_confirmation_cell(&name, self.process_id);
                self.add_boxed_history(Box::new(cell));
                self.request_redraw();
                self.app_event_tx
                    .send(AppEvent::ChaosOp(Op::SetProcessName { name }));
                self.bottom_pane.drain_pending_submission_state();
            }
            SlashCommand::Plan if !trimmed.is_empty() => {
                self.dispatch_command(cmd);
                if self.active_mode_kind() != ModeKind::Plan {
                    return;
                }
                let Some((prepared_args, prepared_elements)) = self
                    .bottom_pane
                    .prepare_inline_args_submission(/*record_history*/ true)
                else {
                    return;
                };
                let local_images = self
                    .bottom_pane
                    .take_recent_submission_images_with_placeholders();
                let remote_image_urls = self.take_remote_image_urls();
                let user_message = UserMessage {
                    text: prepared_args,
                    local_images,
                    remote_image_urls,
                    text_elements: prepared_elements,
                    mention_bindings: self.bottom_pane.take_recent_submission_mention_bindings(),
                };
                if self.is_session_configured() {
                    self.reasoning_buffer.clear();
                    self.full_reasoning_buffer.clear();
                    self.set_status_header(String::from("Working"));
                    self.submit_user_message(user_message);
                } else {
                    self.queue_user_message(user_message);
                }
            }
            SlashCommand::Review if !trimmed.is_empty() => {
                let Some((prepared_args, _prepared_elements)) = self
                    .bottom_pane
                    .prepare_inline_args_submission(/*record_history*/ false)
                else {
                    return;
                };
                self.submit_op(Op::Review {
                    review_request: ReviewRequest {
                        target: ReviewTarget::Custom {
                            instructions: prepared_args,
                        },
                        user_facing_hint: None,
                    },
                });
                self.bottom_pane.drain_pending_submission_state();
            }
            _ => self.dispatch_command(cmd),
        }
    }

    fn show_rename_prompt(&mut self) {
        let tx = self.app_event_tx.clone();
        let has_name = self
            .process_name
            .as_ref()
            .is_some_and(|name| !name.is_empty());
        let title = if has_name {
            "Rename process"
        } else {
            "Name process"
        };
        let process_id = self.process_id;
        let view = CustomPromptView::new(
            title.to_string(),
            "Type a name and press Enter".to_string(),
            /*context_label*/ None,
            Box::new(move |name: String| {
                let Some(name) = chaos_kern::util::normalize_process_name(&name) else {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event("Process name cannot be empty.".to_string()),
                    )));
                    return;
                };
                let cell = Self::rename_confirmation_cell(&name, process_id);
                tx.send(AppEvent::InsertHistoryCell(Box::new(cell)));
                tx.send(AppEvent::ChaosOp(Op::SetProcessName { name }));
            }),
        );

        self.bottom_pane.show_view(Box::new(view));
    }

    pub fn handle_paste(&mut self, text: String) {
        self.bottom_pane.handle_paste(text);
    }

    // Returns true if caller should skip rendering this frame (a future frame is scheduled).
    pub fn handle_paste_burst_tick(&mut self, frame_requester: FrameRequester) -> bool {
        if self.bottom_pane.flush_paste_burst_if_due() {
            // A paste just flushed; request an immediate redraw and skip this frame.
            self.request_redraw();
            true
        } else if self.bottom_pane.is_in_paste_burst() {
            // While capturing a burst, schedule a follow-up tick and skip this frame
            // to avoid redundant renders between ticks.
            frame_requester.schedule_frame_in(
                crate::bottom_pane::ChatComposer::recommended_paste_flush_delay(),
            );
            true
        } else {
            false
        }
    }

    fn flush_active_cell(&mut self) {
        if let Some(active) = self.active_cell.take() {
            self.needs_final_message_separator = true;
            self.app_event_tx.send(AppEvent::InsertHistoryCell(active));
        }
    }

    pub fn add_to_history(&mut self, cell: impl HistoryCell + 'static) {
        self.add_boxed_history(Box::new(cell));
    }

    fn add_boxed_history(&mut self, cell: Box<dyn HistoryCell>) {
        if !cell.display_lines(u16::MAX).is_empty() {
            // Only break exec grouping if the cell renders visible lines.
            self.flush_active_cell();
            self.needs_final_message_separator = true;
        }
        self.app_event_tx.send(AppEvent::InsertHistoryCell(cell));
    }

    fn queue_user_message(&mut self, user_message: UserMessage) {
        if !self.is_session_configured()
            || self.bottom_pane.is_task_running()
            || self.is_review_mode
        {
            self.queued_user_messages.push_back(user_message);
            self.refresh_pending_input_preview();
        } else {
            self.submit_user_message(user_message);
        }
    }

    fn submit_user_message(&mut self, user_message: UserMessage) {
        if !self.is_session_configured() {
            tracing::warn!("cannot submit user message before session is configured; queueing");
            self.queued_user_messages.push_front(user_message);
            self.refresh_pending_input_preview();
            return;
        }
        if self.is_review_mode {
            self.queued_user_messages.push_back(user_message);
            self.refresh_pending_input_preview();
            return;
        }

        let UserMessage {
            text,
            local_images,
            remote_image_urls,
            text_elements,
            mention_bindings,
        } = user_message;
        if text.is_empty() && local_images.is_empty() && remote_image_urls.is_empty() {
            return;
        }
        if (!local_images.is_empty() || !remote_image_urls.is_empty())
            && !self.current_model_supports_images()
        {
            self.restore_blocked_image_submission(
                text,
                text_elements,
                local_images,
                mention_bindings,
                remote_image_urls,
            );
            return;
        }

        let render_in_history = !self.agent_turn_running;
        let mut items: Vec<UserInput> = Vec::new();

        // Special-case: "!cmd" executes a local shell command instead of sending to the model.
        if let Some(stripped) = text.strip_prefix('!') {
            let cmd = stripped.trim();
            if cmd.is_empty() {
                self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_info_event(
                        USER_SHELL_COMMAND_HELP_TITLE.to_string(),
                        Some(USER_SHELL_COMMAND_HELP_HINT.to_string()),
                    ),
                )));
                return;
            }
            self.submit_op(Op::RunUserShellCommand {
                command: cmd.to_string(),
            });
            return;
        }

        for image_url in &remote_image_urls {
            items.push(UserInput::Image {
                image_url: image_url.clone(),
            });
        }

        for image in &local_images {
            items.push(UserInput::LocalImage {
                path: image.path.clone(),
            });
        }

        if !text.is_empty() {
            items.push(UserInput::Text {
                text: text.clone(),
                text_elements: text_elements.clone(),
            });
        }

        let mut selected_app_ids: HashSet<String> = HashSet::new();
        if let Some(apps) = self.connectors_for_mentions() {
            for binding in &mention_bindings {
                let Some(app_id) = binding
                    .path
                    .strip_prefix("app://")
                    .filter(|id| !id.is_empty())
                else {
                    continue;
                };
                if !selected_app_ids.insert(app_id.to_string()) {
                    continue;
                }
                if let Some(app) = apps.iter().find(|app| app.id == app_id && app.is_enabled) {
                    items.push(UserInput::Mention {
                        name: app.name.clone(),
                        path: binding.path.clone(),
                    });
                }
            }
        }

        let effective_mode = self.effective_collaboration_mode();
        let collaboration_mode = if self.collaboration_modes_enabled() {
            self.active_collaboration_mask
                .as_ref()
                .map(|_| effective_mode.clone())
        } else {
            None
        };
        let pending_steer = (!render_in_history).then(|| PendingSteer {
            user_message: UserMessage {
                text: text.clone(),
                local_images: local_images.clone(),
                remote_image_urls: remote_image_urls.clone(),
                text_elements: text_elements.clone(),
                mention_bindings: mention_bindings.clone(),
            },
            compare_key: Self::pending_steer_compare_key_from_items(&items),
        });
        let personality = self
            .config
            .personality
            .filter(|_| self.current_model_supports_personality());
        let service_tier = self.config.service_tier.map(Some);
        let op = Op::UserTurn {
            items,
            cwd: self.config.cwd.clone(),
            approval_policy: self.config.permissions.approval_policy.value(),
            sandbox_policy: self.config.permissions.sandbox_policy.get().clone(),
            model: effective_mode.model().to_string(),
            effort: effective_mode.reasoning_effort(),
            summary: None,
            service_tier,
            final_output_json_schema: None,
            collaboration_mode,
            personality,
        };

        if !self.submit_op(op) {
            return;
        }

        // Persist the text to cross-session message history.
        if !text.is_empty() {
            let encoded_mentions = mention_bindings
                .iter()
                .map(|binding| LinkedMention {
                    mention: binding.mention.clone(),
                    path: binding.path.clone(),
                })
                .collect::<Vec<_>>();
            let history_text = encode_history_mentions(&text, &encoded_mentions);
            self.chaos_op_tx
                .send(Op::AddToHistory { text: history_text })
                .unwrap_or_else(|e| {
                    tracing::error!("failed to send AddHistory op: {e}");
                });
        }

        if let Some(pending_steer) = pending_steer {
            self.pending_steers.push_back(pending_steer);
            self.saw_plan_item_this_turn = false;
            self.refresh_pending_input_preview();
        }

        // Show replayable user content in conversation history.
        if render_in_history && !text.is_empty() {
            let local_image_paths = local_images
                .into_iter()
                .map(|img| img.path)
                .collect::<Vec<_>>();
            self.last_rendered_user_message_event =
                Some(Self::rendered_user_message_event_from_parts(
                    text.clone(),
                    text_elements.clone(),
                    local_image_paths.clone(),
                    remote_image_urls.clone(),
                ));
            self.add_to_history(history_cell::new_user_prompt(
                text,
                text_elements,
                local_image_paths,
                remote_image_urls,
            ));
        } else if render_in_history && !remote_image_urls.is_empty() {
            self.last_rendered_user_message_event =
                Some(Self::rendered_user_message_event_from_parts(
                    String::new(),
                    Vec::new(),
                    Vec::new(),
                    remote_image_urls.clone(),
                ));
            self.add_to_history(history_cell::new_user_prompt(
                String::new(),
                Vec::new(),
                Vec::new(),
                remote_image_urls,
            ));
        }

        self.needs_final_message_separator = false;
    }

    /// Restore the blocked submission draft without losing mention resolution state.
    ///
    /// The blocked-image path intentionally keeps the draft in the composer so
    /// users can remove attachments and retry. We must restore
    /// mention bindings alongside visible text; restoring only `$name` tokens
    /// makes the draft look correct while degrading mention resolution to
    /// name-only heuristics on retry.
    fn restore_blocked_image_submission(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_images: Vec<LocalImageAttachment>,
        mention_bindings: Vec<MentionBinding>,
        remote_image_urls: Vec<String>,
    ) {
        // Preserve the user's composed payload so they can retry after changing models.
        let local_image_paths = local_images.iter().map(|img| img.path.clone()).collect();
        self.set_remote_image_urls(remote_image_urls);
        self.bottom_pane.set_composer_text_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
        );
        self.add_to_history(history_cell::new_warning_event(
            self.image_inputs_not_supported_message(),
        ));
        self.request_redraw();
    }

    /// Exit the UI immediately without waiting for shutdown.
    ///
    /// Prefer [`Self::request_quit_without_confirmation`] for user-initiated exits;
    /// this is mainly a fallback for shutdown completion or emergency exits.
    fn request_immediate_exit(&self) {
        self.app_event_tx.send(AppEvent::Exit(ExitMode::Immediate));
    }

    /// Request a shutdown-first quit.
    ///
    /// This is used for explicit quit commands (`/quit`, `/exit`, `/logout`) and for
    /// the double-press Ctrl+C/Ctrl+D quit shortcut.
    fn request_quit_without_confirmation(&self) {
        self.app_event_tx
            .send(AppEvent::Exit(ExitMode::ShutdownFirst));
    }

    fn request_redraw(&mut self) {
        self.frame_requester.schedule_frame();
    }

    fn bump_active_cell_revision(&mut self) {
        // Wrapping avoids overflow; wraparound would require 2^64 bumps and at
        // worst causes a one-time cache-key collision.
        self.active_cell_revision = self.active_cell_revision.wrapping_add(1);
    }

    fn notify(&mut self, notification: Notification) {
        if !notification.allowed_for(&self.config.tui_notifications) {
            return;
        }
        if let Some(existing) = self.pending_notification.as_ref()
            && existing.priority() > notification.priority()
        {
            return;
        }
        self.pending_notification = Some(notification);
        self.request_redraw();
    }

    pub fn maybe_post_pending_notification(&mut self, tui: &mut crate::tui::Tui) {
        if let Some(notif) = self.pending_notification.take() {
            tui.notify(notif.display());
        }
    }

    /// Mark the active cell as failed (✗) and flush it into history.
    fn finalize_active_cell_as_failed(&mut self) {
        if let Some(mut cell) = self.active_cell.take() {
            // Insert finalized cell into history and keep grouping consistent.
            if let Some(exec) = cell.as_any_mut().downcast_mut::<ExecCell>() {
                exec.mark_failed();
            } else if let Some(tool) = cell.as_any_mut().downcast_mut::<McpToolCallCell>() {
                tool.mark_failed();
            }
            self.add_boxed_history(cell);
        }
    }

    // If idle and there are queued inputs, submit exactly one to start the next turn.
    pub fn maybe_send_next_queued_input(&mut self) {
        if self.suppress_queue_autosend {
            return;
        }
        if self.bottom_pane.is_task_running() {
            return;
        }
        if let Some(user_message) = self.queued_user_messages.pop_front() {
            self.submit_user_message(user_message);
        }
        // Update the list to reflect the remaining queued messages (if any).
        self.refresh_pending_input_preview();
    }

    /// Rebuild and update the bottom-pane pending-input preview.
    fn refresh_pending_input_preview(&mut self) {
        let queued_messages: Vec<String> = self
            .queued_user_messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        let pending_steers: Vec<String> = self
            .pending_steers
            .iter()
            .map(|steer| steer.user_message.text.clone())
            .collect();
        self.bottom_pane
            .set_pending_input_preview(queued_messages, pending_steers);
    }

    pub fn set_pending_process_approvals(&mut self, threads: Vec<String>) {
        self.bottom_pane.set_pending_process_approvals(threads);
    }

    pub fn add_diff_in_progress(&mut self) {
        self.request_redraw();
    }

    pub fn on_diff_complete(&mut self) {
        self.request_redraw();
    }

    pub fn add_status_output(&mut self) {
        let default_usage = TokenUsage::default();
        let token_info = self.token_info.as_ref();
        let total_usage = token_info
            .map(|ti| &ti.total_token_usage)
            .unwrap_or(&default_usage);
        let collaboration_mode = self.collaboration_mode_label();
        let reasoning_effort_override = Some(self.effective_reasoning_effort());
        let rate_limit_snapshots: Vec<RateLimitSnapshotDisplay> = self
            .rate_limit_snapshots_by_limit_id
            .values()
            .cloned()
            .collect();
        self.add_to_history(crate::status::new_status_output_with_rate_limits(
            &self.config,
            self.auth_manager.as_ref(),
            token_info,
            total_usage,
            &self.process_id,
            self.process_name.clone(),
            self.forked_from,
            rate_limit_snapshots.as_slice(),
            self.plan_type,
            Timestamp::now(),
            self.model_display_name(),
            collaboration_mode,
            reasoning_effort_override,
        ));
    }

    pub fn add_debug_config_output(&mut self) {
        self.add_to_history(crate::debug_config::new_debug_config_output(
            &self.config,
            self.session_network_proxy.as_ref(),
        ));
    }

    pub fn add_ps_output(&mut self) {
        let processes = self
            .unified_exec_processes
            .iter()
            .map(|process| history_cell::UnifiedExecProcessDetails {
                command_display: process.command_display.clone(),
                recent_chunks: process.recent_chunks.clone(),
            })
            .collect();
        self.add_to_history(history_cell::new_unified_exec_processes_output(processes));
    }

    fn clean_background_terminals(&mut self) {
        self.submit_op(Op::CleanBackgroundTerminals);
        self.add_info_message(
            "Stopping all background terminals.".to_string(),
            /*hint*/ None,
        );
    }

    pub fn refresh_connectors(&mut self, force_refetch: bool) {
        self.prefetch_connectors_with_options(force_refetch);
    }

    fn prefetch_connectors(&mut self) {
        self.prefetch_connectors_with_options(/*force_refetch*/ false);
    }

    fn prefetch_connectors_with_options(&mut self, force_refetch: bool) {
        if !self.connectors_enabled() {
            return;
        }
        if self.connectors_prefetch_in_flight {
            if force_refetch {
                self.connectors_force_refetch_pending = true;
            }
            return;
        }

        // Connectors infrastructure removed — return empty results immediately.
        self.connectors_prefetch_in_flight = false;
        let snapshot = ConnectorsSnapshot {
            connectors: Vec::new(),
        };
        self.connectors_cache = ConnectorsCacheState::Ready(snapshot);
    }

    fn lower_cost_preset(&self) -> Option<ModelPreset> {
        let models = self.models_manager.try_list_models().ok()?;
        models
            .iter()
            .find(|preset| preset.show_in_picker && preset.model == NUDGE_MODEL_SLUG)
            .cloned()
    }

    fn rate_limit_switch_prompt_hidden(&self) -> bool {
        self.config
            .notices
            .hide_rate_limit_model_nudge
            .unwrap_or(false)
    }

    fn maybe_show_pending_rate_limit_prompt(&mut self) {
        if self.rate_limit_switch_prompt_hidden() {
            self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Idle;
            return;
        }
        if !matches!(
            self.rate_limit_switch_prompt,
            RateLimitSwitchPromptState::Pending
        ) {
            return;
        }
        if let Some(preset) = self.lower_cost_preset() {
            self.open_rate_limit_switch_prompt(preset);
            self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Shown;
        } else {
            self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Idle;
        }
    }

    fn open_rate_limit_switch_prompt(&mut self, preset: ModelPreset) {
        let switch_model = preset.model;
        let switch_model_for_events = switch_model.clone();
        let default_effort: ReasoningEffortConfig = preset.default_reasoning_effort;

        let switch_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(AppEvent::ChaosOp(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: None,
                approvals_reviewer: None,
                sandbox_policy: None,

                model: Some(switch_model_for_events.clone()),
                effort: Some(Some(default_effort)),
                summary: None,
                service_tier: None,
                collaboration_mode: None,
                personality: None,
            }));
            tx.send(AppEvent::UpdateModel(switch_model_for_events.clone()));
            tx.send(AppEvent::UpdateReasoningEffort(Some(default_effort)));
        })];

        let keep_actions: Vec<SelectionAction> = Vec::new();
        let never_actions: Vec<SelectionAction> = vec![Box::new(|tx| {
            tx.send(AppEvent::UpdateRateLimitSwitchPromptHidden(true));
            tx.send(AppEvent::PersistRateLimitSwitchPromptHidden);
        })];
        let description = if preset.description.is_empty() {
            Some("Uses fewer credits for upcoming turns.".to_string())
        } else {
            Some(preset.description)
        };

        let items = vec![
            SelectionItem {
                name: format!("Switch to {switch_model}"),
                description,
                selected_description: None,
                is_current: false,
                actions: switch_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Keep current model".to_string(),
                description: None,
                selected_description: None,
                is_current: false,
                actions: keep_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Keep current model (never show again)".to_string(),
                description: Some(
                    "Hide future rate limit reminders about switching models.".to_string(),
                ),
                selected_description: None,
                is_current: false,
                actions: never_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Approaching rate limits".to_string()),
            subtitle: Some(format!("Switch to {switch_model} for lower credit usage?")),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    /// Open a popup to choose a quick auto model. Selecting "All models"
    /// opens the full picker with every available preset.
    pub fn open_model_popup(&mut self) {
        if !self.is_session_configured() {
            self.add_info_message(
                "Model selection is disabled until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        }

        let presets: Vec<ModelPreset> = if crate::theme::is_clamped() {
            use chaos_ipc::openai_models::ReasoningEffort;
            use chaos_ipc::openai_models::ReasoningEffortPreset;

            // Build presets from the real Claude Code init response.
            let cached = chaos_clamp::cached_models();
            if let Some(models_json) = cached.as_ref().and_then(|v| v.as_array()) {
                models_json
                    .iter()
                    .map(|m| {
                        let value = m.get("value").and_then(|v| v.as_str()).unwrap_or("default");
                        let display = m
                            .get("displayName")
                            .and_then(|v| v.as_str())
                            .unwrap_or(value);
                        let desc = m.get("description").and_then(|v| v.as_str()).unwrap_or("");
                        let is_default = value == "default";
                        let efforts: Vec<ReasoningEffortPreset> = m
                            .get("supportedEffortLevels")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|e| {
                                        let s = e.as_str()?;
                                        let effort = match s {
                                            "low" => ReasoningEffort::Low,
                                            "medium" => ReasoningEffort::Medium,
                                            "high" => ReasoningEffort::High,
                                            _ => return None,
                                        };
                                        Some(ReasoningEffortPreset {
                                            effort,
                                            description: s.to_string(),
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        ModelPreset {
                            id: value.to_string(),
                            model: value.to_string(),
                            display_name: display.to_string(),
                            description: desc.to_string(),
                            default_reasoning_effort: ReasoningEffort::Medium,
                            supported_reasoning_efforts: efforts,
                            supports_personality: false,
                            is_default,
                            show_in_picker: true,
                            availability_nux: None,
                            supported_in_api: true,
                            input_modalities: vec![],
                        }
                    })
                    .collect()
            } else {
                // No cached models yet — subprocess hasn't been spawned.
                self.add_info_message(
                    "Send a message first to initialize the Claude Code subprocess, \
                     then try /model again."
                        .to_string(),
                    None,
                );
                return;
            }
        } else {
            match self.models_manager.try_list_models() {
                Ok(models) => models,
                Err(_) => {
                    self.add_info_message(
                        "Models are being updated; please try /model again in a moment."
                            .to_string(),
                        /*hint*/ None,
                    );
                    return;
                }
            }
        };
        self.open_model_popup_with_presets(presets);
    }

    pub fn open_personality_popup(&mut self) {
        if !self.is_session_configured() {
            self.add_info_message(
                "Personality selection is disabled until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        }
        if !self.current_model_supports_personality() {
            let current_model = self.current_model();
            self.add_error_message(format!(
                "Current model ({current_model}) doesn't support personalities. Try /model to pick a different model."
            ));
            return;
        }
        self.open_personality_popup_for_current_model();
    }

    fn open_personality_popup_for_current_model(&mut self) {
        let current_personality = self.config.personality.unwrap_or(Personality::Friendly);
        let personalities = [Personality::Friendly, Personality::Pragmatic];
        let supports_personality = self.current_model_supports_personality();

        let items: Vec<SelectionItem> = personalities
            .into_iter()
            .map(|personality| {
                let name = Self::personality_label(personality).to_string();
                let description = Some(Self::personality_description(personality).to_string());
                let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                    tx.send(AppEvent::ChaosOp(Op::OverrideTurnContext {
                        cwd: None,
                        approval_policy: None,
                        approvals_reviewer: None,
                        sandbox_policy: None,
                        model: None,
                        effort: None,
                        summary: None,
                        service_tier: None,
                        collaboration_mode: None,

                        personality: Some(personality),
                    }));
                    tx.send(AppEvent::UpdatePersonality(personality));
                    tx.send(AppEvent::PersistPersonalitySelection { personality });
                })];
                SelectionItem {
                    name,
                    description,
                    is_current: current_personality == personality,
                    is_disabled: !supports_personality,
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        let mut header = ColumnRenderable::new();
        header.push(Line::from("Select Personality".bold()));
        header.push(Line::from("Choose a communication style for Chaos.".dim()));

        self.bottom_pane.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    fn model_menu_header(&self, title: &str, subtitle: &str) -> Box<dyn Renderable> {
        let title = title.to_string();
        let subtitle = subtitle.to_string();
        let mut header = ColumnRenderable::new();
        header.push(Line::from(title.bold()));
        header.push(Line::from(subtitle.dim()));
        if let Some(warning) = self.model_menu_warning_line() {
            header.push(warning);
        }
        Box::new(header)
    }

    fn model_menu_warning_line(&self) -> Option<Line<'static>> {
        let base_url = self.custom_openai_base_url()?;
        let warning = format!(
            "Warning: OpenAI base URL is overridden to {base_url}. Selecting models may not be supported or work properly."
        );
        Some(Line::from(warning.red()))
    }

    fn custom_openai_base_url(&self) -> Option<String> {
        if !self.config.model_provider.is_openai() {
            return None;
        }

        let base_url = self.config.model_provider.base_url.as_ref()?;
        let trimmed = base_url.trim();
        if trimmed.is_empty() {
            return None;
        }

        let normalized = trimmed.trim_end_matches('/');
        if normalized == OPENAI_DEFAULT_BASE_URL {
            return None;
        }

        Some(trimmed.to_string())
    }

    pub fn open_model_popup_with_presets(&mut self, presets: Vec<ModelPreset>) {
        let presets: Vec<ModelPreset> = presets
            .into_iter()
            .filter(|preset| preset.show_in_picker)
            .collect();

        let current_model = self.current_model();
        let current_label = presets
            .iter()
            .find(|preset| preset.model.as_str() == current_model)
            .map(|preset| preset.model.to_string())
            .unwrap_or_else(|| self.model_display_name().to_string());

        let (mut auto_presets, other_presets): (Vec<ModelPreset>, Vec<ModelPreset>) = presets
            .into_iter()
            .partition(|preset| Self::is_auto_model(&preset.model));

        if auto_presets.is_empty() {
            self.open_all_models_popup(other_presets);
            return;
        }

        auto_presets.sort_by_key(|preset| Self::auto_model_order(&preset.model));
        let mut items: Vec<SelectionItem> = auto_presets
            .into_iter()
            .map(|preset| {
                let description =
                    (!preset.description.is_empty()).then_some(preset.description.clone());
                let model = preset.model.clone();
                let should_prompt_plan_mode_scope = self.should_prompt_plan_mode_reasoning_scope(
                    model.as_str(),
                    Some(preset.default_reasoning_effort),
                );
                let actions = Self::model_selection_actions(
                    model.clone(),
                    Some(preset.default_reasoning_effort),
                    should_prompt_plan_mode_scope,
                );
                SelectionItem {
                    name: model.clone(),
                    description,
                    is_current: model.as_str() == current_model,
                    is_default: preset.is_default,
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        if !other_presets.is_empty() {
            let all_models = other_presets;
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenAllModelsPopup {
                    models: all_models.clone(),
                });
            })];

            let is_current = !items.iter().any(|item| item.is_current);
            let description = Some(format!(
                "Choose a specific model and reasoning level (current: {current_label})"
            ));

            items.push(SelectionItem {
                name: "All models".to_string(),
                description,
                is_current,
                actions,
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        let header = self.model_menu_header(
            "Select Model",
            "Pick a quick auto mode or browse all models.",
        );
        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header,
            ..Default::default()
        });
    }

    fn is_auto_model(model: &str) -> bool {
        model.starts_with("chaos-auto-")
    }

    fn auto_model_order(model: &str) -> usize {
        match model {
            "chaos-auto-fast" => 0,
            "chaos-auto-balanced" => 1,
            "chaos-auto-thorough" => 2,
            _ => 3,
        }
    }

    pub fn open_all_models_popup(&mut self, presets: Vec<ModelPreset>) {
        if presets.is_empty() {
            self.add_info_message(
                "No additional models are available right now.".to_string(),
                /*hint*/ None,
            );
            return;
        }

        let mut items: Vec<SelectionItem> = Vec::new();
        for preset in presets.into_iter() {
            let description =
                (!preset.description.is_empty()).then_some(preset.description.to_string());
            let is_current = preset.model.as_str() == self.current_model();
            let single_supported_effort = preset.supported_reasoning_efforts.len() == 1;
            let preset_for_action = preset.clone();
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                let preset_for_event = preset_for_action.clone();
                tx.send(AppEvent::OpenReasoningPopup {
                    model: preset_for_event,
                });
            })];
            items.push(SelectionItem {
                name: preset.model.clone(),
                description,
                is_current,
                is_default: preset.is_default,
                actions,
                dismiss_on_select: single_supported_effort,
                ..Default::default()
            });
        }

        let header = self.model_menu_header(
            "Select Model and Effort",
            "Access legacy models by running chaos -m <model_name> or in your config.toml",
        );
        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some("Press enter to select reasoning effort, or esc to dismiss.".into()),
            items,
            header,
            ..Default::default()
        });
    }

    pub fn open_collaboration_modes_popup(&mut self) {
        let presets = collaboration_modes::presets_for_tui(self.models_manager.as_ref());
        if presets.is_empty() {
            self.add_info_message(
                "No collaboration modes are available right now.".to_string(),
                /*hint*/ None,
            );
            return;
        }

        let current_kind = self
            .active_collaboration_mask
            .as_ref()
            .and_then(|mask| mask.mode)
            .or_else(|| {
                collaboration_modes::default_mask(self.models_manager.as_ref())
                    .and_then(|mask| mask.mode)
            });
        let items: Vec<SelectionItem> = presets
            .into_iter()
            .map(|mask| {
                let name = mask.name.clone();
                let is_current = current_kind == mask.mode;
                let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                    tx.send(AppEvent::UpdateCollaborationMode(mask.clone()));
                })];
                SelectionItem {
                    name,
                    is_current,
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select Collaboration Mode".to_string()),
            subtitle: Some("Pick a collaboration preset.".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    fn model_selection_actions(
        model_for_action: String,
        effort_for_action: Option<ReasoningEffortConfig>,
        should_prompt_plan_mode_scope: bool,
    ) -> Vec<SelectionAction> {
        vec![Box::new(move |tx| {
            if should_prompt_plan_mode_scope {
                tx.send(AppEvent::OpenPlanReasoningScopePrompt {
                    model: model_for_action.clone(),
                    effort: effort_for_action,
                });
                return;
            }

            tx.send(AppEvent::UpdateModel(model_for_action.clone()));
            tx.send(AppEvent::UpdateReasoningEffort(effort_for_action));
            Self::queue_persist_model_selection(
                tx,
                model_for_action.clone(),
                effort_for_action,
                crate::theme::is_clamped(),
            );
        })]
    }

    fn should_prompt_plan_mode_reasoning_scope(
        &self,
        selected_model: &str,
        selected_effort: Option<ReasoningEffortConfig>,
    ) -> bool {
        if !self.collaboration_modes_enabled()
            || self.active_mode_kind() != ModeKind::Plan
            || selected_model != self.current_model()
        {
            return false;
        }

        // Prompt whenever the selection is not a true no-op for both:
        // 1) the active Plan-mode effective reasoning, and
        // 2) the stored global defaults that would be updated by the fallback path.
        selected_effort != self.effective_reasoning_effort()
            || selected_model != self.current_collaboration_mode.model()
            || selected_effort != self.current_collaboration_mode.reasoning_effort()
    }

    pub fn open_plan_reasoning_scope_prompt(
        &mut self,
        model: String,
        effort: Option<ReasoningEffortConfig>,
    ) {
        let reasoning_phrase = match effort {
            Some(ReasoningEffortConfig::None) => "no reasoning".to_string(),
            Some(selected_effort) => {
                format!(
                    "{} reasoning",
                    Self::reasoning_effort_label(selected_effort).to_lowercase()
                )
            }
            None => "the selected reasoning".to_string(),
        };
        let plan_only_description = format!("Always use {reasoning_phrase} in Plan mode.");
        let plan_reasoning_source = if let Some(plan_override) =
            self.config.plan_mode_reasoning_effort
        {
            format!(
                "user-chosen Plan override ({})",
                Self::reasoning_effort_label(plan_override).to_lowercase()
            )
        } else if let Some(plan_mask) = collaboration_modes::plan_mask(self.models_manager.as_ref())
        {
            match plan_mask.reasoning_effort.flatten() {
                Some(plan_effort) => format!(
                    "built-in Plan default ({})",
                    Self::reasoning_effort_label(plan_effort).to_lowercase()
                ),
                None => "built-in Plan default (no reasoning)".to_string(),
            }
        } else {
            "built-in Plan default".to_string()
        };
        let all_modes_description = format!(
            "Set the global default reasoning level and the Plan mode override. This replaces the current {plan_reasoning_source}."
        );
        let subtitle = format!("Choose where to apply {reasoning_phrase}.");

        let plan_only_actions: Vec<SelectionAction> = vec![Box::new({
            let model = model.clone();
            move |tx| {
                tx.send(AppEvent::UpdateModel(model.clone()));
                tx.send(AppEvent::UpdatePlanModeReasoningEffort(effort));
                tx.send(AppEvent::PersistPlanModeReasoningEffort(effort));
            }
        })];
        let all_modes_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(AppEvent::UpdateModel(model.clone()));
            tx.send(AppEvent::UpdateReasoningEffort(effort));
            tx.send(AppEvent::UpdatePlanModeReasoningEffort(effort));
            tx.send(AppEvent::PersistPlanModeReasoningEffort(effort));
            Self::queue_persist_model_selection(
                tx,
                model.clone(),
                effort,
                crate::theme::is_clamped(),
            );
        })];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(PLAN_MODE_REASONING_SCOPE_TITLE.to_string()),
            subtitle: Some(subtitle),
            footer_hint: Some(standard_popup_hint_line()),
            items: vec![
                SelectionItem {
                    name: PLAN_MODE_REASONING_SCOPE_PLAN_ONLY.to_string(),
                    description: Some(plan_only_description),
                    actions: plan_only_actions,
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: PLAN_MODE_REASONING_SCOPE_ALL_MODES.to_string(),
                    description: Some(all_modes_description),
                    actions: all_modes_actions,
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        self.notify(Notification::PlanModePrompt {
            title: PLAN_MODE_REASONING_SCOPE_TITLE.to_string(),
        });
    }

    /// Open a popup to choose the reasoning effort (stage 2) for the given model.
    pub fn open_reasoning_popup(&mut self, preset: ModelPreset) {
        let default_effort: ReasoningEffortConfig = preset.default_reasoning_effort;
        let supported = preset.supported_reasoning_efforts;
        let in_plan_mode =
            self.collaboration_modes_enabled() && self.active_mode_kind() == ModeKind::Plan;

        let warn_effort = if supported
            .iter()
            .any(|option| option.effort == ReasoningEffortConfig::XHigh)
        {
            Some(ReasoningEffortConfig::XHigh)
        } else if supported
            .iter()
            .any(|option| option.effort == ReasoningEffortConfig::High)
        {
            Some(ReasoningEffortConfig::High)
        } else {
            None
        };
        let warning_text = warn_effort.map(|effort| {
            let effort_label = Self::reasoning_effort_label(effort);
            format!("⚠ {effort_label} reasoning effort can quickly consume Plus plan rate limits.")
        });
        let warn_for_model = preset.model.starts_with("gpt-5.1-codex")
            || preset.model.starts_with("gpt-5.1-codex-max")
            || preset.model.starts_with("gpt-5.2");

        struct EffortChoice {
            stored: Option<ReasoningEffortConfig>,
            display: ReasoningEffortConfig,
        }
        let mut choices: Vec<EffortChoice> = Vec::new();
        for effort in ReasoningEffortConfig::iter() {
            if supported.iter().any(|option| option.effort == effort) {
                choices.push(EffortChoice {
                    stored: Some(effort),
                    display: effort,
                });
            }
        }
        if choices.is_empty() {
            choices.push(EffortChoice {
                stored: Some(default_effort),
                display: default_effort,
            });
        }

        if choices.len() == 1 {
            let selected_effort = choices.first().and_then(|c| c.stored);
            let selected_model = preset.model;
            if self.should_prompt_plan_mode_reasoning_scope(&selected_model, selected_effort) {
                self.app_event_tx
                    .send(AppEvent::OpenPlanReasoningScopePrompt {
                        model: selected_model,
                        effort: selected_effort,
                    });
            } else {
                self.apply_model_and_effort(selected_model, selected_effort);
            }
            return;
        }

        let default_choice: Option<ReasoningEffortConfig> = choices
            .iter()
            .any(|choice| choice.stored == Some(default_effort))
            .then_some(Some(default_effort))
            .flatten()
            .or_else(|| choices.iter().find_map(|choice| choice.stored))
            .or(Some(default_effort));

        let model_slug = preset.model.to_string();
        let is_current_model = self.current_model() == preset.model.as_str();
        let highlight_choice = if is_current_model {
            if in_plan_mode {
                self.config
                    .plan_mode_reasoning_effort
                    .or(self.effective_reasoning_effort())
            } else {
                self.effective_reasoning_effort()
            }
        } else {
            default_choice
        };
        let selection_choice = highlight_choice.or(default_choice);
        let initial_selected_idx = choices
            .iter()
            .position(|choice| choice.stored == selection_choice)
            .or_else(|| {
                selection_choice
                    .and_then(|effort| choices.iter().position(|choice| choice.display == effort))
            });
        let mut items: Vec<SelectionItem> = Vec::new();
        for choice in choices.iter() {
            let effort = choice.display;
            let mut effort_label = Self::reasoning_effort_label(effort).to_string();
            if choice.stored == default_choice {
                effort_label.push_str(" (default)");
            }

            let description = choice
                .stored
                .and_then(|effort| {
                    supported
                        .iter()
                        .find(|option| option.effort == effort)
                        .map(|option| option.description.to_string())
                })
                .filter(|text| !text.is_empty());

            let show_warning = warn_for_model && warn_effort == Some(effort);
            let selected_description = if show_warning {
                warning_text.as_ref().map(|warning_message| {
                    description.as_ref().map_or_else(
                        || warning_message.clone(),
                        |d| format!("{d}\n{warning_message}"),
                    )
                })
            } else {
                None
            };

            let model_for_action = model_slug.clone();
            let choice_effort = choice.stored;
            let should_prompt_plan_mode_scope =
                self.should_prompt_plan_mode_reasoning_scope(model_slug.as_str(), choice_effort);
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                if should_prompt_plan_mode_scope {
                    tx.send(AppEvent::OpenPlanReasoningScopePrompt {
                        model: model_for_action.clone(),
                        effort: choice_effort,
                    });
                } else {
                    tx.send(AppEvent::UpdateModel(model_for_action.clone()));
                    tx.send(AppEvent::UpdateReasoningEffort(choice_effort));
                    Self::queue_persist_model_selection(
                        tx,
                        model_for_action.clone(),
                        choice_effort,
                        crate::theme::is_clamped(),
                    );
                }
            })];

            items.push(SelectionItem {
                name: effort_label,
                description,
                selected_description,
                is_current: is_current_model && choice.stored == highlight_choice,
                actions,
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        let mut header = ColumnRenderable::new();
        header.push(Line::from(
            format!("Select Reasoning Level for {model_slug}").bold(),
        ));

        self.bottom_pane.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx,
            ..Default::default()
        });
    }

    fn reasoning_effort_label(effort: ReasoningEffortConfig) -> &'static str {
        match effort {
            ReasoningEffortConfig::None => "None",
            ReasoningEffortConfig::Minimal => "Minimal",
            ReasoningEffortConfig::Low => "Low",
            ReasoningEffortConfig::Medium => "Medium",
            ReasoningEffortConfig::High => "High",
            ReasoningEffortConfig::XHigh => "Extra high",
        }
    }

    fn apply_model_and_effort_without_persist(
        &self,
        model: String,
        effort: Option<ReasoningEffortConfig>,
    ) {
        self.app_event_tx.send(AppEvent::UpdateModel(model));
        self.app_event_tx
            .send(AppEvent::UpdateReasoningEffort(effort));
    }

    fn queue_persist_model_selection(
        tx: &AppEventSender,
        model: String,
        effort: Option<ReasoningEffortConfig>,
        clamped: bool,
    ) {
        if clamped {
            return;
        }

        tx.send(AppEvent::PersistModelSelection { model, effort });
    }

    fn apply_model_and_effort(&self, model: String, effort: Option<ReasoningEffortConfig>) {
        self.apply_model_and_effort_without_persist(model.clone(), effort);
        Self::queue_persist_model_selection(
            &self.app_event_tx,
            model,
            effort,
            crate::theme::is_clamped(),
        );
    }

    fn capture_pre_clamp_selection(&mut self) {
        self.pre_clamp_selection = Some(PreClampSelection {
            model: self.current_model().to_string(),
            reasoning_effort: self.current_collaboration_mode.reasoning_effort(),
            plan_mode_reasoning_effort: self.config.plan_mode_reasoning_effort,
        });
    }

    fn restore_pre_clamp_selection(&mut self) {
        let Some(previous) = self.pre_clamp_selection.take() else {
            return;
        };

        self.app_event_tx
            .send(AppEvent::UpdateModel(previous.model));
        self.app_event_tx
            .send(AppEvent::UpdateReasoningEffort(previous.reasoning_effort));
        self.app_event_tx
            .send(AppEvent::UpdatePlanModeReasoningEffort(
                previous.plan_mode_reasoning_effort,
            ));
    }

    /// Open the permissions popup (alias for /permissions).
    pub fn open_approvals_popup(&mut self) {
        self.open_permissions_popup();
    }

    /// Open a popup to choose the permissions mode (approval policy + sandbox policy).
    pub fn open_permissions_popup(&mut self) {
        let include_read_only = false;
        let current_approval = self.config.permissions.approval_policy.value();
        let current_sandbox = self.config.permissions.sandbox_policy.get();
        let mut items: Vec<SelectionItem> = Vec::new();
        let presets: Vec<ApprovalPreset> = builtin_approval_presets();

        for preset in presets.into_iter() {
            if !include_read_only && preset.id == "read-only" {
                continue;
            }
            let base_name = preset.label.to_string();
            let base_description =
                Some(preset.description.replace(" (Identical to Agent mode)", ""));
            let approval_disabled_reason = match self
                .config
                .permissions
                .approval_policy
                .can_set(&preset.approval)
            {
                Ok(()) => None,
                Err(err) => Some(err.to_string()),
            };
            let default_disabled_reason = approval_disabled_reason.clone();
            let requires_confirmation = preset.id == "full-access"
                && !self
                    .config
                    .notices
                    .hide_full_access_warning
                    .unwrap_or(false);
            let default_actions: Vec<SelectionAction> = if requires_confirmation {
                let preset_clone = preset.clone();
                vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenFullAccessConfirmation {
                        preset: preset_clone.clone(),
                        return_to_permissions: !include_read_only,
                    });
                })]
            } else {
                Self::approval_preset_actions(
                    preset.approval,
                    preset.sandbox.clone(),
                    base_name.clone(),
                    ApprovalsReviewer::User,
                )
            };
            if preset.id == "auto" {
                items.push(SelectionItem {
                    name: base_name.clone(),
                    description: base_description.clone(),
                    is_current: Self::preset_matches_current(
                        current_approval,
                        current_sandbox,
                        &preset,
                    ),
                    actions: default_actions,
                    dismiss_on_select: true,
                    disabled_reason: default_disabled_reason,
                    ..Default::default()
                });
            } else {
                items.push(SelectionItem {
                    name: base_name,
                    description: base_description,
                    is_current: Self::preset_matches_current(
                        current_approval,
                        current_sandbox,
                        &preset,
                    ),
                    actions: default_actions,
                    dismiss_on_select: true,
                    disabled_reason: default_disabled_reason,
                    ..Default::default()
                });
            }
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Update Model Permissions".to_string()),
            footer_note: None,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(()),
            ..Default::default()
        });
    }

    fn approval_preset_actions(
        approval: ApprovalPolicy,
        sandbox: SandboxPolicy,
        label: String,
        approvals_reviewer: ApprovalsReviewer,
    ) -> Vec<SelectionAction> {
        vec![Box::new(move |tx| {
            let sandbox_clone = sandbox.clone();
            tx.send(AppEvent::ChaosOp(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: Some(approval),
                approvals_reviewer: Some(approvals_reviewer),
                sandbox_policy: Some(sandbox_clone.clone()),

                model: None,
                effort: None,
                summary: None,
                service_tier: None,
                collaboration_mode: None,
                personality: None,
            }));
            tx.send(AppEvent::UpdateApprovalPolicy(approval));
            tx.send(AppEvent::UpdateSandboxPolicy(sandbox_clone));
            tx.send(AppEvent::UpdateApprovalsReviewer(approvals_reviewer));
            tx.send(AppEvent::InsertHistoryCell(Box::new(
                history_cell::new_info_event(
                    format!("Permissions updated to {label}"),
                    /*hint*/ None,
                ),
            )));
        })]
    }

    fn preset_matches_current(
        current_approval: ApprovalPolicy,
        current_sandbox: &SandboxPolicy,
        preset: &ApprovalPreset,
    ) -> bool {
        if current_approval != preset.approval {
            return false;
        }

        match (current_sandbox, &preset.sandbox) {
            (SandboxPolicy::RootAccess, SandboxPolicy::RootAccess) => true,
            (
                SandboxPolicy::ReadOnly {
                    network_access: current_network_access,
                    ..
                },
                SandboxPolicy::ReadOnly {
                    network_access: preset_network_access,
                    ..
                },
            ) => current_network_access == preset_network_access,
            (
                SandboxPolicy::WorkspaceWrite {
                    network_access: current_network_access,
                    ..
                },
                SandboxPolicy::WorkspaceWrite {
                    network_access: preset_network_access,
                    ..
                },
            ) => current_network_access == preset_network_access,
            _ => false,
        }
    }

    pub fn open_full_access_confirmation(
        &mut self,
        preset: ApprovalPreset,
        return_to_permissions: bool,
    ) {
        let selected_name = preset.label.to_string();
        let approval = preset.approval;
        let sandbox = preset.sandbox;
        let mut header_children: Vec<Box<dyn Renderable>> = Vec::new();
        let title_line = Line::from("Enable full access?").bold();
        let info_line = Line::from(vec![
            "When Chaos runs with full access, it can edit any file on your computer and run commands with network, without your approval. "
                .into(),
            "Exercise caution when enabling full access. This significantly increases the risk of data loss, leaks, or unexpected behavior."
                .fg(crate::theme::red()),
        ]);
        header_children.push(Box::new(title_line));
        header_children.push(Box::new(
            Paragraph::new(vec![info_line]).wrap(Wrap { trim: false }),
        ));
        let header = ColumnRenderable::with(header_children);

        let mut accept_actions = Self::approval_preset_actions(
            approval,
            sandbox.clone(),
            selected_name.clone(),
            ApprovalsReviewer::User,
        );
        accept_actions.push(Box::new(|tx| {
            tx.send(AppEvent::UpdateFullAccessWarningAcknowledged(true));
        }));

        let mut accept_and_remember_actions = Self::approval_preset_actions(
            approval,
            sandbox,
            selected_name,
            ApprovalsReviewer::User,
        );
        accept_and_remember_actions.push(Box::new(|tx| {
            tx.send(AppEvent::UpdateFullAccessWarningAcknowledged(true));
            tx.send(AppEvent::PersistFullAccessWarningAcknowledged);
        }));

        let deny_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            if return_to_permissions {
                tx.send(AppEvent::OpenPermissionsPopup);
            } else {
                tx.send(AppEvent::OpenApprovalsPopup);
            }
        })];

        let items = vec![
            SelectionItem {
                name: "Yes, continue anyway".to_string(),
                description: Some("Apply full access for this session".to_string()),
                actions: accept_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Yes, and don't ask again".to_string(),
                description: Some("Enable full access and remember this choice".to_string()),
                actions: accept_and_remember_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Cancel".to_string(),
                description: Some("Go back without enabling full access".to_string()),
                actions: deny_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            ..Default::default()
        });
    }

    /// Set the approval policy in the widget's config copy.
    pub fn set_approval_policy(&mut self, policy: ApprovalPolicy) {
        if let Err(err) = self.config.permissions.approval_policy.set(policy) {
            tracing::warn!(%err, "failed to set approval_policy on chat config");
        }
    }

    /// Set the sandbox policy in the widget's config copy.
    pub fn set_sandbox_policy(&mut self, policy: SandboxPolicy) -> ConstraintResult<()> {
        self.config.permissions.sandbox_policy.set(policy)?;
        Ok(())
    }

    pub fn set_feature_enabled(&mut self, feature: Feature, enabled: bool) -> bool {
        if let Err(err) = self.config.features.set_enabled(feature, enabled) {
            tracing::warn!(
                error = %err,
                feature = feature.key(),
                "failed to update constrained chat widget feature state"
            );
        }
        self.config.features.enabled(feature)
    }

    pub fn set_approvals_reviewer(&mut self, policy: ApprovalsReviewer) {
        self.config.approvals_reviewer = policy;
    }

    pub fn set_full_access_warning_acknowledged(&mut self, acknowledged: bool) {
        self.config.notices.hide_full_access_warning = Some(acknowledged);
    }

    pub fn set_rate_limit_switch_prompt_hidden(&mut self, hidden: bool) {
        self.config.notices.hide_rate_limit_model_nudge = Some(hidden);
        if hidden {
            self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Idle;
        }
    }

    pub fn set_plan_mode_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        self.config.plan_mode_reasoning_effort = effort;
        if self.collaboration_modes_enabled()
            && let Some(mask) = self.active_collaboration_mask.as_mut()
            && mask.mode == Some(ModeKind::Plan)
        {
            if let Some(effort) = effort {
                mask.reasoning_effort = Some(Some(effort));
            } else if let Some(plan_mask) =
                collaboration_modes::plan_mask(self.models_manager.as_ref())
            {
                mask.reasoning_effort = plan_mask.reasoning_effort;
            }
        }
    }

    /// Set the reasoning effort in the stored collaboration mode.
    pub fn set_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        self.current_collaboration_mode = self.current_collaboration_mode.with_updates(
            /*model*/ None,
            Some(effort),
            /*minion_instructions*/ None,
        );
        if self.collaboration_modes_enabled()
            && let Some(mask) = self.active_collaboration_mask.as_mut()
            && mask.mode != Some(ModeKind::Plan)
        {
            // Generic "global default" updates should not mutate the active Plan mask.
            // Plan reasoning is controlled by the Plan preset and Plan-only override updates.
            mask.reasoning_effort = Some(effort);
        }
    }

    /// Set the personality in the widget's config copy.
    pub fn set_personality(&mut self, personality: Personality) {
        self.config.personality = Some(personality);
    }

    /// Set the syntax theme override in the widget's config copy.
    pub fn set_tui_theme(&mut self, theme: Option<String>) {
        self.config.tui_theme = theme;
    }

    /// Set the model in the widget's config copy and stored collaboration mode.
    pub fn set_model(&mut self, model: &str) {
        self.current_collaboration_mode = self.current_collaboration_mode.with_updates(
            Some(model.to_string()),
            /*effort*/ None,
            /*minion_instructions*/ None,
        );
        if self.collaboration_modes_enabled()
            && let Some(mask) = self.active_collaboration_mask.as_mut()
        {
            mask.model = Some(model.to_string());
        }
        self.refresh_model_display();
    }

    pub fn current_model(&self) -> &str {
        if !self.collaboration_modes_enabled() {
            return self.current_collaboration_mode.model();
        }
        self.active_collaboration_mask
            .as_ref()
            .and_then(|mask| mask.model.as_deref())
            .unwrap_or_else(|| self.current_collaboration_mode.model())
    }

    fn sync_personality_command_enabled(&mut self) {
        self.bottom_pane.set_personality_command_enabled(true);
    }

    fn current_model_supports_personality(&self) -> bool {
        let model = self.current_model();
        self.models_manager
            .try_list_models()
            .ok()
            .and_then(|models| {
                models
                    .into_iter()
                    .find(|preset| preset.model == model)
                    .map(|preset| preset.supports_personality)
            })
            .unwrap_or(false)
    }

    /// Return whether the effective model currently advertises image-input support.
    ///
    /// We intentionally default to `true` when model metadata cannot be read so transient catalog
    /// failures do not hard-block user input in the UI.
    fn current_model_supports_images(&self) -> bool {
        let model = self.current_model();
        self.models_manager
            .try_list_models()
            .ok()
            .and_then(|models| {
                models
                    .into_iter()
                    .find(|preset| preset.model == model)
                    .map(|preset| preset.input_modalities.contains(&InputModality::Image))
            })
            .unwrap_or(true)
    }

    fn sync_image_paste_enabled(&mut self) {
        let enabled = self.current_model_supports_images();
        self.bottom_pane.set_image_paste_enabled(enabled);
    }

    fn image_inputs_not_supported_message(&self) -> String {
        format!(
            "Model {} does not support image inputs. Remove images or switch models.",
            self.current_model()
        )
    }

    #[allow(dead_code)] // Used in tests
    pub fn current_collaboration_mode(&self) -> &CollaborationMode {
        &self.current_collaboration_mode
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn active_collaboration_mode_kind(&self) -> ModeKind {
        self.active_mode_kind()
    }

    fn is_session_configured(&self) -> bool {
        self.process_id.is_some()
    }

    fn collaboration_modes_enabled(&self) -> bool {
        true
    }

    fn initial_collaboration_mask(
        _config: &Config,
        models_manager: &ModelsManager,
        model_override: Option<&str>,
    ) -> Option<CollaborationModeMask> {
        let mut mask = collaboration_modes::default_mask(models_manager)?;
        if let Some(model_override) = model_override {
            mask.model = Some(model_override.to_string());
        }
        Some(mask)
    }

    fn active_mode_kind(&self) -> ModeKind {
        self.active_collaboration_mask
            .as_ref()
            .and_then(|mask| mask.mode)
            .unwrap_or(ModeKind::Default)
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn current_reasoning_effort(&self) -> Option<ReasoningEffortConfig> {
        self.effective_reasoning_effort()
    }

    fn effective_reasoning_effort(&self) -> Option<ReasoningEffortConfig> {
        if !self.collaboration_modes_enabled() {
            return self.current_collaboration_mode.reasoning_effort();
        }
        let current_effort = self.current_collaboration_mode.reasoning_effort();
        self.active_collaboration_mask
            .as_ref()
            .and_then(|mask| mask.reasoning_effort)
            .unwrap_or(current_effort)
    }

    fn effective_collaboration_mode(&self) -> CollaborationMode {
        if !self.collaboration_modes_enabled() {
            return self.current_collaboration_mode.clone();
        }
        self.active_collaboration_mask.as_ref().map_or_else(
            || self.current_collaboration_mode.clone(),
            |mask| self.current_collaboration_mode.apply_mask(mask),
        )
    }

    fn refresh_model_display(&mut self) {
        let effective = self.effective_collaboration_mode();
        self.session_header.set_model(effective.model());
        // Keep composer paste affordances aligned with the currently effective model.
        self.sync_image_paste_enabled();
    }

    fn model_display_name(&self) -> &str {
        let model = self.current_model();
        if model.is_empty() {
            DEFAULT_MODEL_DISPLAY_NAME
        } else {
            model
        }
    }

    /// Get the label for the current collaboration mode.
    fn collaboration_mode_label(&self) -> Option<&'static str> {
        if !self.collaboration_modes_enabled() {
            return None;
        }
        let active_mode = self.active_mode_kind();
        active_mode
            .is_tui_visible()
            .then_some(active_mode.display_name())
    }

    fn collaboration_mode_indicator(&self) -> Option<CollaborationModeIndicator> {
        if !self.collaboration_modes_enabled() {
            return None;
        }
        match self.active_mode_kind() {
            ModeKind::Plan => Some(CollaborationModeIndicator::Plan),
            ModeKind::Default | ModeKind::PairProgramming | ModeKind::Execute => None,
        }
    }

    fn update_collaboration_mode_indicator(&mut self) {
        let indicator = self.collaboration_mode_indicator();
        self.bottom_pane.set_collaboration_mode_indicator(indicator);
    }

    fn personality_label(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "None",
            Personality::Friendly => "Friendly",
            Personality::Pragmatic => "Pragmatic",
        }
    }

    fn personality_description(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "No personality instructions.",
            Personality::Friendly => "Warm, collaborative, and helpful.",
            Personality::Pragmatic => "Concise, task-focused, and direct.",
        }
    }

    /// Cycle to the next collaboration mode variant (Plan -> Default -> Plan).
    fn cycle_collaboration_mode(&mut self) {
        if !self.collaboration_modes_enabled() {
            return;
        }

        if let Some(next_mask) = collaboration_modes::next_mask(
            self.models_manager.as_ref(),
            self.active_collaboration_mask.as_ref(),
        ) {
            self.set_collaboration_mask(next_mask);
        }
    }

    /// Update the active collaboration mask.
    ///
    /// When collaboration modes are enabled and a preset is selected,
    /// the current mode is attached to submissions as `Op::UserTurn { collaboration_mode: Some(...) }`.
    pub fn set_collaboration_mask(&mut self, mut mask: CollaborationModeMask) {
        if !self.collaboration_modes_enabled() {
            return;
        }
        let previous_mode = self.active_mode_kind();
        let previous_model = self.current_model().to_string();
        let previous_effort = self.effective_reasoning_effort();
        if mask.mode == Some(ModeKind::Plan)
            && let Some(effort) = self.config.plan_mode_reasoning_effort
        {
            mask.reasoning_effort = Some(Some(effort));
        }
        self.active_collaboration_mask = Some(mask);
        self.update_collaboration_mode_indicator();
        self.refresh_model_display();
        let next_mode = self.active_mode_kind();
        let next_model = self.current_model();
        let next_effort = self.effective_reasoning_effort();
        if previous_mode != next_mode
            && (previous_model != next_model || previous_effort != next_effort)
        {
            let mut message = format!("Model changed to {next_model}");
            if !next_model.starts_with("chaos-auto-") {
                let reasoning_label = match next_effort {
                    Some(ReasoningEffortConfig::Minimal) => "minimal",
                    Some(ReasoningEffortConfig::Low) => "low",
                    Some(ReasoningEffortConfig::Medium) => "medium",
                    Some(ReasoningEffortConfig::High) => "high",
                    Some(ReasoningEffortConfig::XHigh) => "xhigh",
                    None | Some(ReasoningEffortConfig::None) => "default",
                };
                message.push(' ');
                message.push_str(reasoning_label);
            }
            message.push_str(" for ");
            message.push_str(next_mode.display_name());
            message.push_str(" mode.");
            self.add_info_message(message, /*hint*/ None);
        }
        self.request_redraw();
    }

    fn connectors_enabled(&self) -> bool {
        false
    }

    fn connectors_for_mentions(&self) -> Option<&[AppInfo]> {
        if !self.connectors_enabled() {
            return None;
        }

        if let Some(snapshot) = &self.connectors_partial_snapshot {
            return Some(snapshot.connectors.as_slice());
        }

        match &self.connectors_cache {
            ConnectorsCacheState::Ready(snapshot) => Some(snapshot.connectors.as_slice()),
            _ => None,
        }
    }

    /// Build a placeholder cell while the session is configuring.
    fn placeholder_session_header_cell(_config: &Config) -> Box<dyn HistoryCell> {
        Box::new(history_cell::PlainHistoryCell::new(Vec::new()))
    }

    /// Apply the real session info cell.
    fn apply_session_info_cell(&mut self, cell: history_cell::SessionInfoCell) {
        // Replace any placeholder active cell with the real session info.
        self.active_cell.take();
        let cell = Box::new(cell) as Box<dyn HistoryCell>;
        if !cell.display_lines(u16::MAX).is_empty() {
            self.add_boxed_history(cell);
        }
    }

    pub fn add_info_message(&mut self, message: String, hint: Option<String>) {
        self.add_to_history(history_cell::new_info_event(message, hint));
        self.request_redraw();
    }

    pub fn add_plain_history_lines(&mut self, lines: Vec<Line<'static>>) {
        self.add_boxed_history(Box::new(PlainHistoryCell::new(lines)));
        self.request_redraw();
    }

    pub fn add_error_message(&mut self, message: String) {
        self.add_to_history(history_cell::new_error_event(message));
        self.request_redraw();
    }

    fn rename_confirmation_cell(name: &str, process_id: Option<ProcessId>) -> PlainHistoryCell {
        let resume_cmd = chaos_kern::util::resume_command(Some(name), process_id)
            .unwrap_or_else(|| format!("chaos resume {name}"));
        let name = name.to_string();
        let line = vec![
            "• ".into(),
            "Thread renamed to ".into(),
            name.cyan(),
            ", to resume this thread run ".into(),
            resume_cmd.cyan(),
        ];
        PlainHistoryCell::new(vec![line.into()])
    }

    pub fn add_mcp_output(&mut self) {
        let mcp_manager = McpManager::new();
        if mcp_manager.effective_servers(&self.config).is_empty() {
            self.add_to_history(history_cell::empty_mcp_output());
        } else {
            self.submit_op(Op::ListMcpTools);
        }
    }

    pub fn open_mcp_add_form(&mut self) {
        let cwd = self.config.cwd.clone();
        let view = crate::bottom_pane::McpAddForm::new(
            cwd,
            self.config.config_layer_stack.clone(),
            self.process_id,
            self.app_event_tx.clone(),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    #[allow(dead_code)]
    pub fn add_connectors_output(&mut self) {
        if !self.connectors_enabled() {
            self.add_info_message(
                "Apps are disabled.".to_string(),
                Some("Enable the apps feature to use $ or /apps.".to_string()),
            );
            return;
        }

        let connectors_cache = self.connectors_cache.clone();
        let should_force_refetch = !self.connectors_prefetch_in_flight
            || matches!(connectors_cache, ConnectorsCacheState::Ready(_));
        self.prefetch_connectors_with_options(should_force_refetch);

        match connectors_cache {
            ConnectorsCacheState::Ready(snapshot) => {
                if snapshot.connectors.is_empty() {
                    self.add_info_message("No apps available.".to_string(), /*hint*/ None);
                } else {
                    self.open_connectors_popup(&snapshot.connectors);
                }
            }
            ConnectorsCacheState::Failed(err) => {
                self.add_to_history(history_cell::new_error_event(err));
            }
            ConnectorsCacheState::Loading | ConnectorsCacheState::Uninitialized => {
                self.open_connectors_loading_popup();
            }
        }
        self.request_redraw();
    }

    #[allow(dead_code)]
    fn open_connectors_loading_popup(&mut self) {
        if !self.bottom_pane.replace_selection_view_if_active(
            CONNECTORS_SELECTION_VIEW_ID,
            self.connectors_loading_popup_params(),
        ) {
            self.bottom_pane
                .show_selection_view(self.connectors_loading_popup_params());
        }
    }

    #[allow(dead_code)]
    fn open_connectors_popup(&mut self, connectors: &[AppInfo]) {
        self.bottom_pane.show_selection_view(
            self.connectors_popup_params(connectors, /*selected_connector_id*/ None),
        );
    }

    #[allow(dead_code)]
    fn connectors_loading_popup_params(&self) -> SelectionViewParams {
        let mut header = ColumnRenderable::new();
        header.push(Line::from("Apps".bold()));
        header.push(Line::from("Loading installed and available apps...".dim()));

        SelectionViewParams {
            view_id: Some(CONNECTORS_SELECTION_VIEW_ID),
            header: Box::new(header),
            items: vec![SelectionItem {
                name: "Loading apps...".to_string(),
                description: Some("This updates when the full list is ready.".to_string()),
                is_disabled: true,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn connectors_popup_params(
        &self,
        connectors: &[AppInfo],
        selected_connector_id: Option<&str>,
    ) -> SelectionViewParams {
        let total = connectors.len();
        let installed = connectors
            .iter()
            .filter(|connector| connector.is_accessible)
            .count();
        let mut header = ColumnRenderable::new();
        header.push(Line::from("Apps".bold()));
        header.push(Line::from(
            "Use $ to insert an installed app into your prompt.".dim(),
        ));
        header.push(Line::from(
            format!("Installed {installed} of {total} available apps.").dim(),
        ));
        let initial_selected_idx = selected_connector_id.and_then(|selected_connector_id| {
            connectors
                .iter()
                .position(|connector| connector.id == selected_connector_id)
        });
        let mut items: Vec<SelectionItem> = Vec::with_capacity(connectors.len());
        for connector in connectors {
            let connector_label = connector.name.clone();
            let connector_title = connector_label.clone();
            let link_description = Self::connector_description(connector);
            let description = Self::connector_brief_description(connector);
            let status_label = Self::connector_status_label(connector);
            let search_value = format!("{connector_label} {}", connector.id);
            let mut item = SelectionItem {
                name: connector_label,
                description: Some(description),
                search_value: Some(search_value),
                ..Default::default()
            };
            let is_installed = connector.is_accessible;
            let selected_label = if is_installed {
                format!(
                    "{status_label}. Press Enter to open the app page to install, manage, or enable/disable this app."
                )
            } else {
                format!("{status_label}. Press Enter to open the app page to install this app.")
            };
            let missing_label = format!("{status_label}. App link unavailable.");
            let instructions = if connector.is_accessible {
                "Manage this app in your browser."
            } else {
                "Install this app in your browser, then reload Chaos."
            };
            if let Some(install_url) = connector.install_url.clone() {
                let app_id = connector.id.clone();
                let is_enabled = connector.is_enabled;
                let title = connector_title.clone();
                let instructions = instructions.to_string();
                let description = link_description.clone();
                item.actions = vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenAppLink {
                        app_id: app_id.clone(),
                        title: title.clone(),
                        description: description.clone(),
                        instructions: instructions.clone(),
                        url: install_url.clone(),
                        is_installed,
                        is_enabled,
                    });
                })];
                item.dismiss_on_select = true;
                item.selected_description = Some(selected_label);
            } else {
                let missing_label_for_action = missing_label.clone();
                item.actions = vec![Box::new(move |tx| {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_info_event(
                            missing_label_for_action.clone(),
                            /*hint*/ None,
                        ),
                    )));
                })];
                item.dismiss_on_select = true;
                item.selected_description = Some(missing_label);
            }
            items.push(item);
        }

        SelectionViewParams {
            view_id: Some(CONNECTORS_SELECTION_VIEW_ID),
            header: Box::new(header),
            footer_hint: Some(Self::connectors_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search apps".to_string()),
            col_width_mode: ColumnWidthMode::AutoAllRows,
            initial_selected_idx,
            ..Default::default()
        }
    }

    fn refresh_connectors_popup_if_open(&mut self, connectors: &[AppInfo]) {
        let selected_connector_id =
            if let (Some(selected_index), ConnectorsCacheState::Ready(snapshot)) = (
                self.bottom_pane
                    .selected_index_for_active_view(CONNECTORS_SELECTION_VIEW_ID),
                &self.connectors_cache,
            ) {
                snapshot
                    .connectors
                    .get(selected_index)
                    .map(|connector| connector.id.as_str())
            } else {
                None
            };
        let _ = self.bottom_pane.replace_selection_view_if_active(
            CONNECTORS_SELECTION_VIEW_ID,
            self.connectors_popup_params(connectors, selected_connector_id),
        );
    }

    fn connectors_popup_hint_line() -> Line<'static> {
        Line::from(vec![
            "Press ".into(),
            key_hint::plain(KeyCode::Esc).into(),
            " to close.".into(),
        ])
    }

    fn connector_brief_description(connector: &AppInfo) -> String {
        let status_label = Self::connector_status_label(connector);
        match Self::connector_description(connector) {
            Some(description) => format!("{status_label} · {description}"),
            None => status_label.to_string(),
        }
    }

    fn connector_status_label(connector: &AppInfo) -> &'static str {
        if connector.is_accessible {
            if connector.is_enabled {
                "Installed"
            } else {
                "Installed · Disabled"
            }
        } else {
            "Can be installed"
        }
    }

    fn connector_description(connector: &AppInfo) -> Option<String> {
        connector
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(str::to_string)
    }

    /// Forward file-search results to the bottom pane.
    pub fn apply_file_search_result(&mut self, query: String, matches: Vec<FileMatch>) {
        self.bottom_pane.on_file_search_result(query, matches);
    }

    /// Handles a Ctrl+C press at the chat-widget layer.
    ///
    /// The first press arms a time-bounded quit shortcut and shows a footer hint via the bottom
    /// pane. If cancellable work is active, Ctrl+C also submits `Op::Interrupt` after the shortcut
    /// is armed; this interrupts the turn but intentionally preserves background terminals.
    ///
    /// If the same quit shortcut is pressed again before expiry, this requests a shutdown-first
    /// quit.
    fn on_ctrl_c(&mut self) {
        let key = key_hint::ctrl(KeyCode::Char('c'));
        let modal_or_popup_active = !self.bottom_pane.no_modal_or_popup_active();
        if self.bottom_pane.on_ctrl_c() == CancellationEvent::Handled {
            if DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED {
                if modal_or_popup_active {
                    self.quit_shortcut_expires_at = None;
                    self.quit_shortcut_key = None;
                    self.bottom_pane.clear_quit_shortcut_hint();
                } else {
                    self.arm_quit_shortcut(key);
                }
            }
            return;
        }

        if !DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED {
            if self.is_cancellable_work_active() {
                self.submit_op(Op::Interrupt);
            } else {
                self.request_quit_without_confirmation();
            }
            return;
        }

        if self.quit_shortcut_active_for(key) {
            self.quit_shortcut_expires_at = None;
            self.quit_shortcut_key = None;
            self.request_quit_without_confirmation();
            return;
        }

        self.arm_quit_shortcut(key);

        if self.is_cancellable_work_active() {
            self.submit_op(Op::Interrupt);
        }
    }

    /// Handles a Ctrl+D press at the chat-widget layer.
    ///
    /// Ctrl-D only participates in quit when the composer is empty and no modal/popup is active.
    /// Otherwise it should be routed to the active view and not attempt to quit.
    fn on_ctrl_d(&mut self) -> bool {
        let key = key_hint::ctrl(KeyCode::Char('d'));
        if !DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED {
            if !self.bottom_pane.composer_is_empty() || !self.bottom_pane.no_modal_or_popup_active()
            {
                return false;
            }

            self.request_quit_without_confirmation();
            return true;
        }

        if self.quit_shortcut_active_for(key) {
            self.quit_shortcut_expires_at = None;
            self.quit_shortcut_key = None;
            self.request_quit_without_confirmation();
            return true;
        }

        if !self.bottom_pane.composer_is_empty() || !self.bottom_pane.no_modal_or_popup_active() {
            return false;
        }

        self.arm_quit_shortcut(key);
        true
    }

    /// True if `key` matches the armed quit shortcut and the window has not expired.
    fn quit_shortcut_active_for(&self, key: KeyBinding) -> bool {
        self.quit_shortcut_key == Some(key)
            && self
                .quit_shortcut_expires_at
                .is_some_and(|expires_at| Instant::now() < expires_at)
    }

    /// Arm the double-press quit shortcut and show the footer hint.
    ///
    /// This keeps the state machine (`quit_shortcut_*`) in `ChatWidget`, since
    /// it is the component that interprets Ctrl+C vs Ctrl+D and decides whether
    /// quitting is currently allowed, while delegating rendering to `BottomPane`.
    fn arm_quit_shortcut(&mut self, key: KeyBinding) {
        self.quit_shortcut_expires_at = Instant::now()
            .checked_add(QUIT_SHORTCUT_TIMEOUT)
            .or_else(|| Some(Instant::now()));
        self.quit_shortcut_key = Some(key);
        self.bottom_pane.show_quit_shortcut_hint(key);
    }

    // Review mode counts as cancellable work so Ctrl+C interrupts instead of quitting.
    fn is_cancellable_work_active(&self) -> bool {
        self.bottom_pane.is_task_running() || self.is_review_mode
    }

    fn is_plan_streaming_in_tui(&self) -> bool {
        self.plan_stream_controller.is_some()
    }

    pub fn composer_is_empty(&self) -> bool {
        self.bottom_pane.composer_is_empty()
    }

    pub fn submit_user_message_with_mode(
        &mut self,
        text: String,
        mut collaboration_mode: CollaborationModeMask,
    ) {
        if collaboration_mode.mode == Some(ModeKind::Plan)
            && let Some(effort) = self.config.plan_mode_reasoning_effort
        {
            collaboration_mode.reasoning_effort = Some(Some(effort));
        }
        if self.agent_turn_running
            && self.active_collaboration_mask.as_ref() != Some(&collaboration_mode)
        {
            self.add_error_message(
                "Cannot switch collaboration mode while a turn is running.".to_string(),
            );
            return;
        }
        self.set_collaboration_mask(collaboration_mode);
        let should_queue = self.is_plan_streaming_in_tui();
        let user_message = UserMessage {
            text,
            local_images: Vec::new(),
            remote_image_urls: Vec::new(),
            text_elements: Vec::new(),
            mention_bindings: Vec::new(),
        };
        if should_queue {
            self.queue_user_message(user_message);
        } else {
            self.submit_user_message(user_message);
        }
    }

    /// True when the UI is in the regular composer state with no running task,
    /// no modal overlay (e.g. approvals or status indicator), and no composer popups.
    /// In this state Esc-Esc backtracking is enabled.
    pub fn is_normal_backtrack_mode(&self) -> bool {
        self.bottom_pane.is_normal_backtrack_mode()
    }

    pub fn insert_str(&mut self, text: &str) {
        self.bottom_pane.insert_str(text);
    }

    /// Replace the composer content with the provided text and reset cursor.
    pub fn set_composer_text(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
    ) {
        self.bottom_pane
            .set_composer_text(text, text_elements, local_image_paths);
    }

    pub fn set_remote_image_urls(&mut self, remote_image_urls: Vec<String>) {
        self.bottom_pane.set_remote_image_urls(remote_image_urls);
    }

    fn take_remote_image_urls(&mut self) -> Vec<String> {
        self.bottom_pane.take_remote_image_urls()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn remote_image_urls(&self) -> Vec<String> {
        self.bottom_pane.remote_image_urls()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn queued_user_message_texts(&self) -> Vec<String> {
        self.queued_user_messages
            .iter()
            .map(|message| message.text.clone())
            .collect()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn pending_process_approvals(&self) -> &[String] {
        self.bottom_pane.pending_process_approvals()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn has_active_view(&self) -> bool {
        self.bottom_pane.has_active_view()
    }

    pub fn show_esc_backtrack_hint(&mut self) {
        self.bottom_pane.show_esc_backtrack_hint();
    }

    pub fn clear_esc_backtrack_hint(&mut self) {
        self.bottom_pane.clear_esc_backtrack_hint();
    }
    /// Forward an `Op` directly to chaos.
    pub fn submit_op(&mut self, op: Op) -> bool {
        // Record outbound operation for session replay fidelity.
        crate::session_log::log_outbound_op(&op);
        if matches!(&op, Op::Review { .. }) && !self.bottom_pane.is_task_running() {
            self.bottom_pane.set_task_running(/*running*/ true);
        }
        if let Err(e) = self.chaos_op_tx.send(op) {
            tracing::error!("failed to submit op: {e}");
            return false;
        }
        true
    }

    fn on_all_tools_response(&mut self, ev: chaos_ipc::protocol::AllToolsResponseEvent) {
        // Forward to the app layer where the TileManager can open/populate the tools pane.
        self.app_event_tx.send(AppEvent::AllToolsReceived(ev));
    }

    fn on_list_mcp_tools(&mut self, ev: McpListToolsResponseEvent) {
        self.add_to_history(history_cell::new_mcp_tools_output(
            &self.config,
            ev.tools,
            ev.resources,
            ev.resource_templates,
            &ev.auth_statuses,
        ));
    }

    fn on_list_custom_prompts(&mut self, ev: ListCustomPromptsResponseEvent) {
        let len = ev.custom_prompts.len();
        debug!("received {len} custom prompts");
        // Forward to bottom pane so the slash popup can show them now.
        self.bottom_pane.set_custom_prompts(ev.custom_prompts);
    }

    pub fn on_connectors_loaded(
        &mut self,
        result: Result<ConnectorsSnapshot, String>,
        is_final: bool,
    ) {
        let mut trigger_pending_force_refetch = false;
        if is_final {
            self.connectors_prefetch_in_flight = false;
            if self.connectors_force_refetch_pending {
                self.connectors_force_refetch_pending = false;
                trigger_pending_force_refetch = true;
            }
        }

        match result {
            Ok(mut snapshot) => {
                // Connectors infrastructure removed — pass through as-is.
                if let ConnectorsCacheState::Ready(existing_snapshot) = &self.connectors_cache {
                    let enabled_by_id: HashMap<&str, bool> = existing_snapshot
                        .connectors
                        .iter()
                        .map(|connector| (connector.id.as_str(), connector.is_enabled))
                        .collect();
                    for connector in &mut snapshot.connectors {
                        if let Some(is_enabled) = enabled_by_id.get(connector.id.as_str()) {
                            connector.is_enabled = *is_enabled;
                        }
                    }
                }
                if is_final {
                    self.connectors_partial_snapshot = None;
                    self.refresh_connectors_popup_if_open(&snapshot.connectors);
                    self.connectors_cache = ConnectorsCacheState::Ready(snapshot.clone());
                } else {
                    self.connectors_partial_snapshot = Some(snapshot.clone());
                }
                self.bottom_pane.set_connectors_snapshot(Some(snapshot));
            }
            Err(err) => {
                let partial_snapshot = self.connectors_partial_snapshot.take();
                if let ConnectorsCacheState::Ready(snapshot) = &self.connectors_cache {
                    warn!("failed to refresh apps list; retaining current apps snapshot: {err}");
                    self.bottom_pane
                        .set_connectors_snapshot(Some(snapshot.clone()));
                } else if let Some(snapshot) = partial_snapshot {
                    warn!(
                        "failed to load full apps list; falling back to installed apps snapshot: {err}"
                    );
                    self.refresh_connectors_popup_if_open(&snapshot.connectors);
                    self.connectors_cache = ConnectorsCacheState::Ready(snapshot.clone());
                    self.bottom_pane.set_connectors_snapshot(Some(snapshot));
                } else {
                    self.connectors_cache = ConnectorsCacheState::Failed(err);
                    self.bottom_pane.set_connectors_snapshot(/*snapshot*/ None);
                }
            }
        }

        if trigger_pending_force_refetch {
            self.prefetch_connectors_with_options(/*force_refetch*/ true);
        }
    }

    pub fn update_connector_enabled(&mut self, connector_id: &str, enabled: bool) {
        let ConnectorsCacheState::Ready(mut snapshot) = self.connectors_cache.clone() else {
            return;
        };

        let mut changed = false;
        for connector in &mut snapshot.connectors {
            if connector.id == connector_id {
                changed = connector.is_enabled != enabled;
                connector.is_enabled = enabled;
                break;
            }
        }

        if !changed {
            return;
        }

        self.refresh_connectors_popup_if_open(&snapshot.connectors);
        self.connectors_cache = ConnectorsCacheState::Ready(snapshot.clone());
        self.bottom_pane.set_connectors_snapshot(Some(snapshot));
    }

    pub fn open_review_popup(&mut self) {
        let mut items: Vec<SelectionItem> = Vec::new();

        items.push(SelectionItem {
            name: "Review against a base branch".to_string(),
            description: Some("(PR Style)".into()),
            actions: vec![Box::new({
                let cwd = self.config.cwd.clone();
                move |tx| {
                    tx.send(AppEvent::OpenReviewBranchPicker(cwd.clone()));
                }
            })],
            dismiss_on_select: false,
            ..Default::default()
        });

        items.push(SelectionItem {
            name: "Review uncommitted changes".to_string(),
            actions: vec![Box::new(move |tx: &AppEventSender| {
                tx.send(AppEvent::ChaosOp(Op::Review {
                    review_request: ReviewRequest {
                        target: ReviewTarget::UncommittedChanges,
                        user_facing_hint: None,
                    },
                }));
            })],
            dismiss_on_select: true,
            ..Default::default()
        });

        // New: Review a specific commit (opens commit picker)
        items.push(SelectionItem {
            name: "Review a commit".to_string(),
            actions: vec![Box::new({
                let cwd = self.config.cwd.clone();
                move |tx| {
                    tx.send(AppEvent::OpenReviewCommitPicker(cwd.clone()));
                }
            })],
            dismiss_on_select: false,
            ..Default::default()
        });

        items.push(SelectionItem {
            name: "Custom review instructions".to_string(),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenReviewCustomPrompt);
            })],
            dismiss_on_select: false,
            ..Default::default()
        });

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select a review preset".into()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub async fn show_review_branch_picker(&mut self, cwd: &Path) {
        let branches = local_git_branches(cwd).await;
        let current_branch = current_branch_name(cwd)
            .await
            .unwrap_or_else(|| "(detached HEAD)".to_string());
        let mut items: Vec<SelectionItem> = Vec::with_capacity(branches.len());

        for option in branches {
            let branch = option.clone();
            items.push(SelectionItem {
                name: format!("{current_branch} -> {branch}"),
                actions: vec![Box::new(move |tx3: &AppEventSender| {
                    tx3.send(AppEvent::ChaosOp(Op::Review {
                        review_request: ReviewRequest {
                            target: ReviewTarget::BaseBranch {
                                branch: branch.clone(),
                            },
                            user_facing_hint: None,
                        },
                    }));
                })],
                dismiss_on_select: true,
                search_value: Some(option),
                ..Default::default()
            });
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select a base branch".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search branches".to_string()),
            ..Default::default()
        });
    }

    pub async fn show_review_commit_picker(&mut self, cwd: &Path) {
        let commits = chaos_kern::git_info::recent_commits(cwd, /*limit*/ 100).await;

        let mut items: Vec<SelectionItem> = Vec::with_capacity(commits.len());
        for entry in commits {
            let subject = entry.subject.clone();
            let sha = entry.sha.clone();
            let search_val = format!("{subject} {sha}");

            items.push(SelectionItem {
                name: subject.clone(),
                actions: vec![Box::new(move |tx3: &AppEventSender| {
                    tx3.send(AppEvent::ChaosOp(Op::Review {
                        review_request: ReviewRequest {
                            target: ReviewTarget::Commit {
                                sha: sha.clone(),
                                title: Some(subject.clone()),
                            },
                            user_facing_hint: None,
                        },
                    }));
                })],
                dismiss_on_select: true,
                search_value: Some(search_val),
                ..Default::default()
            });
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select a commit to review".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search commits".to_string()),
            ..Default::default()
        });
    }

    pub fn show_review_custom_prompt(&mut self) {
        let tx = self.app_event_tx.clone();
        let view = CustomPromptView::new(
            "Custom review instructions".to_string(),
            "Type instructions and press Enter".to_string(),
            /*context_label*/ None,
            Box::new(move |prompt: String| {
                let trimmed = prompt.trim().to_string();
                if trimmed.is_empty() {
                    return;
                }
                tx.send(AppEvent::ChaosOp(Op::Review {
                    review_request: ReviewRequest {
                        target: ReviewTarget::Custom {
                            instructions: trimmed,
                        },
                        user_facing_hint: None,
                    },
                }));
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    pub fn token_usage(&self) -> TokenUsage {
        self.token_info
            .as_ref()
            .map(|ti| ti.total_token_usage.clone())
            .unwrap_or_default()
    }

    pub fn process_id(&self) -> Option<ProcessId> {
        self.process_id
    }

    pub fn process_name(&self) -> Option<String> {
        self.process_name.clone()
    }

    /// Returns a cache key describing the current in-flight active cell for the transcript overlay.
    ///
    /// `Ctrl+T` renders committed transcript cells plus a render-only live tail derived from the
    /// current active cell, and the overlay caches that tail; this key is what it uses to decide
    /// whether it must recompute. When there is no active cell, this returns `None` so the overlay
    /// can drop the tail entirely.
    ///
    /// If callers mutate the active cell's transcript output without bumping the revision (or
    /// providing an appropriate animation tick), the overlay will keep showing a stale tail while
    /// the main viewport updates.
    pub fn active_cell_transcript_key(&self) -> Option<ActiveCellTranscriptKey> {
        let cell = self.active_cell.as_ref()?;
        Some(ActiveCellTranscriptKey {
            revision: self.active_cell_revision,
            is_stream_continuation: cell.is_stream_continuation(),
            animation_tick: cell.transcript_animation_tick(),
        })
    }

    /// Returns the active cell's transcript lines for a given terminal width.
    ///
    /// This is a convenience for the transcript overlay live-tail path, and it intentionally
    /// filters out empty results so the overlay can treat "nothing to render" as "no tail". Callers
    /// should pass the same width the overlay uses; using a different width will cause wrapping
    /// mismatches between the main viewport and the transcript overlay.
    pub fn active_cell_transcript_lines(&self, width: u16) -> Option<Vec<Line<'static>>> {
        let cell = self.active_cell.as_ref()?;
        let lines = cell.transcript_lines(width);
        (!lines.is_empty()).then_some(lines)
    }

    /// Return a reference to the widget's current config (includes any
    /// runtime overrides applied via TUI, e.g., model or approval policy).
    pub fn config_ref(&self) -> &Config {
        &self.config
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn status_line_text(&self) -> Option<String> {
        self.bottom_pane.status_line_text()
    }

    pub fn clear_token_usage(&mut self) {
        self.token_info = None;
    }
}
pub fn show_review_commit_picker_with_entries(
    chat: &mut ChatWidget,
    entries: Vec<chaos_kern::git_info::CommitLogEntry>,
) {
    let mut items: Vec<SelectionItem> = Vec::with_capacity(entries.len());
    for entry in entries {
        let subject = entry.subject.clone();
        let sha = entry.sha.clone();
        let search_val = format!("{subject} {sha}");

        items.push(SelectionItem {
            name: subject.clone(),
            actions: vec![Box::new(move |tx3: &AppEventSender| {
                tx3.send(AppEvent::ChaosOp(Op::Review {
                    review_request: ReviewRequest {
                        target: ReviewTarget::Commit {
                            sha: sha.clone(),
                            title: Some(subject.clone()),
                        },
                        user_facing_hint: None,
                    },
                }));
            })],
            dismiss_on_select: true,
            search_value: Some(search_val),
            ..Default::default()
        });
    }

    chat.bottom_pane.show_selection_view(SelectionViewParams {
        title: Some("Select a commit to review".to_string()),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        is_searchable: true,
        search_placeholder: Some("Type to search commits".to_string()),
        ..Default::default()
    });
}

// Items that tests.rs imports via `use super::{...}`. These were previously
// brought into scope by the monolithic chatwidget.rs and are now re-supplied
// here for test compilation without duplicating them in non-test builds.
#[cfg(any(test, feature = "testing"))]
use self::core::PendingSteerCompareKey;
#[cfg(any(test, feature = "testing"))]
use self::core::remap_placeholders_for_message;
#[cfg(any(test, feature = "testing"))]
use chaos_ipc::protocol::ErrorEvent;
#[cfg(any(test, feature = "testing"))]
use chaos_ipc::protocol::TurnAbortReason;
#[cfg(any(test, feature = "testing"))]
use chaos_ipc::protocol::UserMessageEvent;
#[cfg(any(test, feature = "testing"))]
use chaos_kern::config::types::Notifications;
#[cfg(any(test, feature = "testing"))]
use ratatui::buffer::Buffer;
#[cfg(any(test, feature = "testing"))]
use ratatui::layout::Rect;

#[cfg(any(test, feature = "testing"))]
pub mod tests;
