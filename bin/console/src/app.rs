use crate::app_backtrack::BacktrackState;
use crate::app_event::AppEvent;
use crate::app_event::ExitMode;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::ApprovalRequest;
use crate::bottom_pane::McpServerElicitationFormRequest;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::chatwidget::ChatWidget;
use crate::chatwidget::ExternalEditorState;
use crate::chatwidget::ProcessInputState;
use crate::cwd_prompt::CwdPromptAction;
use crate::diff_render::DiffSummary;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::external_editor;
use crate::file_search::FileSearchManager;
use crate::history_cell;
use crate::history_cell::HistoryCell;
use crate::multi_agents::agent_picker_status_dot_spans;
use crate::multi_agents::format_agent_picker_item_name;
use crate::multi_agents::next_agent_shortcut_matches;
use crate::multi_agents::previous_agent_shortcut_matches;
use crate::pager_overlay::Overlay;
use crate::panes::tool_list::ToolListPane;
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::renderable::Renderable;
use crate::resume_picker::SessionSelection;
use crate::side_panel::LOG_PANEL_BACKFILL_LIMIT;
use crate::side_panel::LogPanelState;
use crate::tile_manager::PaneKind;
use crate::tile_manager::TileManager;
use crate::tui;
use crate::tui::TuiEvent;
use chaos_ipc::ProcessId;
use chaos_ipc::api::ConfigLayerSource;
use chaos_ipc::config_types::Personality;
use chaos_ipc::items::TurnItem;
use chaos_ipc::openai_models::ReasoningEffort as ReasoningEffortConfig;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::FinalOutput;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SessionConfiguredEvent;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::TokenUsage;
use chaos_kern::AuthManager;
use chaos_kern::ProcessTable;
use chaos_kern::config::Config;
use chaos_kern::config::ConfigBuilder;
use chaos_kern::config::ConfigOverrides;
use chaos_kern::config::edit::ConfigEdit;
use chaos_kern::config::edit::ConfigEditsBuilder;

use chaos_kern::config_loader::ConfigLayerStackOrdering;
use chaos_kern::features::Feature;
use chaos_kern::models_manager::manager::RefreshStrategy;
use chaos_proc::LogQuery;
use chaos_proc::StateRuntime;
use chaos_realpath::AbsolutePathBuf;
use chaos_syslog::SessionTelemetry;
use chaos_syslog::TelemetryAuthMode;
use chaos_termcap::ansi_escape_line;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use ratatui_hypertile::HypertileEvent;
use ratatui_hypertile::PaneId;
use ratatui_hypertile_extras::keychord_from_crossterm;
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;
use toml::Value as TomlValue;

mod agent_management;
mod agent_navigation;
mod config_management;
mod editor_integration;
mod event_dispatch;
mod interactive_requests;
mod key_handling;
mod log_panel;
mod pending_interactive_replay;
mod process_routing;
mod session_lifecycle;
mod ui_helpers;

use self::agent_navigation::AgentNavigationDirection;
use self::agent_navigation::AgentNavigationState;
use self::pending_interactive_replay::PendingInteractiveReplayState;

const EXTERNAL_EDITOR_HINT: &str = "Save and close external editor to continue.";
const PROCESS_EVENT_CHANNEL_CAPACITY: usize = 32768;

enum ProcessInteractiveRequest {
    Approval(ApprovalRequest),
    McpServerElicitation(McpServerElicitationFormRequest),
}

/// Baseline cadence for periodic stream commit animation ticks.
///
/// Smooth-mode streaming drains one line per tick, so this interval controls
/// perceived typing speed for non-backlogged output.
const COMMIT_ANIMATION_TICK: Duration = tui::TARGET_FRAME_INTERVAL;

#[derive(Debug, Clone)]
pub struct AppExitInfo {
    pub token_usage: TokenUsage,
    pub process_id: Option<ProcessId>,
    pub process_name: Option<String>,
    pub exit_reason: ExitReason,
}

impl AppExitInfo {
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            token_usage: TokenUsage::default(),
            process_id: None,
            process_name: None,
            exit_reason: ExitReason::Fatal(message.into()),
        }
    }
}

#[derive(Debug)]
pub(crate) enum AppRunControl {
    Continue,
    Exit(ExitReason),
}

#[derive(Debug, Clone)]
pub enum ExitReason {
    UserRequested,
    Fatal(String),
}

fn session_summary(
    token_usage: TokenUsage,
    process_id: Option<ProcessId>,
    process_name: Option<String>,
) -> Option<SessionSummary> {
    if token_usage.is_zero() {
        return None;
    }

    let usage_line = FinalOutput::from(token_usage).to_string();
    let resume_command = chaos_kern::util::resume_command(process_name.as_deref(), process_id);
    Some(SessionSummary {
        usage_line,
        resume_command,
    })
}

