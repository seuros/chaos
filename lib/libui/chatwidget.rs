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
use crate::bottom_pane::CollaborationModeIndicator;
use crate::bottom_pane::ColumnWidthMode;
use crate::bottom_pane::LocalImageAttachment;
use crate::bottom_pane::MentionBinding;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::custom_prompt_view::CustomPromptView;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
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
mod state;
use self::session_header::SessionHeader;
mod dispatch;
mod popups;
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

    /// Enable clamp mode programmatically (e.g., from `--clamp` CLI flag).
    /// Equivalent to the user running `/clamp` from a fresh session.
    pub fn activate_clamp(&mut self) {
        crate::theme::set_clamped(true);
        self.capture_pre_clamp_selection();
        self.app_event_tx
            .send(AppEvent::ChaosOp(chaos_ipc::protocol::Op::SetClamped {
                enabled: true,
            }));
        self.add_info_message(
            "Clamped: using Claude Code MAX subscription as transport.".to_string(),
            Some("Type /clamp again to switch back".to_string()),
        );
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