fn emit_project_config_warnings(app_event_tx: &AppEventSender, config: &Config) {
    let mut disabled_folders = Vec::new();

    for layer in config.config_layer_stack.get_layers(
        ConfigLayerStackOrdering::LowestPrecedenceFirst,
        /*include_disabled*/ true,
    ) {
        let ConfigLayerSource::Project { dot_codex_folder } = &layer.name else {
            continue;
        };
        if layer.disabled_reason.is_none() {
            continue;
        }
        disabled_folders.push((
            dot_codex_folder.as_path().display().to_string(),
            layer
                .disabled_reason
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "config.toml is disabled.".to_string()),
        ));
    }

    if disabled_folders.is_empty() {
        return;
    }

    let mut message = concat!(
        "Project config.toml files are disabled in the following folders. ",
        "Settings in those files are ignored, but skills and exec policies still load.\n",
    )
    .to_string();
    for (index, (folder, reason)) in disabled_folders.iter().enumerate() {
        let display_index = index + 1;
        message.push_str(&format!("    {display_index}. {folder}\n"));
        message.push_str(&format!("       {reason}\n"));
    }

    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
        history_cell::new_warning_event(message),
    )));
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionSummary {
    usage_line: String,
    resume_command: Option<String>,
}

#[derive(Debug, Clone)]
struct ProcessEventSnapshot {
    session_configured: Option<Event>,
    events: Vec<Event>,
    input_state: Option<ProcessInputState>,
}

#[derive(Debug)]
struct ProcessEventStore {
    session_configured: Option<Event>,
    buffer: VecDeque<Event>,
    user_message_ids: HashSet<String>,
    pending_interactive_replay: PendingInteractiveReplayState,
    input_state: Option<ProcessInputState>,
    capacity: usize,
    active: bool,
}

impl ProcessEventStore {
    fn new(capacity: usize) -> Self {
        Self {
            session_configured: None,
            buffer: VecDeque::new(),
            user_message_ids: HashSet::new(),
            pending_interactive_replay: PendingInteractiveReplayState::default(),
            input_state: None,
            capacity,
            active: false,
        }
    }

    fn new_with_session_configured(capacity: usize, event: Event) -> Self {
        let mut store = Self::new(capacity);
        store.session_configured = Some(event);
        store
    }

    fn push_event(&mut self, event: Event) {
        self.pending_interactive_replay.note_event(&event);
        match &event.msg {
            EventMsg::SessionConfigured(_) => {
                self.session_configured = Some(event);
                return;
            }
            EventMsg::ItemCompleted(completed) => {
                if let TurnItem::UserMessage(item) = &completed.item {
                    if !event.id.is_empty() && self.user_message_ids.contains(&event.id) {
                        return;
                    }
                    let user_msg_event = Event {
                        id: event.id,
                        msg: EventMsg::UserMessage(item.to_user_message_event()),
                    };
                    self.push_legacy_event(user_msg_event);
                    return;
                }
            }
            _ => {}
        }

        self.push_legacy_event(event);
    }

    fn push_legacy_event(&mut self, event: Event) {
        if let EventMsg::UserMessage(_) = &event.msg
            && !event.id.is_empty()
            && !self.user_message_ids.insert(event.id.clone())
        {
            return;
        }
        self.buffer.push_back(event);
        if self.buffer.len() > self.capacity
            && let Some(removed) = self.buffer.pop_front()
        {
            self.pending_interactive_replay.note_evicted_event(&removed);
            if matches!(removed.msg, EventMsg::UserMessage(_)) && !removed.id.is_empty() {
                self.user_message_ids.remove(&removed.id);
            }
        }
    }

    fn snapshot(&self) -> ProcessEventSnapshot {
        ProcessEventSnapshot {
            session_configured: self.session_configured.clone(),
            // Thread switches replay buffered events into a rebuilt ChatWidget. Only replay
            // interactive prompts that are still pending, or answered approvals/input will reappear.
            events: self
                .buffer
                .iter()
                .filter(|event| {
                    self.pending_interactive_replay
                        .should_replay_snapshot_event(event)
                })
                .cloned()
                .collect(),
            input_state: self.input_state.clone(),
        }
    }

    fn note_outbound_op(&mut self, op: &Op) {
        self.pending_interactive_replay.note_outbound_op(op);
    }

    fn op_can_change_pending_replay_state(op: &Op) -> bool {
        PendingInteractiveReplayState::op_can_change_state(op)
    }

    fn event_can_change_pending_process_approvals(event: &Event) -> bool {
        PendingInteractiveReplayState::event_can_change_pending_process_approvals(event)
    }

    fn has_pending_process_approvals(&self) -> bool {
        self.pending_interactive_replay
            .has_pending_process_approvals()
    }
}

#[derive(Debug)]
struct ProcessEventChannel {
    sender: mpsc::Sender<Event>,
    receiver: Option<mpsc::Receiver<Event>>,
    store: Arc<Mutex<ProcessEventStore>>,
}

impl ProcessEventChannel {
    fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(ProcessEventStore::new(capacity))),
        }
    }

    fn new_with_session_configured(capacity: usize, event: Event) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(ProcessEventStore::new_with_session_configured(
                capacity, event,
            ))),
        }
    }
}

pub(crate) struct App {
    pub(crate) server: Arc<ProcessTable>,
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) chat_widget: ChatWidget,
    pub(crate) auth_manager: Arc<AuthManager>,
    /// Config is stored here so we can recreate ChatWidgets as needed.
    pub(crate) config: Config,
    pub(crate) active_profile: Option<String>,
    cli_kv_overrides: Vec<(String, TomlValue)>,
    harness_overrides: ConfigOverrides,
    runtime_approval_policy_override: Option<ApprovalPolicy>,
    runtime_sandbox_policy_override: Option<SandboxPolicy>,

    pub(crate) tile_manager: TileManager,
    pub(crate) tool_list_pane: Rc<RefCell<ToolListPane>>,
    tool_list_close: Rc<Cell<bool>>,
    pub(crate) file_search: FileSearchManager,
    log_state_db: Option<Arc<StateRuntime>>,
    log_state_db_init_error: Option<String>,
    log_panel: LogPanelState,

    pub(crate) transcript_cells: Vec<Arc<dyn HistoryCell>>,

    // Pager overlay state (Transcript or Static like Diff)
    pub(crate) overlay: Option<Overlay>,
    pub(crate) deferred_history_lines: Vec<Line<'static>>,
    has_emitted_history_lines: bool,

    pub(crate) enhanced_keys_supported: bool,

    /// Controls the animation thread that sends CommitTick events.
    pub(crate) commit_anim_running: Arc<AtomicBool>,
    // Shared across ChatWidget instances so invalid status-line config warnings only emit once.
    status_line_invalid_items_warned: Arc<AtomicBool>,

    // Esc-backtracking state grouped
    pub(crate) backtrack: crate::app_backtrack::BacktrackState,
    /// When set, the next draw re-renders the transcript into terminal scrollback once.
    ///
    /// This is used after a confirmed thread rollback to ensure scrollback reflects the trimmed
    /// transcript cells.
    pub(crate) backtrack_render_pending: bool,
    /// One-shot guard used while switching threads.
    ///
    /// We set this when intentionally stopping the current thread before moving
    /// to another one, then ignore exactly one `ShutdownComplete` so it is not
    /// misclassified as an unexpected sub-agent death.
    suppress_shutdown_complete: bool,
    /// Tracks the thread we intentionally shut down while exiting the app.
    ///
    /// When this matches the active thread, its `ShutdownComplete` should lead to
    /// process exit instead of being treated as an unexpected sub-agent death that
    /// triggers failover to the primary thread.
    ///
    /// This is process-scoped state (`Option<ProcessId>`) instead of a global bool
    /// so shutdown events from other threads still take the normal failover path.
    pending_shutdown_exit_process_id: Option<ProcessId>,

    process_event_channels: HashMap<ProcessId, ProcessEventChannel>,
    process_event_listener_tasks: HashMap<ProcessId, JoinHandle<()>>,
    agent_navigation: AgentNavigationState,
    active_process_id: Option<ProcessId>,
    active_process_rx: Option<mpsc::Receiver<Event>>,
    primary_process_id: Option<ProcessId>,
    primary_session_configured: Option<SessionConfiguredEvent>,
    pending_primary_events: VecDeque<Event>,
}

fn normalize_harness_overrides_for_cwd(
    mut overrides: ConfigOverrides,
    base_cwd: &Path,
) -> Result<ConfigOverrides> {
    if overrides.additional_writable_roots.is_empty() {
        return Ok(overrides);
    }

    let mut normalized = Vec::with_capacity(overrides.additional_writable_roots.len());
    for root in overrides.additional_writable_roots.drain(..) {
        let absolute = AbsolutePathBuf::resolve_path_against_base(root, base_cwd)?;
        normalized.push(absolute.into_path_buf());
    }
    overrides.additional_writable_roots = normalized;
    Ok(overrides)
}

#[cfg(test)]
#[path = "app/app_tests.rs"]
mod tests;
