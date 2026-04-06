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
use crate::onboarding::auth::AuthModeWidget;
use crate::pager_overlay::Overlay;
use crate::panes::tool_list::ToolListPane;
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::renderable::Renderable;
use crate::resume_picker::SessionSelection;
use crate::side_panel::LOG_PANEL_BACKFILL_LIMIT;
use crate::side_panel::LOG_PANEL_POLL_INTERVAL;
use crate::side_panel::LogPanelState;
use crate::side_panel::split_main_and_panel;
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
use chaos_kern::ChaosAuth;
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
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;
use toml::Value as TomlValue;

mod agent_navigation;
mod pending_interactive_replay;

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

impl App {
    pub fn chatwidget_init_for_forked_or_resumed_process(
        &self,
        tui: &mut tui::Tui,
        cfg: chaos_kern::config::Config,
    ) -> crate::chatwidget::ChatWidgetInit {
        crate::chatwidget::ChatWidgetInit {
            config: cfg,
            frame_requester: tui.frame_requester(),
            app_event_tx: self.app_event_tx.clone(),
            // Fork/resume bootstraps here don't carry any prefilled message content.
            initial_user_message: None,
            enhanced_keys_supported: self.enhanced_keys_supported,
            auth_manager: self.auth_manager.clone(),
            models_manager: self.server.get_models_manager(),
            is_first_run: false,
            model: Some(self.chat_widget.current_model().to_string()),
            status_line_invalid_items_warned: self.status_line_invalid_items_warned.clone(),
            session_telemetry: self.session_telemetry.clone(),
        }
    }

    async fn rebuild_config_for_cwd(&self, cwd: PathBuf) -> Result<Config> {
        let mut overrides = self.harness_overrides.clone();
        overrides.cwd = Some(cwd.clone());
        let cwd_display = cwd.display().to_string();
        ConfigBuilder::default()
            .chaos_home(self.config.chaos_home.clone())
            .cli_overrides(self.cli_kv_overrides.clone())
            .harness_overrides(overrides)
            .build()
            .await
            .wrap_err_with(|| format!("Failed to rebuild config for cwd {cwd_display}"))
    }

    async fn refresh_in_memory_config_from_disk(&mut self) -> Result<()> {
        let mut config = self
            .rebuild_config_for_cwd(self.chat_widget.config_ref().cwd.clone())
            .await?;
        self.apply_runtime_policy_overrides(&mut config);
        self.config = config;
        Ok(())
    }

    async fn refresh_in_memory_config_from_disk_best_effort(&mut self, action: &str) {
        if let Err(err) = self.refresh_in_memory_config_from_disk().await {
            tracing::warn!(
                error = %err,
                action,
                "failed to refresh config before process transition; continuing with current in-memory config"
            );
        }
    }

    async fn rebuild_config_for_resume_or_fallback(
        &mut self,
        current_cwd: &Path,
        resume_cwd: PathBuf,
    ) -> Result<Config> {
        match self.rebuild_config_for_cwd(resume_cwd.clone()).await {
            Ok(config) => Ok(config),
            Err(err) => {
                if crate::cwds_differ(current_cwd, &resume_cwd) {
                    Err(err)
                } else {
                    let resume_cwd_display = resume_cwd.display().to_string();
                    tracing::warn!(
                        error = %err,
                        cwd = %resume_cwd_display,
                        "failed to rebuild config for same-cwd resume; using current in-memory config"
                    );
                    Ok(self.config.clone())
                }
            }
        }
    }

    fn apply_runtime_policy_overrides(&mut self, config: &mut Config) {
        if let Some(policy) = self.runtime_approval_policy_override.as_ref()
            && let Err(err) = config.permissions.approval_policy.set(*policy)
        {
            tracing::warn!(%err, "failed to carry forward approval policy override");
            self.chat_widget.add_error_message(format!(
                "Failed to carry forward approval policy override: {err}"
            ));
        }
        if let Some(policy) = self.runtime_sandbox_policy_override.as_ref()
            && let Err(err) = config.permissions.sandbox_policy.set(policy.clone())
        {
            tracing::warn!(%err, "failed to carry forward sandbox policy override");
            self.chat_widget.add_error_message(format!(
                "Failed to carry forward sandbox policy override: {err}"
            ));
        }
    }

    fn try_set_approval_policy_on_config(
        &mut self,
        config: &mut Config,
        policy: ApprovalPolicy,
        user_message_prefix: &str,
        log_message: &str,
    ) -> bool {
        if let Err(err) = config.permissions.approval_policy.set(policy) {
            tracing::warn!(error = %err, "{log_message}");
            self.chat_widget
                .add_error_message(format!("{user_message_prefix}: {err}"));
            return false;
        }

        true
    }

    fn try_set_sandbox_policy_on_config(
        &mut self,
        config: &mut Config,
        policy: SandboxPolicy,
        user_message_prefix: &str,
        log_message: &str,
    ) -> bool {
        if let Err(err) = config.permissions.sandbox_policy.set(policy) {
            tracing::warn!(error = %err, "{log_message}");
            self.chat_widget
                .add_error_message(format!("{user_message_prefix}: {err}"));
            return false;
        }

        true
    }

    async fn update_feature_flags(&mut self, updates: Vec<(Feature, bool)>) {
        if updates.is_empty() {
            return;
        }

        let mut next_config = self.config.clone();
        let mut feature_updates_to_apply = Vec::with_capacity(updates.len());
        let permissions_history_label: Option<&'static str> = None;
        let mut builder = ConfigEditsBuilder::new(&self.config.chaos_home)
            .with_profile(self.active_profile.as_deref());

        for (feature, enabled) in updates {
            let feature_key = feature.key();
            let feature_edits = Vec::new();
            let mut feature_config = next_config.clone();
            if let Err(err) = feature_config.features.set_enabled(feature, enabled) {
                tracing::error!(
                    error = %err,
                    feature = feature_key,
                    "failed to update constrained feature flags"
                );
                self.chat_widget.add_error_message(format!(
                    "Failed to update experimental feature `{feature_key}`: {err}"
                ));
                continue;
            }
            let effective_enabled = feature_config.features.enabled(feature);

            next_config = feature_config;
            feature_updates_to_apply.push((feature, effective_enabled));
            builder = builder
                .with_edits(feature_edits)
                .set_feature_enabled(feature_key, effective_enabled);
        }

        // Persist first so the live session does not diverge from disk if the
        // config edit fails. Runtime/UI state is patched below only after the
        // durable config update succeeds.
        if let Err(err) = builder.apply().await {
            tracing::error!(error = %err, "failed to persist feature flags");
            self.chat_widget
                .add_error_message(format!("Failed to update experimental features: {err}"));
            return;
        }

        self.config = next_config;
        for (feature, effective_enabled) in feature_updates_to_apply {
            self.chat_widget
                .set_feature_enabled(feature, effective_enabled);
        }

        if let Some(label) = permissions_history_label {
            self.chat_widget.add_info_message(
                format!("Permissions updated to {label}"),
                /*hint*/ None,
            );
        }
    }

    fn open_url_in_browser(&mut self, url: String) -> bool {
        if let Err(err) = webbrowser::open(&url) {
            self.chat_widget
                .add_error_message(format!("Failed to open browser for {url}: {err}"));
            return false;
        }

        self.chat_widget
            .add_info_message(format!("Opened {url} in your browser."), /*hint*/ None);
        true
    }

    fn clear_ui_header_lines(&self, _width: u16) -> Vec<Line<'static>> {
        Vec::new()
    }

    fn queue_clear_ui_header(&mut self, tui: &mut tui::Tui) {
        let width = tui.terminal.last_known_screen_size.width;
        let header_lines = self.clear_ui_header_lines(width);
        if !header_lines.is_empty() {
            tui.insert_history_lines(header_lines);
            self.has_emitted_history_lines = true;
        }
    }

    fn clear_terminal_ui(&mut self, tui: &mut tui::Tui, redraw_header: bool) -> Result<()> {
        let is_alt_screen_active = tui.is_alt_screen_active();

        // Drop queued history insertions so stale transcript lines cannot be flushed after /clear.
        tui.clear_pending_history_lines();

        if is_alt_screen_active {
            tui.terminal.clear_visible_screen()?;
        } else {
            // Some terminals (Terminal.app, Warp) do not reliably drop scrollback when purge and
            // clear are emitted as separate backend commands. Prefer a single ANSI sequence.
            tui.terminal.clear_scrollback_and_visible_screen_ansi()?;
        }

        let mut area = tui.terminal.viewport_area;
        if area.y > 0 {
            // After a full clear, anchor the inline viewport at the top and redraw a fresh header
            // box. `insert_history_lines()` will shift the viewport down by the rendered height.
            area.y = 0;
            tui.terminal.set_viewport_area(area);
        }
        self.has_emitted_history_lines = false;

        if redraw_header {
            self.queue_clear_ui_header(tui);
        }
        Ok(())
    }

    fn reset_app_ui_state_after_clear(&mut self) {
        self.overlay = None;
        self.transcript_cells.clear();
        self.deferred_history_lines.clear();
        self.has_emitted_history_lines = false;
        self.backtrack = BacktrackState::default();
        self.backtrack_render_pending = false;
    }

    async fn shutdown_current_process(&mut self) {
        if let Some(process_id) = self.chat_widget.process_id() {
            // Clear any in-flight rollback guard when switching processes.
            self.backtrack.pending_rollback = None;
            self.suppress_shutdown_complete = true;
            self.chat_widget.submit_op(Op::Shutdown);
            self.server.remove_process(&process_id).await;
            self.abort_process_event_listener(process_id);
        }
    }

    fn abort_process_event_listener(&mut self, process_id: ProcessId) {
        if let Some(handle) = self.process_event_listener_tasks.remove(&process_id) {
            handle.abort();
        }
    }

    fn abort_all_process_event_listeners(&mut self) {
        for handle in self
            .process_event_listener_tasks
            .drain()
            .map(|(_, handle)| handle)
        {
            handle.abort();
        }
    }

    fn ensure_process_channel(&mut self, process_id: ProcessId) -> &mut ProcessEventChannel {
        self.process_event_channels
            .entry(process_id)
            .or_insert_with(|| ProcessEventChannel::new(PROCESS_EVENT_CHANNEL_CAPACITY))
    }

    async fn set_process_active(&mut self, process_id: ProcessId, active: bool) {
        if let Some(channel) = self.process_event_channels.get_mut(&process_id) {
            let mut store = channel.store.lock().await;
            store.active = active;
        }
    }

    async fn activate_process_channel(&mut self, process_id: ProcessId) {
        if self.active_process_id.is_some() {
            return;
        }
        self.set_process_active(process_id, /*active*/ true).await;
        let receiver = if let Some(channel) = self.process_event_channels.get_mut(&process_id) {
            channel.receiver.take()
        } else {
            None
        };
        self.active_process_id = Some(process_id);
        self.active_process_rx = receiver;
        self.refresh_pending_process_approvals().await;
    }

    async fn store_active_process_receiver(&mut self) {
        let Some(active_id) = self.active_process_id else {
            return;
        };
        let input_state = self.chat_widget.capture_process_input_state();
        if let Some(channel) = self.process_event_channels.get_mut(&active_id) {
            let receiver = self.active_process_rx.take();
            let mut store = channel.store.lock().await;
            store.active = false;
            store.input_state = input_state;
            if let Some(receiver) = receiver {
                channel.receiver = Some(receiver);
            }
        }
    }

    async fn activate_process_for_replay(
        &mut self,
        process_id: ProcessId,
    ) -> Option<(mpsc::Receiver<Event>, ProcessEventSnapshot)> {
        let channel = self.process_event_channels.get_mut(&process_id)?;
        let receiver = channel.receiver.take()?;
        let mut store = channel.store.lock().await;
        store.active = true;
        let snapshot = store.snapshot();
        Some((receiver, snapshot))
    }

    async fn clear_active_thread(&mut self) {
        if let Some(active_id) = self.active_process_id.take() {
            self.set_process_active(active_id, /*active*/ false).await;
        }
        self.active_process_rx = None;
        self.refresh_pending_process_approvals().await;
    }

    async fn note_process_outbound_op(&mut self, process_id: ProcessId, op: &Op) {
        let Some(channel) = self.process_event_channels.get(&process_id) else {
            return;
        };
        let mut store = channel.store.lock().await;
        store.note_outbound_op(op);
    }

    async fn note_active_process_outbound_op(&mut self, op: &Op) {
        if !ProcessEventStore::op_can_change_pending_replay_state(op) {
            return;
        }
        let Some(process_id) = self.active_process_id else {
            return;
        };
        self.note_process_outbound_op(process_id, op).await;
    }

    fn process_label(&self, process_id: ProcessId) -> String {
        let is_primary = self.primary_process_id == Some(process_id);
        let fallback_label = if is_primary {
            "Main [default]".to_string()
        } else {
            let process_id = process_id.to_string();
            let short_id: String = process_id.chars().take(8).collect();
            format!("Agent ({short_id})")
        };
        if let Some(entry) = self.agent_navigation.get(&process_id) {
            let label = format_agent_picker_item_name(
                entry.agent_nickname.as_deref(),
                entry.agent_role.as_deref(),
                is_primary,
            );
            if label == "Agent" {
                let process_id = process_id.to_string();
                let short_id: String = process_id.chars().take(8).collect();
                format!("{label} ({short_id})")
            } else {
                label
            }
        } else {
            fallback_label
        }
    }

    /// Returns the thread whose transcript is currently on screen.
    ///
    /// `active_process_id` is the source of truth during steady state, but the widget can briefly
    /// lag behind thread bookkeeping during transitions. The footer label and adjacent-thread
    /// navigation both follow what the user is actually looking at, not whichever thread most
    /// recently began switching.
    fn current_displayed_process_id(&self) -> Option<ProcessId> {
        self.active_process_id.or(self.chat_widget.process_id())
    }

    /// Mirrors the visible thread into the contextual footer row.
    ///
    /// The footer sometimes shows ambient context instead of an instructional hint. In multi-agent
    /// sessions, that contextual row includes the currently viewed agent label. The label is
    /// intentionally hidden until there is more than one known thread so single-thread sessions do
    /// not spend footer space restating that the user is already on the main conversation.
    fn sync_active_agent_label(&mut self) {
        let label = self
            .agent_navigation
            .active_agent_label(self.current_displayed_process_id(), self.primary_process_id);
        self.chat_widget.set_active_agent_label(label);
    }

    async fn process_cwd(&self, process_id: ProcessId) -> Option<PathBuf> {
        let channel = self.process_event_channels.get(&process_id)?;
        let store = channel.store.lock().await;
        match store.session_configured.as_ref().map(|event| &event.msg) {
            Some(EventMsg::SessionConfigured(session)) => Some(session.cwd.clone()),
            _ => None,
        }
    }

    async fn interactive_request_for_process_event(
        &self,
        process_id: ProcessId,
        event: &Event,
    ) -> Option<ProcessInteractiveRequest> {
        let process_label = Some(self.process_label(process_id));
        match &event.msg {
            EventMsg::ExecApprovalRequest(ev) => {
                Some(ProcessInteractiveRequest::Approval(ApprovalRequest::Exec {
                    process_id,
                    process_label,
                    id: ev.effective_approval_id(),
                    command: ev.command.clone(),
                    reason: ev.reason.clone(),
                    available_decisions: ev.effective_available_decisions(),
                    network_approval_context: ev.network_approval_context.clone(),
                    additional_permissions: ev.additional_permissions.clone(),
                }))
            }
            EventMsg::ApplyPatchApprovalRequest(ev) => Some(ProcessInteractiveRequest::Approval(
                ApprovalRequest::ApplyPatch {
                    process_id,
                    process_label,
                    id: ev.call_id.clone(),
                    reason: ev.reason.clone(),
                    cwd: self
                        .process_cwd(process_id)
                        .await
                        .unwrap_or_else(|| self.config.cwd.clone()),
                    changes: ev.changes.clone(),
                },
            )),
            EventMsg::ElicitationRequest(ev) => {
                if let Some(request) =
                    McpServerElicitationFormRequest::from_event(process_id, ev.clone())
                {
                    Some(ProcessInteractiveRequest::McpServerElicitation(request))
                } else {
                    let url = match &ev.request {
                        chaos_ipc::approvals::ElicitationRequest::Url { url, .. } => {
                            Some(url.clone())
                        }
                        chaos_ipc::approvals::ElicitationRequest::Form { .. } => None,
                    };
                    Some(ProcessInteractiveRequest::Approval(
                        ApprovalRequest::McpElicitation {
                            process_id,
                            process_label,
                            server_name: ev.server_name.clone(),
                            request_id: ev.id.clone(),
                            message: ev.request.message().to_string(),
                            url,
                        },
                    ))
                }
            }
            EventMsg::RequestPermissions(ev) => Some(ProcessInteractiveRequest::Approval(
                ApprovalRequest::Permissions {
                    process_id,
                    process_label,
                    call_id: ev.call_id.clone(),
                    reason: ev.reason.clone(),
                    permissions: ev.permissions.clone(),
                },
            )),
            _ => None,
        }
    }

    async fn submit_op_to_process(&mut self, process_id: ProcessId, op: Op) {
        let replay_state_op =
            ProcessEventStore::op_can_change_pending_replay_state(&op).then(|| op.clone());
        let submitted = if self.active_process_id == Some(process_id) {
            self.chat_widget.submit_op(op)
        } else {
            crate::session_log::log_outbound_op(&op);
            match self.server.get_process(process_id).await {
                Ok(thread) => match thread.submit(op).await {
                    Ok(_) => true,
                    Err(err) => {
                        self.chat_widget.add_error_message(format!(
                            "Failed to submit op to process {process_id}: {err}"
                        ));
                        false
                    }
                },
                Err(err) => {
                    self.chat_widget.add_error_message(format!(
                        "Failed to find process {process_id} for approval response: {err}"
                    ));
                    false
                }
            }
        };
        if submitted && let Some(op) = replay_state_op.as_ref() {
            self.note_process_outbound_op(process_id, op).await;
            self.refresh_pending_process_approvals().await;
        }
    }

    async fn refresh_pending_process_approvals(&mut self) {
        let channels: Vec<(ProcessId, Arc<Mutex<ProcessEventStore>>)> = self
            .process_event_channels
            .iter()
            .map(|(process_id, channel)| (*process_id, Arc::clone(&channel.store)))
            .collect();

        let mut pending_process_ids = Vec::new();
        for (process_id, store) in channels {
            if Some(process_id) == self.active_process_id {
                continue;
            }

            let store = store.lock().await;
            if store.has_pending_process_approvals() {
                pending_process_ids.push(process_id);
            }
        }

        pending_process_ids.sort_by_key(ProcessId::to_string);

        let threads = pending_process_ids
            .into_iter()
            .map(|process_id| self.process_label(process_id))
            .collect();

        self.chat_widget.set_pending_process_approvals(threads);
    }

    async fn enqueue_process_event(&mut self, process_id: ProcessId, event: Event) -> Result<()> {
        let refresh_pending_process_approvals =
            ProcessEventStore::event_can_change_pending_process_approvals(&event);
        let inactive_interactive_request = if self.active_process_id != Some(process_id) {
            self.interactive_request_for_process_event(process_id, &event)
                .await
        } else {
            None
        };
        let (sender, store) = {
            let channel = self.ensure_process_channel(process_id);
            (channel.sender.clone(), Arc::clone(&channel.store))
        };

        let should_send = {
            let mut guard = store.lock().await;
            guard.push_event(event.clone());
            guard.active
        };

        if should_send {
            // Never await a bounded channel send on the main TUI loop: if the receiver falls behind,
            // `send().await` can block and the UI stops drawing. If the channel is full, wait in a
            // spawned task instead.
            match sender.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    tokio::spawn(async move {
                        if let Err(err) = sender.send(event).await {
                            tracing::warn!("process {process_id} event channel closed: {err}");
                        }
                    });
                }
                Err(TrySendError::Closed(_)) => {
                    tracing::warn!("process {process_id} event channel closed");
                }
            }
        } else if let Some(request) = inactive_interactive_request {
            match request {
                ProcessInteractiveRequest::Approval(request) => {
                    self.chat_widget.push_approval_request(request);
                }
                ProcessInteractiveRequest::McpServerElicitation(request) => {
                    self.chat_widget
                        .push_mcp_server_elicitation_request(request);
                }
            }
        }
        if refresh_pending_process_approvals {
            self.refresh_pending_process_approvals().await;
        }
        Ok(())
    }

    async fn handle_routed_process_event(
        &mut self,
        process_id: ProcessId,
        event: Event,
    ) -> Result<()> {
        if !self.process_event_channels.contains_key(&process_id) {
            tracing::debug!("dropping stale event for untracked process {process_id}");
            return Ok(());
        }

        self.enqueue_process_event(process_id, event).await
    }

    async fn enqueue_primary_event(&mut self, event: Event) -> Result<()> {
        if let Some(process_id) = self.primary_process_id {
            return self.enqueue_process_event(process_id, event).await;
        }

        if let EventMsg::SessionConfigured(session) = &event.msg {
            let process_id = session.session_id;
            self.primary_process_id = Some(process_id);
            self.primary_session_configured = Some(session.clone());
            self.upsert_agent_picker_thread(
                process_id, /*agent_nickname*/ None, /*agent_role*/ None,
                /*is_closed*/ false,
            );
            self.ensure_process_channel(process_id);
            self.activate_process_channel(process_id).await;
            self.enqueue_process_event(process_id, event).await?;

            let pending = std::mem::take(&mut self.pending_primary_events);
            for pending_event in pending {
                self.enqueue_process_event(process_id, pending_event)
                    .await?;
            }
        } else {
            self.pending_primary_events.push_back(event);
        }
        Ok(())
    }

    /// Opens the `/agent` picker after refreshing cached labels for known threads.
    ///
    /// The picker state is derived from long-lived thread channels plus best-effort metadata
    /// refreshes from the backend. Refresh failures are treated as "thread is only inspectable by
    /// historical id now" and converted into closed picker entries instead of deleting them, so
    /// the stable traversal order remains intact for review and keyboard navigation.
    async fn open_agent_picker(&mut self) {
        let process_ids: Vec<ProcessId> = self.process_event_channels.keys().cloned().collect();
        for process_id in process_ids {
            match self.server.get_process(process_id).await {
                Ok(thread) => {
                    let session_source = thread.config_snapshot().await.session_source;
                    self.upsert_agent_picker_thread(
                        process_id,
                        session_source.get_nickname(),
                        session_source.get_agent_role(),
                        /*is_closed*/ false,
                    );
                }
                Err(_) => {
                    self.mark_agent_picker_process_closed(process_id);
                }
            }
        }

        let has_non_primary_agent_process = self
            .agent_navigation
            .has_non_primary_process(self.primary_process_id);
        if !self.config.features.enabled(Feature::Collab) && !has_non_primary_agent_process {
            self.chat_widget.open_multi_agent_enable_prompt();
            return;
        }

        if self.agent_navigation.is_empty() {
            self.chat_widget
                .add_info_message("No agents available yet.".to_string(), /*hint*/ None);
            return;
        }

        let mut initial_selected_idx = None;
        let items: Vec<SelectionItem> = self
            .agent_navigation
            .ordered_processes()
            .iter()
            .enumerate()
            .map(|(idx, (process_id, entry))| {
                if self.active_process_id == Some(*process_id) {
                    initial_selected_idx = Some(idx);
                }
                let id = *process_id;
                let is_primary = self.primary_process_id == Some(*process_id);
                let name = format_agent_picker_item_name(
                    entry.agent_nickname.as_deref(),
                    entry.agent_role.as_deref(),
                    is_primary,
                );
                let uuid = process_id.to_string();
                SelectionItem {
                    name: name.clone(),
                    name_prefix_spans: agent_picker_status_dot_spans(entry.is_closed),
                    description: Some(uuid.clone()),
                    is_current: self.active_process_id == Some(*process_id),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::SelectAgentProcess(id));
                    })],
                    dismiss_on_select: true,
                    search_value: Some(format!("{name} {uuid}")),
                    ..Default::default()
                }
            })
            .collect();

        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Subagents".to_string()),
            subtitle: Some(AgentNavigationState::picker_subtitle()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx,
            ..Default::default()
        });
    }

    /// Updates cached picker metadata and then mirrors any visible-label change into the footer.
    ///
    /// These two writes stay paired so the picker rows and contextual footer continue to describe
    /// the same displayed thread after nickname or role updates.
    fn upsert_agent_picker_thread(
        &mut self,
        process_id: ProcessId,
        agent_nickname: Option<String>,
        agent_role: Option<String>,
        is_closed: bool,
    ) {
        self.agent_navigation
            .upsert(process_id, agent_nickname, agent_role, is_closed);
        self.sync_active_agent_label();
    }

    /// Marks a cached picker thread closed and recomputes the contextual footer label.
    ///
    /// Closing a thread is not the same as removing it: users can still inspect finished agent
    /// transcripts, and the stable next/previous traversal order should not collapse around them.
    fn mark_agent_picker_process_closed(&mut self, process_id: ProcessId) {
        self.agent_navigation.mark_closed(process_id);
        self.sync_active_agent_label();
    }

    async fn select_agent_process(
        &mut self,
        tui: &mut tui::Tui,
        process_id: ProcessId,
    ) -> Result<()> {
        if self.active_process_id == Some(process_id) {
            return Ok(());
        }

        let live_thread = match self.server.get_process(process_id).await {
            Ok(thread) => Some(thread),
            Err(err) => {
                if self.process_event_channels.contains_key(&process_id) {
                    self.mark_agent_picker_process_closed(process_id);
                    None
                } else {
                    self.chat_widget.add_error_message(format!(
                        "Failed to attach to agent process {process_id}: {err}"
                    ));
                    return Ok(());
                }
            }
        };
        let is_replay_only = live_thread.is_none();

        let previous_process_id = self.active_process_id;
        self.store_active_process_receiver().await;
        self.active_process_id = None;
        let Some((receiver, snapshot)) = self.activate_process_for_replay(process_id).await else {
            self.chat_widget
                .add_error_message(format!("Agent process {process_id} is already active."));
            if let Some(previous_process_id) = previous_process_id {
                self.activate_process_channel(previous_process_id).await;
            }
            return Ok(());
        };

        self.active_process_id = Some(process_id);
        self.active_process_rx = Some(receiver);

        let init = self.chatwidget_init_for_forked_or_resumed_process(tui, self.config.clone());
        let codex_op_tx = if let Some(thread) = live_thread {
            crate::chatwidget::spawn_op_forwarder(thread)
        } else {
            let (tx, _rx) = unbounded_channel();
            tx
        };
        self.chat_widget = ChatWidget::new_with_op_sender(init, codex_op_tx);
        self.sync_active_agent_label();

        self.reset_for_process_switch(tui)?;
        self.replay_process_snapshot(snapshot, !is_replay_only);
        if is_replay_only {
            self.chat_widget.add_info_message(
                format!("Agent process {process_id} is closed. Replaying saved transcript."),
                /*hint*/ None,
            );
        }
        self.drain_active_process_events(tui).await?;
        self.refresh_pending_process_approvals().await;

        Ok(())
    }

    fn reset_for_process_switch(&mut self, tui: &mut tui::Tui) -> Result<()> {
        self.overlay = None;
        self.transcript_cells.clear();
        self.deferred_history_lines.clear();
        self.has_emitted_history_lines = false;
        self.backtrack = BacktrackState::default();
        self.backtrack_render_pending = false;
        tui.terminal.clear_scrollback()?;
        tui.terminal.clear()?;
        Ok(())
    }

    fn reset_process_event_state(&mut self) {
        self.abort_all_process_event_listeners();
        self.process_event_channels.clear();
        self.agent_navigation.clear();
        self.active_process_id = None;
        self.active_process_rx = None;
        self.primary_process_id = None;
        self.pending_primary_events.clear();
        self.chat_widget.set_pending_process_approvals(Vec::new());
        self.sync_active_agent_label();
    }

    async fn start_fresh_session_with_summary_hint(&mut self, tui: &mut tui::Tui) {
        // Start a fresh in-memory session while preserving resumability via persisted rollout
        // history.
        self.refresh_in_memory_config_from_disk_best_effort("starting a new process")
            .await;
        let model = self.chat_widget.current_model().to_string();
        let config = self.fresh_session_config();
        let summary = session_summary(
            self.chat_widget.token_usage(),
            self.chat_widget.process_id(),
            self.chat_widget.process_name(),
        );
        self.shutdown_current_process().await;
        let report = self
            .server
            .shutdown_all_processes_bounded(Duration::from_secs(10))
            .await;
        if !report.submit_failed.is_empty() || !report.timed_out.is_empty() {
            tracing::warn!(
                submit_failed = report.submit_failed.len(),
                timed_out = report.timed_out.len(),
                "failed to close all processes"
            );
        }
        let init = crate::chatwidget::ChatWidgetInit {
            config,
            frame_requester: tui.frame_requester(),
            app_event_tx: self.app_event_tx.clone(),
            // New sessions start without prefilled message content.
            initial_user_message: None,
            enhanced_keys_supported: self.enhanced_keys_supported,
            auth_manager: self.auth_manager.clone(),
            models_manager: self.server.get_models_manager(),
            is_first_run: false,
            model: Some(model),
            status_line_invalid_items_warned: self.status_line_invalid_items_warned.clone(),
            session_telemetry: self.session_telemetry.clone(),
        };
        self.chat_widget = ChatWidget::new(init, self.server.clone());
        self.reset_process_event_state();
        if let Some(summary) = summary {
            let mut lines: Vec<Line<'static>> = vec![summary.usage_line.clone().into()];
            if let Some(command) = summary.resume_command {
                let spans = vec!["To continue this session, run ".into(), command.cyan()];
                lines.push(spans.into());
            }
            self.chat_widget.add_plain_history_lines(lines);
        }
        tui.frame_requester().schedule_frame();
    }

    fn fresh_session_config(&self) -> Config {
        self.config.clone()
    }

    async fn drain_active_process_events(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let Some(mut rx) = self.active_process_rx.take() else {
            return Ok(());
        };

        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(event) => self.handle_codex_event_now(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if !disconnected {
            self.active_process_rx = Some(rx);
        } else {
            self.clear_active_thread().await;
        }

        if self.backtrack_render_pending {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }

    /// Returns `(closed_process_id, primary_process_id)` when a non-primary active
    /// thread has died and we should fail over to the primary thread.
    ///
    /// A user-requested shutdown (`ExitMode::ShutdownFirst`) sets
    /// `pending_shutdown_exit_process_id`; matching shutdown completions are ignored
    /// here so Ctrl+C-like exits don't accidentally resurrect the main thread.
    ///
    /// Failover is only eligible when all of these are true:
    /// 1. the event is `ShutdownComplete`;
    /// 2. the active thread differs from the primary thread;
    /// 3. the active thread is not the pending shutdown-exit thread.
    fn active_non_primary_shutdown_target(&self, msg: &EventMsg) -> Option<(ProcessId, ProcessId)> {
        if !matches!(msg, EventMsg::ShutdownComplete) {
            return None;
        }
        let active_process_id = self.active_process_id?;
        let primary_process_id = self.primary_process_id?;
        if self.pending_shutdown_exit_process_id == Some(active_process_id) {
            return None;
        }
        (active_process_id != primary_process_id).then_some((active_process_id, primary_process_id))
    }

    fn replay_process_snapshot(
        &mut self,
        snapshot: ProcessEventSnapshot,
        resume_restored_queue: bool,
    ) {
        if let Some(event) = snapshot.session_configured {
            self.handle_codex_event_replay(event);
        }
        self.chat_widget
            .set_queue_autosend_suppressed(/*suppressed*/ true);
        self.chat_widget
            .restore_process_input_state(snapshot.input_state);
        for event in snapshot.events {
            self.handle_codex_event_replay(event);
        }
        self.chat_widget
            .set_queue_autosend_suppressed(/*suppressed*/ false);
        if resume_restored_queue {
            self.chat_widget.maybe_send_next_queued_input();
        }
        self.refresh_status_line();
    }

    fn should_wait_for_initial_session(session_selection: &SessionSelection) -> bool {
        matches!(
            session_selection,
            SessionSelection::StartFresh | SessionSelection::Exit
        )
    }

    fn should_handle_active_process_events(
        waiting_for_initial_session_configured: bool,
        has_active_process_receiver: bool,
    ) -> bool {
        has_active_process_receiver && !waiting_for_initial_session_configured
    }

    fn should_stop_waiting_for_initial_session(
        waiting_for_initial_session_configured: bool,
        primary_process_id: Option<ProcessId>,
    ) -> bool {
        waiting_for_initial_session_configured && primary_process_id.is_some()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        tui: &mut tui::Tui,
        auth_manager: Arc<AuthManager>,
        process_table: Arc<ProcessTable>,
        log_state_db: Option<Arc<StateRuntime>>,
        config: Config,
        cli_kv_overrides: Vec<(String, TomlValue)>,
        harness_overrides: ConfigOverrides,
        active_profile: Option<String>,
        initial_prompt: Option<String>,
        initial_images: Vec<PathBuf>,
        session_selection: SessionSelection,
        is_first_run: bool,
    ) -> Result<AppExitInfo> {
        use tokio_stream::StreamExt;
        let (app_event_tx, mut app_event_rx) = unbounded_channel();
        let app_event_tx = AppEventSender::new(app_event_tx);
        emit_project_config_warnings(&app_event_tx, &config);
        tui.set_notification_method(config.tui_notification_method);

        let harness_overrides =
            normalize_harness_overrides_for_cwd(harness_overrides, &config.cwd)?;
        let auth_manager = auth_manager.clone();
        let process_table = process_table.clone();
        let model = process_table
            .get_models_manager()
            .get_default_model(&config.model, RefreshStrategy::Offline)
            .await;
        let auth = auth_manager.auth().await;
        let auth_ref = auth.as_ref();
        let auth_mode = auth_ref
            .map(ChaosAuth::auth_mode)
            .map(TelemetryAuthMode::from);
        let session_telemetry = SessionTelemetry::new(
            ProcessId::new(),
            model.as_str(),
            model.as_str(),
            auth_ref.and_then(ChaosAuth::get_account_id),
            auth_ref.and_then(ChaosAuth::get_account_email),
            auth_mode,
            chaos_kern::default_client::originator().value,
            config.otel.log_user_prompt,
            chaos_kern::terminal::user_agent(),
            SessionSource::Cli,
        );
        if config
            .tui_status_line
            .as_ref()
            .is_some_and(|cmd| !cmd.is_empty())
        {
            session_telemetry.counter("codex.status_line", /*inc*/ 1, &[]);
        }

        let status_line_invalid_items_warned = Arc::new(AtomicBool::new(false));

        let enhanced_keys_supported = tui.enhanced_keys_supported();
        let wait_for_initial_session_configured =
            Self::should_wait_for_initial_session(&session_selection);
        let chat_widget = match session_selection {
            SessionSelection::StartFresh | SessionSelection::Exit => {
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: process_table.get_models_manager(),
                    is_first_run,
                    model: Some(model.clone()),
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                ChatWidget::new(init, process_table.clone())
            }
            SessionSelection::Resume(target_session) => {
                let resumed = process_table
                    .resume_process(
                        config.clone(),
                        target_session.process_id,
                        auth_manager.clone(),
                        /*parent_trace*/ None,
                    )
                    .await
                    .wrap_err_with(|| {
                        format!("Failed to resume session {}", target_session.process_id)
                    })?;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: process_table.get_models_manager(),
                    is_first_run,
                    model: config.model.clone(),
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                let (_, process, session_configured) = resumed.into_parts();
                ChatWidget::new_from_existing(init, process, session_configured)
            }
            SessionSelection::Fork(target_session) => {
                session_telemetry.counter(
                    "codex.thread.fork",
                    /*inc*/ 1,
                    &[("source", "cli_subcommand")],
                );
                let forked = process_table
                    .fork_process_by_id(
                        usize::MAX,
                        config.clone(),
                        target_session.process_id,
                        /*persist_extended_history*/ false,
                        /*parent_trace*/ None,
                    )
                    .await
                    .wrap_err_with(|| {
                        format!("Failed to fork session {}", target_session.process_id)
                    })?;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: process_table.get_models_manager(),
                    is_first_run,
                    model: config.model.clone(),
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                let (_, process, session_configured) = forked.into_parts();
                ChatWidget::new_from_existing(init, process, session_configured)
            }
        };

        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        let tool_list_pane = Rc::new(RefCell::new(ToolListPane::new()));
        let tool_list_close = Rc::new(Cell::new(false));
        let mut app = Self {
            server: process_table.clone(),
            session_telemetry: session_telemetry.clone(),
            app_event_tx,
            chat_widget,
            auth_manager: auth_manager.clone(),
            config,
            active_profile,
            cli_kv_overrides,
            harness_overrides,
            runtime_approval_policy_override: None,
            runtime_sandbox_policy_override: None,
            tile_manager: TileManager::new(tool_list_pane.clone(), tool_list_close.clone()),
            tool_list_pane,
            tool_list_close,
            file_search,
            log_state_db,
            log_state_db_init_error: None,
            log_panel: LogPanelState::default(),
            enhanced_keys_supported,
            transcript_cells: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            suppress_shutdown_complete: false,
            pending_shutdown_exit_process_id: None,

            process_event_channels: HashMap::new(),
            process_event_listener_tasks: HashMap::new(),
            agent_navigation: AgentNavigationState::default(),
            active_process_id: None,
            active_process_rx: None,
            primary_process_id: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
        };

        let tui_events = tui.event_stream();
        tokio::pin!(tui_events);

        tui.frame_requester().schedule_frame();

        let mut process_created_rx = process_table.subscribe_process_created();
        let mut listen_for_threads = true;
        let mut waiting_for_initial_session_configured = wait_for_initial_session_configured;

        let exit_reason_result = {
            loop {
                let control = select! {
                    Some(event) = app_event_rx.recv() => {
                        match app.handle_event(tui, event).await {
                            Ok(control) => control,
                            Err(err) => break Err(err),
                        }
                    }
                    active = async {
                        if let Some(rx) = app.active_process_rx.as_mut() {
                            rx.recv().await
                        } else {
                            None
                        }
                    }, if App::should_handle_active_process_events(
                        waiting_for_initial_session_configured,
                        app.active_process_rx.is_some()
                    ) => {
                        if let Some(event) = active {
                            if let Err(err) = app.handle_active_process_event(tui, event).await {
                                break Err(err);
                            }
                        } else {
                            app.clear_active_thread().await;
                        }
                        AppRunControl::Continue
                    }
                    Some(event) = tui_events.next() => {
                        match app.handle_tui_event(tui, event).await {
                            Ok(control) => control,
                            Err(err) => break Err(err),
                        }
                    }
                    // Listen on new thread creation due to collab tools.
                    created = process_created_rx.recv(), if listen_for_threads => {
                        match created {
                            Ok(process_id) => {
                                if let Err(err) = app.handle_process_created(process_id).await {
                                    break Err(err);
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                tracing::warn!("process_created receiver lagged; skipping resync");
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                listen_for_threads = false;
                            }
                        }
                        AppRunControl::Continue
                    }
                };
                if App::should_stop_waiting_for_initial_session(
                    waiting_for_initial_session_configured,
                    app.primary_process_id,
                ) {
                    waiting_for_initial_session_configured = false;
                }
                match control {
                    AppRunControl::Continue => {}
                    AppRunControl::Exit(reason) => break Ok(reason),
                }
            }
        };
        let clear_result = tui.terminal.clear();
        let exit_reason = match exit_reason_result {
            Ok(exit_reason) => {
                clear_result?;
                exit_reason
            }
            Err(err) => {
                if let Err(clear_err) = clear_result {
                    tracing::warn!(error = %clear_err, "failed to clear terminal UI");
                }
                return Err(err);
            }
        };
        Ok(AppExitInfo {
            token_usage: app.token_usage(),
            process_id: app.chat_widget.process_id(),
            process_name: app.chat_widget.process_name(),
            exit_reason,
        })
    }

    pub(crate) async fn handle_tui_event(
        &mut self,
        tui: &mut tui::Tui,
        event: TuiEvent,
    ) -> Result<AppRunControl> {
        if matches!(event, TuiEvent::Draw) {
            let size = tui.terminal.size()?;
            if size != tui.terminal.last_known_screen_size {
                self.refresh_status_line();
            }
        }

        if self.overlay.is_some() {
            let _ = self.handle_backtrack_overlay_event(tui, event).await?;
        } else {
            match event {
                TuiEvent::Key(key_event) => {
                    self.handle_key_event(tui, key_event).await;
                }
                TuiEvent::Mouse(mouse_event) => {
                    match mouse_event.kind {
                        crossterm::event::MouseEventKind::ScrollUp
                            if self.log_panel.is_visible()
                                && self.chat_widget.no_modal_or_popup_active() =>
                        {
                            self.log_panel.scroll_up(3);
                            tui.frame_requester().schedule_frame();
                        }
                        crossterm::event::MouseEventKind::ScrollDown
                            if self.log_panel.is_visible()
                                && self.chat_widget.no_modal_or_popup_active() =>
                        {
                            self.log_panel.scroll_down(3);
                            tui.frame_requester().schedule_frame();
                        }
                        _ => {}
                    }
                }
                TuiEvent::Paste(pasted) => {
                    // Only paste into chat when chat is focused — do not leak
                    // clipboard content into the composer from auxiliary panes.
                    let chat_focused = self
                        .tile_manager
                        .focused()
                        .is_none_or(|id| id == PaneId::ROOT);
                    if chat_focused {
                        // Many terminals convert newlines to \r when pasting (e.g., iTerm2),
                        // but tui-textarea expects \n. Normalize CR to LF.
                        let pasted = pasted.replace("\r", "\n");
                        self.chat_widget.handle_paste(pasted);
                    }
                }
                TuiEvent::Draw => {
                    if self.backtrack_render_pending {
                        self.backtrack_render_pending = false;
                        self.render_transcript_once(tui);
                    }
                    self.chat_widget.maybe_post_pending_notification(tui);
                    if self
                        .chat_widget
                        .handle_paste_burst_tick(tui.frame_requester())
                    {
                        return Ok(AppRunControl::Continue);
                    }
                    // Allow widgets to process any pending timers before rendering.
                    self.chat_widget.pre_draw_tick();
                    let terminal_size = tui.terminal.size()?;
                    self.refresh_log_panel_if_needed(tui, terminal_size.into())
                        .await;
                    let (chat_area, log_area) =
                        split_main_and_panel(terminal_size.into(), self.log_panel.is_visible());
                    if let Some(log_area) = log_area {
                        self.log_panel
                            .set_viewport_height(log_area.height.saturating_sub(2));
                    }
                    // When tiled, the viewport must be tall enough for the
                    // auxiliary panes, not just the chat content.
                    let desired = self.chat_widget.desired_height(chat_area.width);
                    let draw_height = if self.tile_manager.is_single_pane() {
                        desired
                    } else {
                        desired.max(terminal_size.height)
                    };
                    tui.draw(draw_height, |frame| {
                        // The top bar is rendered outside the viewport by Tui::draw()
                        // as a sticky row at screen position 0.
                        let (main_area, log_area) =
                            split_main_and_panel(frame.area(), self.log_panel.is_visible());

                        if self.tile_manager.is_single_pane() {
                            // Fast path: no tiling overhead, identical to pre-hypertile.
                            self.chat_widget.render(main_area, frame.buffer);
                            if let Some((x, y)) = self.chat_widget.cursor_pos(main_area) {
                                frame.set_cursor_position((x, y));
                            }
                        } else {
                            self.tile_manager.render(main_area, frame.buffer);

                            if let Some(chat_rect) = self.tile_manager.pane_rect(PaneId::ROOT) {
                                self.chat_widget.render(chat_rect, frame.buffer);
                            }

                            // Place cursor in chat pane.
                            if self.tile_manager.focused() == Some(PaneId::ROOT)
                                && let Some(chat_rect) = self.tile_manager.pane_rect(PaneId::ROOT)
                                && let Some((x, y)) = self.chat_widget.cursor_pos(chat_rect)
                            {
                                frame.set_cursor_position((x, y));
                            }
                        }

                        if let Some(log_area) = log_area {
                            self.log_panel.render(log_area, frame.buffer);
                        }
                    })?;
                    if self.log_panel.is_visible() {
                        tui.frame_requester()
                            .schedule_frame_in(LOG_PANEL_POLL_INTERVAL);
                    }
                    if self.chat_widget.external_editor_state() == ExternalEditorState::Requested {
                        self.chat_widget
                            .set_external_editor_state(ExternalEditorState::Active);
                        self.app_event_tx.send(AppEvent::LaunchExternalEditor);
                    }
                }
            }
        }
        Ok(AppRunControl::Continue)
    }

    async fn handle_event(&mut self, tui: &mut tui::Tui, event: AppEvent) -> Result<AppRunControl> {
        match event {
            AppEvent::NewSession => {
                self.start_fresh_session_with_summary_hint(tui).await;
            }
            AppEvent::ClearUi => {
                self.clear_terminal_ui(tui, /*redraw_header*/ false)?;
                self.reset_app_ui_state_after_clear();

                self.start_fresh_session_with_summary_hint(tui).await;
            }
            AppEvent::OpenResumePicker => {
                match crate::resume_picker::run_resume_picker(
                    tui,
                    &self.config,
                    /*show_all*/ false,
                )
                .await?
                {
                    SessionSelection::Resume(target_session) => {
                        let current_cwd = self.config.cwd.clone();
                        let resume_cwd = match crate::resolve_cwd_for_resume_or_fork(
                            tui,
                            &self.config,
                            &current_cwd,
                            target_session.process_id,
                            CwdPromptAction::Resume,
                            /*allow_prompt*/ true,
                        )
                        .await?
                        {
                            crate::ResolveCwdOutcome::Continue(Some(cwd)) => cwd,
                            crate::ResolveCwdOutcome::Continue(None) => current_cwd.clone(),
                            crate::ResolveCwdOutcome::Exit => {
                                return Ok(AppRunControl::Exit(ExitReason::UserRequested));
                            }
                        };
                        let mut resume_config = match self
                            .rebuild_config_for_resume_or_fallback(&current_cwd, resume_cwd)
                            .await
                        {
                            Ok(cfg) => cfg,
                            Err(err) => {
                                self.chat_widget.add_error_message(format!(
                                    "Failed to rebuild configuration for resume: {err}"
                                ));
                                return Ok(AppRunControl::Continue);
                            }
                        };
                        self.apply_runtime_policy_overrides(&mut resume_config);
                        let summary = session_summary(
                            self.chat_widget.token_usage(),
                            self.chat_widget.process_id(),
                            self.chat_widget.process_name(),
                        );
                        match self
                            .server
                            .resume_process(
                                resume_config.clone(),
                                target_session.process_id,
                                self.auth_manager.clone(),
                                /*parent_trace*/ None,
                            )
                            .await
                        {
                            Ok(resumed) => {
                                self.shutdown_current_process().await;
                                self.config = resume_config;
                                tui.set_notification_method(self.config.tui_notification_method);
                                self.file_search.update_search_dir(self.config.cwd.clone());
                                let init = self.chatwidget_init_for_forked_or_resumed_process(
                                    tui,
                                    self.config.clone(),
                                );
                                let (_, process, session_configured) = resumed.into_parts();
                                self.chat_widget = ChatWidget::new_from_existing(
                                    init,
                                    process,
                                    session_configured,
                                );
                                self.reset_process_event_state();
                                if let Some(summary) = summary {
                                    let mut lines: Vec<Line<'static>> =
                                        vec![summary.usage_line.clone().into()];
                                    if let Some(command) = summary.resume_command {
                                        let spans = vec![
                                            "To continue this session, run ".into(),
                                            command.cyan(),
                                        ];
                                        lines.push(spans.into());
                                    }
                                    self.chat_widget.add_plain_history_lines(lines);
                                }
                            }
                            Err(err) => {
                                self.chat_widget.add_error_message(format!(
                                    "Failed to resume session {}: {err}",
                                    target_session.process_id
                                ));
                            }
                        }
                    }
                    SessionSelection::Exit
                    | SessionSelection::StartFresh
                    | SessionSelection::Fork(_) => {}
                }

                // Leaving alt-screen may blank the inline viewport; force a redraw either way.
                tui.frame_requester().schedule_frame();
            }
            AppEvent::ForkCurrentSession => {
                self.session_telemetry.counter(
                    "codex.thread.fork",
                    /*inc*/ 1,
                    &[("source", "slash_command")],
                );
                let summary = session_summary(
                    self.chat_widget.token_usage(),
                    self.chat_widget.process_id(),
                    self.chat_widget.process_name(),
                );
                self.chat_widget
                    .add_plain_history_lines(vec!["/fork".magenta().into()]);
                if let Some(process_id) = self.chat_widget.process_id() {
                    self.refresh_in_memory_config_from_disk_best_effort("forking the process")
                        .await;
                    match self
                        .server
                        .fork_process_by_id(
                            usize::MAX,
                            self.config.clone(),
                            process_id,
                            /*persist_extended_history*/ false,
                            /*parent_trace*/ None,
                        )
                        .await
                    {
                        Ok(forked) => {
                            self.shutdown_current_process().await;
                            let init = self.chatwidget_init_for_forked_or_resumed_process(
                                tui,
                                self.config.clone(),
                            );
                            let (_, process, session_configured) = forked.into_parts();
                            self.chat_widget =
                                ChatWidget::new_from_existing(init, process, session_configured);
                            self.reset_process_event_state();
                            if let Some(summary) = summary {
                                let mut lines: Vec<Line<'static>> =
                                    vec![summary.usage_line.clone().into()];
                                if let Some(command) = summary.resume_command {
                                    let spans = vec![
                                        "To continue this session, run ".into(),
                                        command.cyan(),
                                    ];
                                    lines.push(spans.into());
                                }
                                self.chat_widget.add_plain_history_lines(lines);
                            }
                        }
                        Err(err) => {
                            self.chat_widget.add_error_message(format!(
                                "Failed to fork current session {process_id}: {err}"
                            ));
                        }
                    }
                } else {
                    self.chat_widget.add_error_message(
                        "A process must contain at least one turn before it can be forked."
                            .to_string(),
                    );
                }

                tui.frame_requester().schedule_frame();
            }
            AppEvent::InsertHistoryCell(cell) => {
                let cell: Arc<dyn HistoryCell> = cell.into();
                if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                    t.insert_cell(cell.clone());
                    tui.frame_requester().schedule_frame();
                }
                self.transcript_cells.push(cell.clone());
                let mut display = cell.display_lines(tui.terminal.last_known_screen_size.width);
                if !display.is_empty() {
                    // Only insert a separating blank line for new cells that are not
                    // part of an ongoing stream. Streaming continuations should not
                    // accrue extra blank lines between chunks.
                    if !cell.is_stream_continuation() {
                        if self.has_emitted_history_lines {
                            display.insert(0, Line::from(""));
                        } else {
                            self.has_emitted_history_lines = true;
                        }
                    }
                    if self.overlay.is_some() {
                        self.deferred_history_lines.extend(display);
                    } else {
                        tui.insert_history_lines(display);
                    }
                }
            }
            AppEvent::ApplyProcessRollback { num_turns } => {
                if self.apply_non_pending_process_rollback(num_turns) {
                    tui.frame_requester().schedule_frame();
                }
            }
            AppEvent::StartCommitAnimation => {
                if self
                    .commit_anim_running
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    let tx = self.app_event_tx.clone();
                    let running = self.commit_anim_running.clone();
                    thread::spawn(move || {
                        while running.load(Ordering::Relaxed) {
                            thread::sleep(COMMIT_ANIMATION_TICK);
                            tx.send(AppEvent::CommitTick);
                        }
                    });
                }
            }
            AppEvent::StopCommitAnimation => {
                self.commit_anim_running.store(false, Ordering::Release);
            }
            AppEvent::CommitTick => {
                self.chat_widget.on_commit_tick();
            }
            AppEvent::ChaosEvent(event) => {
                self.enqueue_primary_event(event).await?;
            }
            AppEvent::ProcessEvent { process_id, event } => {
                self.handle_routed_process_event(process_id, event).await?;
            }
            AppEvent::Exit(mode) => {
                return Ok(self.handle_exit_mode(mode));
            }
            AppEvent::FatalExitRequest(message) => {
                return Ok(AppRunControl::Exit(ExitReason::Fatal(message)));
            }
            AppEvent::ChaosOp(op) => {
                let replay_state_op =
                    ProcessEventStore::op_can_change_pending_replay_state(&op).then(|| op.clone());
                let submitted = self.chat_widget.submit_op(op);
                if submitted && let Some(op) = replay_state_op.as_ref() {
                    self.note_active_process_outbound_op(op).await;
                    self.refresh_pending_process_approvals().await;
                }
            }
            AppEvent::SubmitProcessOp { process_id, op } => {
                self.submit_op_to_process(process_id, op).await;
            }
            AppEvent::DiffResult(text) => {
                // Clear the in-progress state in the bottom pane
                self.chat_widget.on_diff_complete();
                // Enter alternate screen using TUI helper and build pager lines
                let _ = tui.enter_alt_screen();
                let pager_lines: Vec<ratatui::text::Line<'static>> = if text.trim().is_empty() {
                    vec!["No changes detected.".italic().into()]
                } else {
                    text.lines().map(ansi_escape_line).collect()
                };
                self.overlay = Some(Overlay::new_static_with_lines(
                    pager_lines,
                    "D I F F".to_string(),
                ));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenAppLink {
                app_id,
                title,
                description,
                instructions,
                url,
                is_installed,
                is_enabled,
            } => {
                self.chat_widget
                    .open_app_link_view(crate::bottom_pane::AppLinkViewParams {
                        app_id,
                        title,
                        description,
                        instructions,
                        url,
                        is_installed,
                        is_enabled,
                        suggest_reason: None,
                        suggestion_type: None,
                        elicitation_target: None,
                    });
            }
            AppEvent::OpenUrlInBrowser { url } => {
                let _ = self.open_url_in_browser(url);
            }
            AppEvent::OpenUrlElicitationInBrowser {
                process_id,
                server_name,
                request_id,
                url,
                on_open,
                on_error,
            } => {
                let decision = if self.open_url_in_browser(url) {
                    on_open
                } else {
                    on_error
                };
                self.submit_op_to_process(
                    process_id,
                    Op::ResolveElicitation {
                        server_name,
                        request_id,
                        decision,
                        content: None,
                        meta: None,
                    },
                )
                .await;
            }
            AppEvent::RefreshConnectors { force_refetch } => {
                self.chat_widget.refresh_connectors(force_refetch);
            }
            AppEvent::StartFileSearch(query) => {
                self.file_search.on_user_query(query);
            }
            AppEvent::FileSearchResult { query, matches } => {
                self.chat_widget.apply_file_search_result(query, matches);
            }
            AppEvent::ConnectorsLoaded { result, is_final } => {
                self.chat_widget.on_connectors_loaded(result, is_final);
            }
            AppEvent::UpdateReasoningEffort(effort) => {
                self.on_update_reasoning_effort(effort);
                self.refresh_status_line();
            }
            AppEvent::UpdateModel(model) => {
                self.chat_widget.set_model(&model);
                self.refresh_status_line();
            }
            AppEvent::UpdateCollaborationMode(mask) => {
                self.chat_widget.set_collaboration_mask(mask);
                self.refresh_status_line();
            }
            AppEvent::UpdatePersonality(personality) => {
                self.on_update_personality(personality);
            }
            AppEvent::OpenReasoningPopup { model } => {
                self.chat_widget.open_reasoning_popup(model);
            }
            AppEvent::OpenPlanReasoningScopePrompt { model, effort } => {
                self.chat_widget
                    .open_plan_reasoning_scope_prompt(model, effort);
            }
            AppEvent::OpenAllModelsPopup { models } => {
                self.chat_widget.open_all_models_popup(models);
            }
            AppEvent::OpenFullAccessConfirmation {
                preset,
                return_to_permissions,
            } => {
                self.chat_widget
                    .open_full_access_confirmation(preset, return_to_permissions);
            }
            AppEvent::LaunchExternalEditor => {
                if self.chat_widget.external_editor_state() == ExternalEditorState::Active {
                    self.launch_external_editor(tui).await;
                }
            }
            AppEvent::PersistModelSelection { model, effort } => {
                if crate::theme::is_clamped() {
                    tracing::debug!(%model, "skipping model persistence while clamped");
                    return Ok(AppRunControl::Continue);
                }

                let profile = self.active_profile.as_deref();
                match ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_profile(profile)
                    .set_model(Some(model.as_str()), effort)
                    .apply()
                    .await
                {
                    Ok(()) => {
                        let effort_label = effort
                            .map(|selected_effort| selected_effort.to_string())
                            .unwrap_or_else(|| "default".to_string());
                        tracing::info!("Selected model: {model}, Selected effort: {effort_label}");
                        let mut message = format!("Model changed to {model}");
                        if let Some(label) = Self::reasoning_label_for(&model, effort) {
                            message.push(' ');
                            message.push_str(label);
                        }
                        if let Some(profile) = profile {
                            message.push_str(" for ");
                            message.push_str(profile);
                            message.push_str(" profile");
                        }
                        self.chat_widget.add_info_message(message, /*hint*/ None);
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "failed to persist model selection"
                        );
                        if let Some(profile) = profile {
                            self.chat_widget.add_error_message(format!(
                                "Failed to save model for profile `{profile}`: {err}"
                            ));
                        } else {
                            self.chat_widget
                                .add_error_message(format!("Failed to save default model: {err}"));
                        }
                    }
                }
            }
            AppEvent::PersistPersonalitySelection { personality } => {
                let profile = self.active_profile.as_deref();
                match ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_profile(profile)
                    .set_personality(Some(personality))
                    .apply()
                    .await
                {
                    Ok(()) => {
                        let label = Self::personality_label(personality);
                        let mut message = format!("Personality set to {label}");
                        if let Some(profile) = profile {
                            message.push_str(" for ");
                            message.push_str(profile);
                            message.push_str(" profile");
                        }
                        self.chat_widget.add_info_message(message, /*hint*/ None);
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "failed to persist personality selection"
                        );
                        if let Some(profile) = profile {
                            self.chat_widget.add_error_message(format!(
                                "Failed to save personality for profile `{profile}`: {err}"
                            ));
                        } else {
                            self.chat_widget.add_error_message(format!(
                                "Failed to save default personality: {err}"
                            ));
                        }
                    }
                }
            }
            AppEvent::UpdateApprovalPolicy(policy) => {
                let mut config = self.config.clone();
                if !self.try_set_approval_policy_on_config(
                    &mut config,
                    policy,
                    "Failed to set approval policy",
                    "failed to set approval policy on app config",
                ) {
                    return Ok(AppRunControl::Continue);
                }
                self.config = config;
                self.runtime_approval_policy_override =
                    Some(self.config.permissions.approval_policy.value());
                self.chat_widget
                    .set_approval_policy(self.config.permissions.approval_policy.value());
            }
            AppEvent::UpdateSandboxPolicy(policy) => {
                let policy_for_chat = policy.clone();

                let mut config = self.config.clone();
                if !self.try_set_sandbox_policy_on_config(
                    &mut config,
                    policy,
                    "Failed to set sandbox policy",
                    "failed to set sandbox policy on app config",
                ) {
                    return Ok(AppRunControl::Continue);
                }
                self.config = config;
                if let Err(err) = self.chat_widget.set_sandbox_policy(policy_for_chat) {
                    tracing::warn!(%err, "failed to set sandbox policy on chat config");
                    self.chat_widget
                        .add_error_message(format!("Failed to set sandbox policy: {err}"));
                    return Ok(AppRunControl::Continue);
                }
                self.runtime_sandbox_policy_override =
                    Some(self.config.permissions.sandbox_policy.get().clone());
            }
            AppEvent::UpdateApprovalsReviewer(policy) => {
                self.config.approvals_reviewer = policy;
                self.chat_widget.set_approvals_reviewer(policy);
                let profile = self.active_profile.as_deref();
                let segments = if let Some(profile) = profile {
                    vec![
                        "profiles".to_string(),
                        profile.to_string(),
                        "approvals_reviewer".to_string(),
                    ]
                } else {
                    vec!["approvals_reviewer".to_string()]
                };
                if let Err(err) = ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_profile(profile)
                    .with_edits([ConfigEdit::SetPath {
                        segments,
                        value: policy.to_string().into(),
                    }])
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist approvals reviewer update"
                    );
                    self.chat_widget
                        .add_error_message(format!("Failed to save approvals reviewer: {err}"));
                }
            }
            AppEvent::UpdateFeatureFlags { updates } => {
                self.update_feature_flags(updates).await;
            }
            AppEvent::UpdateFullAccessWarningAcknowledged(ack) => {
                self.chat_widget.set_full_access_warning_acknowledged(ack);
            }
            AppEvent::UpdateRateLimitSwitchPromptHidden(hidden) => {
                self.chat_widget.set_rate_limit_switch_prompt_hidden(hidden);
            }
            AppEvent::UpdatePlanModeReasoningEffort(effort) => {
                self.config.plan_mode_reasoning_effort = effort;
                self.chat_widget.set_plan_mode_reasoning_effort(effort);
                self.refresh_status_line();
            }
            AppEvent::PersistFullAccessWarningAcknowledged => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.chaos_home)
                    .set_hide_full_access_warning(/*acknowledged*/ true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist full access warning acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save full access confirmation preference: {err}"
                    ));
                }
            }
            AppEvent::PersistRateLimitSwitchPromptHidden => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.chaos_home)
                    .set_hide_rate_limit_model_nudge(/*acknowledged*/ true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist rate limit switch prompt preference"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save rate limit reminder preference: {err}"
                    ));
                }
            }
            AppEvent::PersistPlanModeReasoningEffort(effort) => {
                let profile = self.active_profile.as_deref();
                let segments = if let Some(profile) = profile {
                    vec![
                        "profiles".to_string(),
                        profile.to_string(),
                        "plan_mode_reasoning_effort".to_string(),
                    ]
                } else {
                    vec!["plan_mode_reasoning_effort".to_string()]
                };
                let edit = if let Some(effort) = effort {
                    ConfigEdit::SetPath {
                        segments,
                        value: effort.to_string().into(),
                    }
                } else {
                    ConfigEdit::ClearPath { segments }
                };
                if let Err(err) = ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_edits([edit])
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist plan mode reasoning effort"
                    );
                    if let Some(profile) = profile {
                        self.chat_widget.add_error_message(format!(
                            "Failed to save Plan mode reasoning effort for profile `{profile}`: {err}"
                        ));
                    } else {
                        self.chat_widget.add_error_message(format!(
                            "Failed to save Plan mode reasoning effort: {err}"
                        ));
                    }
                }
            }
            AppEvent::OpenApprovalsPopup => {
                self.chat_widget.open_approvals_popup();
            }
            AppEvent::OpenLoginPopup => {
                let _ = tui.enter_alt_screen();
                self.overlay = Some(Overlay::new_login(AuthModeWidget::new(
                    tui.frame_requester(),
                    self.config.chaos_home.clone(),
                    self.config.cli_auth_credentials_store_mode,
                    self.auth_manager.clone(),
                    self.config.forced_chatgpt_workspace_id.clone(),
                    self.config.forced_login_method,
                    self.config.animations,
                )));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenAgentPicker => {
                self.open_agent_picker().await;
            }
            AppEvent::SelectAgentProcess(process_id) => {
                self.select_agent_process(tui, process_id).await?;
            }
            AppEvent::SetAppEnabled { id, enabled } => {
                let edits = if enabled {
                    vec![
                        ConfigEdit::ClearPath {
                            segments: vec!["apps".to_string(), id.clone(), "enabled".to_string()],
                        },
                        ConfigEdit::ClearPath {
                            segments: vec![
                                "apps".to_string(),
                                id.clone(),
                                "disabled_reason".to_string(),
                            ],
                        },
                    ]
                } else {
                    vec![
                        ConfigEdit::SetPath {
                            segments: vec!["apps".to_string(), id.clone(), "enabled".to_string()],
                            value: false.into(),
                        },
                        ConfigEdit::SetPath {
                            segments: vec![
                                "apps".to_string(),
                                id.clone(),
                                "disabled_reason".to_string(),
                            ],
                            value: "user".into(),
                        },
                    ]
                };
                match ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_edits(edits)
                    .apply()
                    .await
                {
                    Ok(()) => {
                        self.chat_widget.update_connector_enabled(&id, enabled);
                        if let Err(err) = self.refresh_in_memory_config_from_disk().await {
                            tracing::warn!(error = %err, "failed to refresh config after app toggle");
                        }
                        self.chat_widget.submit_op(Op::ReloadUserConfig);
                    }
                    Err(err) => {
                        self.chat_widget.add_error_message(format!(
                            "Failed to update app config for {id}: {err}"
                        ));
                    }
                }
            }
            AppEvent::OpenPermissionsPopup => {
                self.chat_widget.open_permissions_popup();
            }
            AppEvent::OpenReviewBranchPicker(cwd) => {
                self.chat_widget.show_review_branch_picker(&cwd).await;
            }
            AppEvent::OpenReviewCommitPicker(cwd) => {
                self.chat_widget.show_review_commit_picker(&cwd).await;
            }
            AppEvent::OpenReviewCustomPrompt => {
                self.chat_widget.show_review_custom_prompt();
            }
            AppEvent::SubmitUserMessageWithMode {
                text,
                collaboration_mode,
            } => {
                self.chat_widget
                    .submit_user_message_with_mode(text, collaboration_mode);
            }
            AppEvent::FullScreenApprovalRequest(request) => match request {
                ApprovalRequest::ApplyPatch { cwd, changes, .. } => {
                    let _ = tui.enter_alt_screen();
                    let diff_summary = DiffSummary::new(changes, cwd);
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![diff_summary.into()],
                        "P A T C H".to_string(),
                    ));
                }
                ApprovalRequest::Exec { command, .. } => {
                    let _ = tui.enter_alt_screen();
                    let full_cmd = strip_bash_lc_and_escape(&command);
                    let full_cmd_lines = highlight_bash_to_lines(&full_cmd);
                    self.overlay = Some(Overlay::new_static_with_lines(
                        full_cmd_lines,
                        "E X E C".to_string(),
                    ));
                }
                ApprovalRequest::Permissions {
                    permissions,
                    reason,
                    ..
                } => {
                    let _ = tui.enter_alt_screen();
                    let mut lines = Vec::new();
                    if let Some(reason) = reason {
                        lines.push(Line::from(vec!["Reason: ".into(), reason.italic()]));
                        lines.push(Line::from(""));
                    }
                    if let Some(rule_line) =
                        crate::bottom_pane::format_requested_permissions_rule(&permissions)
                    {
                        lines.push(Line::from(vec![
                            "Permission rule: ".into(),
                            rule_line.cyan(),
                        ]));
                    }
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![Box::new(Paragraph::new(lines).wrap(Wrap { trim: false }))],
                        "P E R M I S S I O N S".to_string(),
                    ));
                }
                ApprovalRequest::McpElicitation {
                    server_name,
                    message,
                    url,
                    ..
                } => {
                    let _ = tui.enter_alt_screen();
                    let mut lines = vec![
                        Line::from(vec!["Server: ".into(), server_name.bold()]),
                        Line::from(""),
                    ];
                    if let Some(url) = url {
                        lines.push(Line::from(vec!["URL: ".into(), url.cyan().underlined()]));
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(message));
                    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![Box::new(paragraph)],
                        "E L I C I T A T I O N".to_string(),
                    ));
                }
            },
            AppEvent::StatusLineSetup { items } => {
                let ids = items.iter().map(ToString::to_string).collect::<Vec<_>>();
                let edit = chaos_kern::config::edit::status_line_items_edit(&ids);
                let apply_result = ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_edits([edit])
                    .apply()
                    .await;
                match apply_result {
                    Ok(()) => {
                        self.config.tui_status_line = Some(ids.clone());
                        self.chat_widget.setup_status_line(items);
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "failed to persist status line items; keeping previous selection");
                        self.chat_widget
                            .add_error_message(format!("Failed to save status line items: {err}"));
                    }
                }
            }
            AppEvent::StatusLineBranchUpdated { cwd, branch } => {
                self.chat_widget.set_status_line_branch(cwd, branch);
                self.refresh_status_line();
            }
            AppEvent::StatusLineSetupCancelled => {
                self.chat_widget.cancel_status_line_setup();
            }
            AppEvent::SyntaxThemeSelected { name } => {
                let edit = chaos_kern::config::edit::syntax_theme_edit(&name);
                let apply_result = ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_edits([edit])
                    .apply()
                    .await;
                match apply_result {
                    Ok(()) => {
                        // Ensure the selected theme is active in the current
                        // session.  The preview callback covers arrow-key
                        // navigation, but if the user presses Enter without
                        // navigating, the runtime theme must still be applied.
                        if let Some(theme) = crate::render::highlight::resolve_theme_by_name(
                            &name,
                            Some(&self.config.chaos_home),
                        ) {
                            crate::render::highlight::set_syntax_theme(theme);
                        }
                        self.sync_tui_theme_selection(name);
                    }
                    Err(err) => {
                        self.restore_runtime_theme_from_config();
                        tracing::error!(error = %err, "failed to persist theme selection");
                        self.chat_widget
                            .add_error_message(format!("Failed to save theme: {err}"));
                    }
                }
            }
            AppEvent::AllToolsReceived(ev) => {
                self.on_all_tools_received(tui, ev);
            }
        }
        Ok(AppRunControl::Continue)
    }

    fn handle_exit_mode(&mut self, mode: ExitMode) -> AppRunControl {
        match mode {
            ExitMode::ShutdownFirst => {
                // Mark the thread we are explicitly shutting down for exit so
                // its shutdown completion does not trigger agent failover.
                self.pending_shutdown_exit_process_id =
                    self.active_process_id.or(self.chat_widget.process_id());
                if self.chat_widget.submit_op(Op::Shutdown) {
                    AppRunControl::Continue
                } else {
                    self.pending_shutdown_exit_process_id = None;
                    AppRunControl::Exit(ExitReason::UserRequested)
                }
            }
            ExitMode::Immediate => {
                self.pending_shutdown_exit_process_id = None;
                AppRunControl::Exit(ExitReason::UserRequested)
            }
        }
    }

    fn handle_codex_event_now(&mut self, event: Event) {
        let needs_refresh = matches!(
            event.msg,
            EventMsg::SessionConfigured(_) | EventMsg::TurnStarted(_) | EventMsg::TokenCount(_)
        );
        // This guard is only for intentional thread-switch shutdowns.
        // App-exit shutdowns are tracked by `pending_shutdown_exit_process_id`
        // and resolved in `handle_active_process_event`.
        if self.suppress_shutdown_complete && matches!(event.msg, EventMsg::ShutdownComplete) {
            self.suppress_shutdown_complete = false;
            return;
        }
        self.handle_backtrack_event(&event.msg);
        self.chat_widget.handle_codex_event(event);

        if needs_refresh {
            self.refresh_status_line();
        }
    }

    fn handle_codex_event_replay(&mut self, event: Event) {
        self.chat_widget.handle_codex_event_replay(event);
    }

    /// Handles an event emitted by the currently active thread.
    ///
    /// This function enforces shutdown intent routing: unexpected non-primary
    /// thread shutdowns fail over to the primary thread, while user-requested
    /// app exits consume only the tracked shutdown completion and then proceed.
    async fn handle_active_process_event(
        &mut self,
        tui: &mut tui::Tui,
        event: Event,
    ) -> Result<()> {
        // Capture this before any potential thread switch: we only want to clear
        // the exit marker when the currently active thread acknowledges shutdown.
        let pending_shutdown_exit_completed = matches!(&event.msg, EventMsg::ShutdownComplete)
            && self.pending_shutdown_exit_process_id == self.active_process_id;

        // Processing order matters:
        //
        // 1. handle unexpected non-primary shutdown failover first;
        // 2. clear pending exit marker for matching shutdown;
        // 3. forward the event through normal handling.
        //
        // This preserves the mental model that user-requested exits do not trigger
        // failover, while true sub-agent deaths still do.
        if let Some((closed_process_id, primary_process_id)) =
            self.active_non_primary_shutdown_target(&event.msg)
        {
            self.mark_agent_picker_process_closed(closed_process_id);
            self.select_agent_process(tui, primary_process_id).await?;
            if self.active_process_id == Some(primary_process_id) {
                self.chat_widget.add_info_message(
                    format!(
                        "Agent process {closed_process_id} closed. Switched back to the main process."
                    ),
                    /*hint*/ None,
                );
            } else {
                self.clear_active_thread().await;
                self.chat_widget.add_error_message(format!(
                    "Agent process {closed_process_id} closed. Failed to switch back to the main process {primary_process_id}.",
                ));
            }
            return Ok(());
        }

        if pending_shutdown_exit_completed {
            // Clear only after seeing the shutdown completion for the tracked
            // thread, so unrelated shutdowns cannot consume this marker.
            self.pending_shutdown_exit_process_id = None;
        }
        self.handle_codex_event_now(event);
        if self.backtrack_render_pending {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }

    async fn handle_process_created(&mut self, process_id: ProcessId) -> Result<()> {
        if self.process_event_channels.contains_key(&process_id) {
            return Ok(());
        }
        let thread = match self.server.get_process(process_id).await {
            Ok(thread) => thread,
            Err(err) => {
                tracing::warn!("failed to attach listener for process {process_id}: {err}");
                return Ok(());
            }
        };
        let config_snapshot = thread.config_snapshot().await;
        self.upsert_agent_picker_thread(
            process_id,
            config_snapshot.session_source.get_nickname(),
            config_snapshot.session_source.get_agent_role(),
            /*is_closed*/ false,
        );
        let event = Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: process_id,
                forked_from_id: None,
                process_name: None,
                model: config_snapshot.model,
                model_provider_id: config_snapshot.model_provider_id,
                service_tier: config_snapshot.service_tier,
                approval_policy: config_snapshot.approval_policy,
                approvals_reviewer: config_snapshot.approvals_reviewer,
                sandbox_policy: config_snapshot.sandbox_policy,
                cwd: config_snapshot.cwd,
                reasoning_effort: config_snapshot.reasoning_effort,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        };
        let channel =
            ProcessEventChannel::new_with_session_configured(PROCESS_EVENT_CHANNEL_CAPACITY, event);
        let app_event_tx = self.app_event_tx.clone();
        self.process_event_channels.insert(process_id, channel);
        let listener_handle = tokio::spawn(async move {
            loop {
                let event = match thread.next_event().await {
                    Ok(event) => event,
                    Err(err) => {
                        tracing::debug!("external process {process_id} listener stopped: {err}");
                        break;
                    }
                };
                app_event_tx.send(AppEvent::ProcessEvent { process_id, event });
            }
        });
        self.process_event_listener_tasks
            .insert(process_id, listener_handle);
        Ok(())
    }

    fn reasoning_label(reasoning_effort: Option<ReasoningEffortConfig>) -> &'static str {
        match reasoning_effort {
            Some(ReasoningEffortConfig::Minimal) => "minimal",
            Some(ReasoningEffortConfig::Low) => "low",
            Some(ReasoningEffortConfig::Medium) => "medium",
            Some(ReasoningEffortConfig::High) => "high",
            Some(ReasoningEffortConfig::XHigh) => "xhigh",
            None | Some(ReasoningEffortConfig::None) => "default",
        }
    }

    fn reasoning_label_for(
        model: &str,
        reasoning_effort: Option<ReasoningEffortConfig>,
    ) -> Option<&'static str> {
        (!model.starts_with("codex-auto-")).then(|| Self::reasoning_label(reasoning_effort))
    }

    pub(crate) fn token_usage(&self) -> chaos_ipc::protocol::TokenUsage {
        self.chat_widget.token_usage()
    }

    fn on_update_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        // TODO(aibrahim): Remove this and don't use config as a state object.
        // Instead, explicitly pass the stored collaboration mode's effort into new sessions.
        self.config.model_reasoning_effort = effort;
        self.chat_widget.set_reasoning_effort(effort);
    }

    fn on_update_personality(&mut self, personality: Personality) {
        self.config.personality = Some(personality);
        self.chat_widget.set_personality(personality);
    }

    fn sync_tui_theme_selection(&mut self, name: String) {
        self.config.tui_theme = Some(name.clone());
        self.chat_widget.set_tui_theme(Some(name));
    }

    fn restore_runtime_theme_from_config(&self) {
        if let Some(name) = self.config.tui_theme.as_deref()
            && let Some(theme) =
                crate::render::highlight::resolve_theme_by_name(name, Some(&self.config.chaos_home))
        {
            crate::render::highlight::set_syntax_theme(theme);
            return;
        }

        let auto_theme_name = crate::render::highlight::adaptive_default_theme_name();
        if let Some(theme) = crate::render::highlight::resolve_theme_by_name(
            auto_theme_name,
            Some(&self.config.chaos_home),
        ) {
            crate::render::highlight::set_syntax_theme(theme);
        }
    }

    fn personality_label(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "None",
            Personality::Friendly => "Friendly",
            Personality::Pragmatic => "Pragmatic",
        }
    }

    async fn launch_external_editor(&mut self, tui: &mut tui::Tui) {
        let editor_cmd = match external_editor::resolve_editor_command() {
            Ok(cmd) => cmd,
            Err(external_editor::EditorError::MissingEditor) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(
                    "Cannot open external editor: set $VISUAL or $EDITOR before starting Chaos."
                        .to_string(),
                ));
                self.reset_external_editor_state(tui);
                return;
            }
            Err(err) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(format!(
                        "Failed to open editor: {err}",
                    )));
                self.reset_external_editor_state(tui);
                return;
            }
        };

        let seed = self.chat_widget.composer_text_with_pending();
        let editor_result = tui
            .with_restored(tui::RestoreMode::KeepRaw, || async {
                external_editor::run_editor(&seed, &editor_cmd).await
            })
            .await;
        self.reset_external_editor_state(tui);

        match editor_result {
            Ok(new_text) => {
                // Trim trailing whitespace
                let cleaned = new_text.trim_end().to_string();
                self.chat_widget.apply_external_edit(cleaned);
            }
            Err(err) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(format!(
                        "Failed to open editor: {err}",
                    )));
            }
        }
        tui.frame_requester().schedule_frame();
    }

    fn request_external_editor_launch(&mut self, tui: &mut tui::Tui) {
        self.chat_widget
            .set_external_editor_state(ExternalEditorState::Requested);
        self.chat_widget.set_footer_hint_override(Some(vec![(
            EXTERNAL_EDITOR_HINT.to_string(),
            String::new(),
        )]));
        tui.frame_requester().schedule_frame();
    }

    fn reset_external_editor_state(&mut self, tui: &mut tui::Tui) {
        self.chat_widget
            .set_external_editor_state(ExternalEditorState::Closed);
        self.chat_widget.set_footer_hint_override(/*items*/ None);
        tui.frame_requester().schedule_frame();
    }

    async fn handle_key_event(&mut self, tui: &mut tui::Tui, key_event: KeyEvent) {
        // Some terminals, especially on macOS, encode Option+Left/Right as Option+b/f unless
        // enhanced keyboard reporting is available. We only treat those word-motion fallbacks as
        // agent-switch shortcuts when the composer is empty so we never steal the expected
        // editing behavior for moving across words inside a draft.
        let allow_agent_word_motion_fallback = !self.enhanced_keys_supported
            && self.chat_widget.composer_text_with_pending().is_empty();
        if self.overlay.is_none()
            && self.chat_widget.no_modal_or_popup_active()
            // Alt+Left/Right are also natural word-motion keys in the composer. Keep agent
            // fast-switch available only once the draft is empty so editing behavior wins whenever
            // there is text on screen.
            && self.chat_widget.composer_text_with_pending().is_empty()
            && previous_agent_shortcut_matches(key_event, allow_agent_word_motion_fallback)
        {
            if let Some(process_id) = self.agent_navigation.adjacent_process_id(
                self.current_displayed_process_id(),
                AgentNavigationDirection::Previous,
            ) {
                let _ = self.select_agent_process(tui, process_id).await;
            }
            return;
        }
        if self.overlay.is_none()
            && self.chat_widget.no_modal_or_popup_active()
            // Mirror the previous-agent rule above: empty drafts may use these keys for process
            // switching, but non-empty drafts keep them for expected word-wise cursor motion.
            && self.chat_widget.composer_text_with_pending().is_empty()
            && next_agent_shortcut_matches(key_event, allow_agent_word_motion_fallback)
        {
            if let Some(process_id) = self.agent_navigation.adjacent_process_id(
                self.current_displayed_process_id(),
                AgentNavigationDirection::Next,
            ) {
                let _ = self.select_agent_process(tui, process_id).await;
            }
            return;
        }

        // Tiling shortcuts — only active when multiple panes are open.
        if !self.tile_manager.is_single_pane()
            && self.overlay.is_none()
            && self.chat_widget.no_modal_or_popup_active()
            && key_event.kind == KeyEventKind::Press
        {
            use crossterm::event::KeyModifiers;
            use ratatui::layout::Direction;
            use ratatui_hypertile::HypertileAction;
            use ratatui_hypertile::Towards;

            let alt = key_event.modifiers.contains(KeyModifiers::ALT);
            let alt_shift = key_event
                .modifiers
                .contains(KeyModifiers::ALT | KeyModifiers::SHIFT);

            let tiling_action = match (key_event.code, alt, alt_shift) {
                // Alt+h/j/k/l — focus direction
                (KeyCode::Char('h'), true, false) => Some(HypertileAction::FocusDirection {
                    direction: Direction::Horizontal,
                    towards: Towards::Start,
                }),
                (KeyCode::Char('l'), true, false) => Some(HypertileAction::FocusDirection {
                    direction: Direction::Horizontal,
                    towards: Towards::End,
                }),
                (KeyCode::Char('k'), true, false) => Some(HypertileAction::FocusDirection {
                    direction: Direction::Vertical,
                    towards: Towards::Start,
                }),
                (KeyCode::Char('j'), true, false) => Some(HypertileAction::FocusDirection {
                    direction: Direction::Vertical,
                    towards: Towards::End,
                }),
                // Alt+Shift+H/J/K/L — resize focused pane
                (KeyCode::Char('H'), _, true) => {
                    Some(HypertileAction::ResizeFocused { delta: -0.05 })
                }
                (KeyCode::Char('L'), _, true) => {
                    Some(HypertileAction::ResizeFocused { delta: 0.05 })
                }
                (KeyCode::Char('K'), _, true) => {
                    Some(HypertileAction::ResizeFocused { delta: -0.05 })
                }
                (KeyCode::Char('J'), _, true) => {
                    Some(HypertileAction::ResizeFocused { delta: 0.05 })
                }
                // Alt+q — close auxiliary panes (tries focused first, then last opened)
                (KeyCode::Char('q'), true, false) => {
                    // Try closing focused first; if it's Chat, close the last auxiliary.
                    if self.tile_manager.close_focused().is_none() {
                        self.tile_manager.close_last_auxiliary();
                    }
                    tui.frame_requester().schedule_frame();
                    return;
                }
                // Alt+w — close all auxiliary panes, return to single chat
                (KeyCode::Char('w'), true, false) => {
                    self.tile_manager.close_all_auxiliary();
                    tui.frame_requester().schedule_frame();
                    return;
                }
                // Alt+Enter — cycle focus
                (KeyCode::Enter, true, false) => Some(HypertileAction::FocusNext),
                _ => None,
            };

            if let Some(action) = tiling_action {
                self.tile_manager.apply_action(action);
                tui.frame_requester().schedule_frame();
                return;
            }
        }

        // ── Global shortcuts ─────────────────────────────────────────
        // These work regardless of which pane is focused.
        match key_event {
            KeyEvent {
                code: KeyCode::Char('o'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } if self.overlay.is_none() && self.chat_widget.no_modal_or_popup_active() => {
                self.toggle_log_panel(tui).await;
                return;
            }
            KeyEvent {
                code: KeyCode::PageUp,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.log_panel.is_visible()
                && self.overlay.is_none()
                && self.chat_widget.no_modal_or_popup_active() =>
            {
                self.log_panel.scroll_up(8);
                tui.frame_requester().schedule_frame();
                return;
            }
            KeyEvent {
                code: KeyCode::PageUp,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.overlay.is_none() && self.chat_widget.no_modal_or_popup_active() => {
                self.open_transcript_overlay(tui, Some(TuiEvent::Key(key_event)));
                return;
            }
            KeyEvent {
                code: KeyCode::PageDown,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.log_panel.is_visible()
                && self.overlay.is_none()
                && self.chat_widget.no_modal_or_popup_active() =>
            {
                self.log_panel.scroll_down(8);
                tui.frame_requester().schedule_frame();
                return;
            }
            KeyEvent {
                code: KeyCode::PageDown,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.overlay.is_none() && self.chat_widget.no_modal_or_popup_active() => {
                self.open_transcript_overlay(tui, Some(TuiEvent::Key(key_event)));
                return;
            }
            KeyEvent {
                code: KeyCode::Home,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.log_panel.is_visible()
                && self.overlay.is_none()
                && self.chat_widget.no_modal_or_popup_active() =>
            {
                self.log_panel.scroll_to_start();
                tui.frame_requester().schedule_frame();
                return;
            }
            KeyEvent {
                code: KeyCode::Home,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.overlay.is_none() && self.chat_widget.no_modal_or_popup_active() => {
                self.open_transcript_overlay(tui, Some(TuiEvent::Key(key_event)));
                return;
            }
            KeyEvent {
                code: KeyCode::End,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.log_panel.is_visible()
                && self.overlay.is_none()
                && self.chat_widget.no_modal_or_popup_active() =>
            {
                self.log_panel.scroll_to_end();
                tui.frame_requester().schedule_frame();
                return;
            }
            KeyEvent {
                code: KeyCode::End,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.overlay.is_none() && self.chat_widget.no_modal_or_popup_active() => {
                self.open_transcript_overlay(tui, Some(TuiEvent::Key(key_event)));
                return;
            }
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                self.open_transcript_overlay(tui, None);
                return;
            }
            KeyEvent {
                code: KeyCode::Char('l'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                if !self.chat_widget.can_run_ctrl_l_clear_now() {
                    return;
                }
                if let Err(err) = self.clear_terminal_ui(tui, /*redraw_header*/ false) {
                    tracing::warn!(error = %err, "failed to clear terminal UI");
                    self.chat_widget
                        .add_error_message(format!("Failed to clear terminal UI: {err}"));
                } else {
                    self.reset_app_ui_state_after_clear();
                    self.queue_clear_ui_header(tui);
                    tui.frame_requester().schedule_frame();
                }
                return;
            }
            KeyEvent {
                code: KeyCode::Char('g'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                if self.overlay.is_none()
                    && self.chat_widget.can_launch_external_editor()
                    && self.chat_widget.external_editor_state() == ExternalEditorState::Closed
                {
                    self.request_external_editor_launch(tui);
                }
                return;
            }
            _ => {}
        }

        // ── Focused-pane local input ────────────────────────────────
        // When an auxiliary pane is focused, give it the key first.
        // If the pane ignores the key we swallow it — unmodified keys
        // must never leak into the chat composer.
        if let Some(focused) = self.tile_manager.focused()
            && focused != PaneId::ROOT
        {
            if matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                && let Some(chord) = keychord_from_crossterm(key_event)
                && let Some(plugin) = self.tile_manager.plugin_mut(focused)
            {
                let _ = plugin.on_event(&HypertileEvent::Key(chord));
            }

            let should_close = self.tool_list_close.replace(false)
                || (key_event.kind == KeyEventKind::Press && key_event.code == KeyCode::Esc);

            if should_close {
                self.tile_manager.close_pane(focused);
            }

            tui.frame_requester().schedule_frame();
            return;
        }

        // ── Chat pane input ─────────────────────────────────────────
        // Only reached when Chat (ROOT) is focused.
        match key_event {
            // Esc primes/advances backtracking only in normal (not working) mode
            // with the composer focused and empty. In any other state, forward
            // Esc so the active UI (e.g. status indicator, modals, popups)
            // handles it.
            KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                if self.chat_widget.is_normal_backtrack_mode()
                    && self.chat_widget.composer_is_empty()
                {
                    self.handle_backtrack_esc_key(tui);
                } else {
                    self.chat_widget.handle_key_event(key_event);
                }
            }
            // Enter confirms backtrack when primed + count > 0. Otherwise pass to widget.
            KeyEvent {
                code: KeyCode::Enter,
                kind: KeyEventKind::Press,
                ..
            } if self.backtrack.primed
                && self.backtrack.nth_user_message != usize::MAX
                && self.chat_widget.composer_is_empty() =>
            {
                if let Some(selection) = self.confirm_backtrack_from_main() {
                    self.apply_backtrack_selection(tui, selection);
                }
            }
            KeyEvent {
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                // Any non-Esc key press should cancel a primed backtrack.
                if key_event.code != KeyCode::Esc && self.backtrack.primed {
                    self.reset_backtrack_state();
                }
                self.chat_widget.handle_key_event(key_event);
            }
            _ => {
                self.chat_widget.handle_key_event(key_event);
            }
        };
    }

    fn refresh_status_line(&mut self) {
        self.chat_widget.refresh_status_line();
    }

    fn on_all_tools_received(
        &mut self,
        tui: &mut tui::Tui,
        ev: chaos_ipc::protocol::AllToolsResponseEvent,
    ) {
        use ratatui::layout::Direction;
        // Toggle: if already open, close it; otherwise open.
        self.tool_list_close.set(false);
        if self.tile_manager.find_pane(PaneKind::ToolList).is_some() {
            self.tile_manager.close_kind(PaneKind::ToolList);
        } else {
            self.tool_list_pane.borrow_mut().set_tools(ev.tools);
            self.tile_manager
                .open_or_focus(PaneKind::ToolList, Direction::Horizontal);
        }
        tui.frame_requester().schedule_frame();
    }

    async fn toggle_log_panel(&mut self, tui: &mut tui::Tui) {
        let visible = self.log_panel.toggle();
        if visible {
            self.reload_log_panel_backfill().await;
        }
        tui.frame_requester().schedule_frame();
    }

    async fn refresh_log_panel_if_needed(
        &mut self,
        tui: &mut tui::Tui,
        area: ratatui::layout::Rect,
    ) {
        if !self.log_panel.is_visible() {
            return;
        }
        let (_, log_area) = split_main_and_panel(area, true);
        let Some(log_area) = log_area else {
            return;
        };
        self.log_panel
            .set_viewport_height(log_area.height.saturating_sub(2));

        let current_process_id = self.current_displayed_process_id();
        if self.log_panel.set_process_id(current_process_id) {
            self.reload_log_panel_backfill().await;
            tui.frame_requester().schedule_frame();
            return;
        }

        if self.log_panel.should_poll(Instant::now()) {
            self.poll_log_panel().await;
        }
    }

    async fn reload_log_panel_backfill(&mut self) {
        let Some(state_db) = self.ensure_log_state_db().await else {
            return;
        };
        let Some(process_id) = self.log_panel.process_id() else {
            self.log_panel
                .replace_batch(chaos_proc::LogTailBatch::default());
            self.log_panel.schedule_next_poll(Instant::now());
            return;
        };
        match state_db
            .tail_backfill(
                &Self::log_query_for_process(process_id),
                LOG_PANEL_BACKFILL_LIMIT,
            )
            .await
        {
            Ok(batch) => {
                self.log_panel.replace_batch(batch);
                self.log_panel.schedule_next_poll(Instant::now());
            }
            Err(err) => {
                self.log_panel
                    .set_error(format!("Failed to load logs: {err}"));
            }
        }
    }

    async fn poll_log_panel(&mut self) {
        let Some(state_db) = self.ensure_log_state_db().await else {
            return;
        };
        let Some(process_id) = self.log_panel.process_id() else {
            self.log_panel.schedule_next_poll(Instant::now());
            return;
        };
        let cursor = self.log_panel.cursor();
        match state_db
            .tail_poll(&Self::log_query_for_process(process_id), &cursor, None)
            .await
        {
            Ok(batch) => {
                self.log_panel.append_batch(batch);
                self.log_panel.schedule_next_poll(Instant::now());
            }
            Err(err) => {
                self.log_panel
                    .set_error(format!("Failed to refresh logs: {err}"));
            }
        }
    }

    fn log_query_for_process(process_id: ProcessId) -> LogQuery {
        LogQuery {
            related_to_process_id: Some(process_id.to_string()),
            include_related_processless: true,
            ..Default::default()
        }
    }

    async fn ensure_log_state_db(&mut self) -> Option<Arc<StateRuntime>> {
        if let Some(state_db) = self.log_state_db.clone() {
            return Some(state_db);
        }

        let sqlite_home = self.config.sqlite_home.clone();
        let model_provider_id = self.config.model_provider_id.clone();
        match StateRuntime::init(sqlite_home.clone(), model_provider_id).await {
            Ok(state_db) => {
                self.log_state_db_init_error = None;
                self.log_state_db = Some(state_db.clone());
                Some(state_db)
            }
            Err(err) => {
                let message = format!(
                    "Failed to initialize logs DB at {}: {err}",
                    sqlite_home.display()
                );
                tracing::warn!(
                    error = %err,
                    sqlite_home = %sqlite_home.display(),
                    "failed to lazily initialize log/state runtime for console"
                );
                self.log_state_db_init_error = Some(message.clone());
                self.log_panel.set_error(message);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_backtrack::BacktrackSelection;
    use crate::app_backtrack::BacktrackState;
    use crate::app_backtrack::user_count;
    use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
    use crate::file_search::FileSearchManager;
    use crate::history_cell::AgentMessageCell;
    use crate::history_cell::HistoryCell;
    use crate::history_cell::UserHistoryCell;
    use crate::history_cell::new_session_info;
    use crate::multi_agents::AgentPickerProcessEntry;
    use assert_matches::assert_matches;
    use chaos_ipc::ProcessId;
    use chaos_ipc::config_types::CollaborationMode;
    use chaos_ipc::config_types::CollaborationModeMask;
    use chaos_ipc::config_types::ModeKind;
    use chaos_ipc::config_types::Settings;
    use chaos_ipc::protocol::AgentMessageContentDeltaEvent;
    use chaos_ipc::protocol::ApprovalPolicy;
    use chaos_ipc::protocol::Event;
    use chaos_ipc::protocol::EventMsg;
    use chaos_ipc::protocol::ProcessRolledBackEvent;
    use chaos_ipc::protocol::SandboxPolicy;
    use chaos_ipc::protocol::SessionConfiguredEvent;
    use chaos_ipc::protocol::SessionSource;
    use chaos_ipc::protocol::TurnAbortReason;
    use chaos_ipc::protocol::TurnAbortedEvent;
    use chaos_ipc::protocol::TurnCompleteEvent;
    use chaos_ipc::protocol::TurnStartedEvent;
    use chaos_ipc::protocol::UserMessageEvent;
    use chaos_ipc::user_input::TextElement;
    use chaos_ipc::user_input::UserInput;
    use chaos_kern::ChaosAuth;
    use chaos_kern::config::ConfigOverrides;
    use chaos_kern::config::types::ApprovalsReviewer;
    use chaos_syslog::SessionTelemetry;
    use crossterm::event::KeyModifiers;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::prelude::Line;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use tempfile::tempdir;
    use tokio::time;

    #[test]
    fn normalize_harness_overrides_resolves_relative_add_dirs() -> Result<()> {
        let temp_dir = tempdir()?;
        let base_cwd = temp_dir.path().join("base");
        std::fs::create_dir_all(&base_cwd)?;

        let overrides = ConfigOverrides {
            additional_writable_roots: vec![PathBuf::from("rel")],
            ..Default::default()
        };
        let normalized = normalize_harness_overrides_for_cwd(overrides, &base_cwd)?;

        assert_eq!(
            normalized.additional_writable_roots,
            vec![base_cwd.join("rel")]
        );
        Ok(())
    }

    #[test]
    fn startup_waiting_gate_is_only_for_fresh_or_exit_session_selection() {
        assert_eq!(
            App::should_wait_for_initial_session(&SessionSelection::StartFresh),
            true
        );
        assert_eq!(
            App::should_wait_for_initial_session(&SessionSelection::Exit),
            true
        );
        assert_eq!(
            App::should_wait_for_initial_session(&SessionSelection::Resume(
                crate::resume_picker::SessionTarget {
                    process_id: ProcessId::new(),
                }
            )),
            false
        );
        assert_eq!(
            App::should_wait_for_initial_session(&SessionSelection::Fork(
                crate::resume_picker::SessionTarget {
                    process_id: ProcessId::new(),
                }
            )),
            false
        );
    }

    #[test]
    fn startup_waiting_gate_holds_active_process_events_until_primary_process_configured() {
        let mut wait_for_initial_session =
            App::should_wait_for_initial_session(&SessionSelection::StartFresh);
        assert_eq!(wait_for_initial_session, true);
        assert_eq!(
            App::should_handle_active_process_events(wait_for_initial_session, true),
            false
        );

        assert_eq!(
            App::should_stop_waiting_for_initial_session(wait_for_initial_session, None),
            false
        );
        if App::should_stop_waiting_for_initial_session(
            wait_for_initial_session,
            Some(ProcessId::new()),
        ) {
            wait_for_initial_session = false;
        }
        assert_eq!(wait_for_initial_session, false);

        assert_eq!(
            App::should_handle_active_process_events(wait_for_initial_session, true),
            true
        );
    }

    #[test]
    fn startup_waiting_gate_not_applied_for_resume_or_fork_session_selection() {
        let wait_for_resume = App::should_wait_for_initial_session(&SessionSelection::Resume(
            crate::resume_picker::SessionTarget {
                process_id: ProcessId::new(),
            },
        ));
        assert_eq!(
            App::should_handle_active_process_events(wait_for_resume, true),
            true
        );
        let wait_for_fork = App::should_wait_for_initial_session(&SessionSelection::Fork(
            crate::resume_picker::SessionTarget {
                process_id: ProcessId::new(),
            },
        ));
        assert_eq!(
            App::should_handle_active_process_events(wait_for_fork, true),
            true
        );
    }

    #[tokio::test]
    async fn enqueue_primary_event_delivers_session_configured_before_buffered_approval()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let process_id = ProcessId::new();
        let approval_event = Event {
            id: "approval-event".to_string(),
            msg: EventMsg::ExecApprovalRequest(chaos_ipc::protocol::ExecApprovalRequestEvent {
                call_id: "call-1".to_string(),
                approval_id: None,
                turn_id: "turn-1".to_string(),
                command: vec!["echo".to_string(), "hello".to_string()],
                cwd: PathBuf::from("/tmp/project"),
                reason: Some("needs approval".to_string()),
                network_approval_context: None,
                proposed_execpolicy_amendment: None,
                proposed_network_policy_amendments: None,
                additional_permissions: None,
                skill_metadata: None,
                available_decisions: None,
                parsed_cmd: Vec::new(),
            }),
        };
        let session_configured_event = Event {
            id: "session-configured".to_string(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: process_id,
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/tmp/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        };

        app.enqueue_primary_event(approval_event.clone()).await?;
        app.enqueue_primary_event(session_configured_event.clone())
            .await?;

        let rx = app
            .active_process_rx
            .as_mut()
            .expect("primary thread receiver should be active");
        let first_event = time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("timed out waiting for session configured event")
            .expect("channel closed unexpectedly");
        let second_event = time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("timed out waiting for buffered approval event")
            .expect("channel closed unexpectedly");

        assert!(matches!(first_event.msg, EventMsg::SessionConfigured(_)));
        assert!(matches!(second_event.msg, EventMsg::ExecApprovalRequest(_)));

        app.handle_codex_event_now(first_event);
        app.handle_codex_event_now(second_event);
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

        while let Ok(app_event) = app_event_rx.try_recv() {
            if let AppEvent::SubmitProcessOp {
                process_id: op_process_id,
                ..
            } = app_event
            {
                assert_eq!(op_process_id, process_id);
                return Ok(());
            }
        }

        panic!("expected approval action to submit a process-scoped op");
    }

    #[tokio::test]
    async fn routed_thread_event_does_not_recreate_channel_after_reset() -> Result<()> {
        let mut app = make_test_app().await;
        let process_id = ProcessId::new();
        app.process_event_channels.insert(
            process_id,
            ProcessEventChannel::new(PROCESS_EVENT_CHANNEL_CAPACITY),
        );

        app.reset_process_event_state();
        app.handle_routed_process_event(
            process_id,
            Event {
                id: "stale-event".to_string(),
                msg: EventMsg::ShutdownComplete,
            },
        )
        .await?;

        assert!(
            !app.process_event_channels.contains_key(&process_id),
            "stale routed events should not recreate cleared thread channels"
        );
        assert_eq!(app.active_process_id, None);
        assert_eq!(app.primary_process_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn reset_process_event_state_aborts_listener_tasks() {
        struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

        impl Drop for NotifyOnDrop {
            fn drop(&mut self) {
                if let Some(tx) = self.0.take() {
                    let _ = tx.send(());
                }
            }
        }

        let mut app = make_test_app().await;
        let process_id = ProcessId::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let _notify_on_drop = NotifyOnDrop(Some(dropped_tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        app.process_event_listener_tasks.insert(process_id, handle);
        started_rx
            .await
            .expect("listener task should report it started");

        app.reset_process_event_state();

        assert_eq!(app.process_event_listener_tasks.is_empty(), true);
        time::timeout(Duration::from_millis(50), dropped_rx)
            .await
            .expect("timed out waiting for listener task abort")
            .expect("listener task drop notification should succeed");
    }

    #[tokio::test]
    async fn enqueue_thread_event_does_not_block_when_channel_full() -> Result<()> {
        let mut app = make_test_app().await;
        let process_id = ProcessId::new();
        app.process_event_channels
            .insert(process_id, ProcessEventChannel::new(1));
        app.set_process_active(process_id, true).await;

        let event = Event {
            id: String::new(),
            msg: EventMsg::ShutdownComplete,
        };

        app.enqueue_process_event(process_id, event.clone()).await?;
        time::timeout(
            Duration::from_millis(50),
            app.enqueue_process_event(process_id, event),
        )
        .await
        .expect("enqueue_process_event blocked on a full channel")?;

        let mut rx = app
            .process_event_channels
            .get_mut(&process_id)
            .expect("missing thread channel")
            .receiver
            .take()
            .expect("missing receiver");

        time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("timed out waiting for first event")
            .expect("channel closed unexpectedly");
        time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("timed out waiting for second event")
            .expect("channel closed unexpectedly");

        Ok(())
    }

    #[tokio::test]
    async fn replay_process_snapshot_restores_draft_and_queued_input() {
        let mut app = make_test_app().await;
        let process_id = ProcessId::new();
        app.process_event_channels.insert(
            process_id,
            ProcessEventChannel::new_with_session_configured(
                PROCESS_EVENT_CHANNEL_CAPACITY,
                Event {
                    id: "session-configured".to_string(),
                    msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                        session_id: process_id,
                        forked_from_id: None,
                        process_name: None,
                        model: "gpt-test".to_string(),
                        model_provider_id: "test-provider".to_string(),
                        service_tier: None,
                        approval_policy: ApprovalPolicy::Headless,
                        approvals_reviewer: ApprovalsReviewer::User,
                        sandbox_policy: SandboxPolicy::new_read_only_policy(),
                        cwd: PathBuf::from("/tmp/project"),
                        reasoning_effort: None,
                        history_log_id: 0,
                        history_entry_count: 0,
                        initial_messages: None,
                        network_proxy: None,
                    }),
                },
            ),
        );
        app.activate_process_channel(process_id).await;

        app.chat_widget
            .apply_external_edit("draft prompt".to_string());
        app.chat_widget.submit_user_message_with_mode(
            "queued follow-up".to_string(),
            CollaborationModeMask {
                name: "Default".to_string(),
                mode: None,
                model: None,
                reasoning_effort: None,
                minion_instructions: None,
            },
        );
        let expected_input_state = app
            .chat_widget
            .capture_process_input_state()
            .expect("expected thread input state");

        app.store_active_process_receiver().await;

        let snapshot = {
            let channel = app
                .process_event_channels
                .get(&process_id)
                .expect("thread channel should exist");
            let store = channel.store.lock().await;
            assert_eq!(store.input_state, Some(expected_input_state));
            store.snapshot()
        };

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;

        app.replay_process_snapshot(snapshot, true);

        assert_eq!(app.chat_widget.composer_text_with_pending(), "draft prompt");
        assert!(app.chat_widget.queued_user_message_texts().is_empty());
        match next_user_turn_op(&mut new_op_rx) {
            Op::UserTurn { items, .. } => assert_eq!(
                items,
                vec![UserInput::Text {
                    text: "queued follow-up".to_string(),
                    text_elements: Vec::new(),
                }]
            ),
            other => panic!("expected queued follow-up submission, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replayed_turn_complete_submits_restored_queued_follow_up() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let process_id = ProcessId::new();
        let session_configured = Event {
            id: "session-configured".to_string(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: process_id,
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/tmp/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        };
        app.chat_widget
            .handle_codex_event(session_configured.clone());
        app.chat_widget.handle_codex_event(Event {
            id: "turn-started".to_string(),
            msg: EventMsg::TurnStarted(TurnStartedEvent {
                turn_id: "turn-1".to_string(),
                model_context_window: None,
                collaboration_mode_kind: Default::default(),
            }),
        });
        app.chat_widget.handle_codex_event(Event {
            id: "agent-delta".to_string(),
            msg: EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
                process_id: String::new(),
                turn_id: String::new(),
                item_id: String::new(),
                delta: "streaming".to_string(),
            }),
        });
        app.chat_widget
            .apply_external_edit("queued follow-up".to_string());
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let input_state = app
            .chat_widget
            .capture_process_input_state()
            .expect("expected queued follow-up state");

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_codex_event(session_configured);
        while new_op_rx.try_recv().is_ok() {}
        app.replay_process_snapshot(
            ProcessEventSnapshot {
                session_configured: None,
                events: vec![Event {
                    id: "turn-complete".to_string(),
                    msg: EventMsg::TurnComplete(TurnCompleteEvent {
                        turn_id: "turn-1".to_string(),
                        last_agent_message: None,
                    }),
                }],
                input_state: Some(input_state),
            },
            true,
        );

        match next_user_turn_op(&mut new_op_rx) {
            Op::UserTurn { items, .. } => assert_eq!(
                items,
                vec![UserInput::Text {
                    text: "queued follow-up".to_string(),
                    text_elements: Vec::new(),
                }]
            ),
            other => panic!("expected queued follow-up submission, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_only_thread_keeps_restored_queue_visible() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let process_id = ProcessId::new();
        let session_configured = Event {
            id: "session-configured".to_string(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: process_id,
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/tmp/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        };
        app.chat_widget
            .handle_codex_event(session_configured.clone());
        app.chat_widget.handle_codex_event(Event {
            id: "turn-started".to_string(),
            msg: EventMsg::TurnStarted(TurnStartedEvent {
                turn_id: "turn-1".to_string(),
                model_context_window: None,
                collaboration_mode_kind: Default::default(),
            }),
        });
        app.chat_widget.handle_codex_event(Event {
            id: "agent-delta".to_string(),
            msg: EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
                process_id: String::new(),
                turn_id: String::new(),
                item_id: String::new(),
                delta: "streaming".to_string(),
            }),
        });
        app.chat_widget
            .apply_external_edit("queued follow-up".to_string());
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let input_state = app
            .chat_widget
            .capture_process_input_state()
            .expect("expected queued follow-up state");

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_codex_event(session_configured);
        while new_op_rx.try_recv().is_ok() {}

        app.replay_process_snapshot(
            ProcessEventSnapshot {
                session_configured: None,
                events: vec![Event {
                    id: "turn-complete".to_string(),
                    msg: EventMsg::TurnComplete(TurnCompleteEvent {
                        turn_id: "turn-1".to_string(),
                        last_agent_message: None,
                    }),
                }],
                input_state: Some(input_state),
            },
            false,
        );

        assert_eq!(
            app.chat_widget.queued_user_message_texts(),
            vec!["queued follow-up".to_string()]
        );
        assert!(
            new_op_rx.try_recv().is_err(),
            "replay-only threads should not auto-submit restored queue"
        );
    }

    #[tokio::test]
    async fn replay_process_snapshot_keeps_queue_when_running_state_only_comes_from_snapshot() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let process_id = ProcessId::new();
        let session_configured = Event {
            id: "session-configured".to_string(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: process_id,
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/tmp/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        };
        app.chat_widget
            .handle_codex_event(session_configured.clone());
        app.chat_widget.handle_codex_event(Event {
            id: "turn-started".to_string(),
            msg: EventMsg::TurnStarted(TurnStartedEvent {
                turn_id: "turn-1".to_string(),
                model_context_window: None,
                collaboration_mode_kind: Default::default(),
            }),
        });
        app.chat_widget.handle_codex_event(Event {
            id: "agent-delta".to_string(),
            msg: EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
                process_id: String::new(),
                turn_id: String::new(),
                item_id: String::new(),
                delta: "streaming".to_string(),
            }),
        });
        app.chat_widget
            .apply_external_edit("queued follow-up".to_string());
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let input_state = app
            .chat_widget
            .capture_process_input_state()
            .expect("expected queued follow-up state");

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_codex_event(session_configured);
        while new_op_rx.try_recv().is_ok() {}

        app.replay_process_snapshot(
            ProcessEventSnapshot {
                session_configured: None,
                events: vec![],
                input_state: Some(input_state),
            },
            true,
        );

        assert_eq!(
            app.chat_widget.queued_user_message_texts(),
            vec!["queued follow-up".to_string()]
        );
        assert!(
            new_op_rx.try_recv().is_err(),
            "restored queue should stay queued when replay did not prove the turn finished"
        );
    }

    #[tokio::test]
    async fn replay_process_snapshot_does_not_submit_queue_before_replay_catches_up() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let process_id = ProcessId::new();
        let session_configured = Event {
            id: "session-configured".to_string(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: process_id,
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/tmp/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        };
        app.chat_widget
            .handle_codex_event(session_configured.clone());
        app.chat_widget.handle_codex_event(Event {
            id: "turn-started".to_string(),
            msg: EventMsg::TurnStarted(TurnStartedEvent {
                turn_id: "turn-1".to_string(),
                model_context_window: None,
                collaboration_mode_kind: Default::default(),
            }),
        });
        app.chat_widget.handle_codex_event(Event {
            id: "agent-delta".to_string(),
            msg: EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
                process_id: String::new(),
                turn_id: String::new(),
                item_id: String::new(),
                delta: "streaming".to_string(),
            }),
        });
        app.chat_widget
            .apply_external_edit("queued follow-up".to_string());
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let input_state = app
            .chat_widget
            .capture_process_input_state()
            .expect("expected queued follow-up state");

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_codex_event(session_configured);
        while new_op_rx.try_recv().is_ok() {}

        app.replay_process_snapshot(
            ProcessEventSnapshot {
                session_configured: None,
                events: vec![
                    Event {
                        id: "older-turn-complete".to_string(),
                        msg: EventMsg::TurnComplete(TurnCompleteEvent {
                            turn_id: "turn-0".to_string(),
                            last_agent_message: None,
                        }),
                    },
                    Event {
                        id: "latest-turn-started".to_string(),
                        msg: EventMsg::TurnStarted(TurnStartedEvent {
                            turn_id: "turn-1".to_string(),
                            model_context_window: None,
                            collaboration_mode_kind: Default::default(),
                        }),
                    },
                ],
                input_state: Some(input_state),
            },
            true,
        );

        assert!(
            new_op_rx.try_recv().is_err(),
            "queued follow-up should stay queued until the latest turn completes"
        );
        assert_eq!(
            app.chat_widget.queued_user_message_texts(),
            vec!["queued follow-up".to_string()]
        );

        app.chat_widget.handle_codex_event(Event {
            id: "latest-turn-complete".to_string(),
            msg: EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: "turn-1".to_string(),
                last_agent_message: None,
            }),
        });

        match next_user_turn_op(&mut new_op_rx) {
            Op::UserTurn { items, .. } => assert_eq!(
                items,
                vec![UserInput::Text {
                    text: "queued follow-up".to_string(),
                    text_elements: Vec::new(),
                }]
            ),
            other => panic!("expected queued follow-up submission, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_process_snapshot_restores_pending_pastes_for_submit() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let process_id = ProcessId::new();
        app.process_event_channels.insert(
            process_id,
            ProcessEventChannel::new_with_session_configured(
                PROCESS_EVENT_CHANNEL_CAPACITY,
                Event {
                    id: "session-configured".to_string(),
                    msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                        session_id: process_id,
                        forked_from_id: None,
                        process_name: None,
                        model: "gpt-test".to_string(),
                        model_provider_id: "test-provider".to_string(),
                        service_tier: None,
                        approval_policy: ApprovalPolicy::Headless,
                        approvals_reviewer: ApprovalsReviewer::User,
                        sandbox_policy: SandboxPolicy::new_read_only_policy(),
                        cwd: PathBuf::from("/tmp/project"),
                        reasoning_effort: None,
                        history_log_id: 0,
                        history_entry_count: 0,
                        initial_messages: None,
                        network_proxy: None,
                    }),
                },
            ),
        );
        app.activate_process_channel(process_id).await;

        let large = "x".repeat(1005);
        app.chat_widget.handle_paste(large.clone());
        let expected_input_state = app
            .chat_widget
            .capture_process_input_state()
            .expect("expected thread input state");

        app.store_active_process_receiver().await;

        let snapshot = {
            let channel = app
                .process_event_channels
                .get(&process_id)
                .expect("thread channel should exist");
            let store = channel.store.lock().await;
            assert_eq!(store.input_state, Some(expected_input_state));
            store.snapshot()
        };

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.replay_process_snapshot(snapshot, true);

        assert_eq!(app.chat_widget.composer_text_with_pending(), large);

        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match next_user_turn_op(&mut new_op_rx) {
            Op::UserTurn { items, .. } => assert_eq!(
                items,
                vec![UserInput::Text {
                    text: large,
                    text_elements: Vec::new(),
                }]
            ),
            other => panic!("expected restored paste submission, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_process_snapshot_restores_collaboration_mode_for_draft_submit() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let process_id = ProcessId::new();
        let session_configured = Event {
            id: "session-configured".to_string(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: process_id,
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/tmp/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        };
        app.chat_widget
            .handle_codex_event(session_configured.clone());
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::High));
        app.chat_widget
            .set_collaboration_mask(CollaborationModeMask {
                name: "Plan".to_string(),
                mode: Some(ModeKind::Plan),
                model: Some("gpt-restored".to_string()),
                reasoning_effort: Some(Some(ReasoningEffortConfig::High)),
                minion_instructions: None,
            });
        app.chat_widget
            .apply_external_edit("draft prompt".to_string());
        let input_state = app
            .chat_widget
            .capture_process_input_state()
            .expect("expected draft input state");

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_codex_event(session_configured);
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::Low));
        app.chat_widget
            .set_collaboration_mask(CollaborationModeMask {
                name: "Default".to_string(),
                mode: Some(ModeKind::Default),
                model: Some("gpt-replacement".to_string()),
                reasoning_effort: Some(Some(ReasoningEffortConfig::Low)),
                minion_instructions: None,
            });
        while new_op_rx.try_recv().is_ok() {}

        app.replay_process_snapshot(
            ProcessEventSnapshot {
                session_configured: None,
                events: vec![],
                input_state: Some(input_state),
            },
            true,
        );
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match next_user_turn_op(&mut new_op_rx) {
            Op::UserTurn {
                items,
                model,
                effort,
                collaboration_mode,
                ..
            } => {
                assert_eq!(
                    items,
                    vec![UserInput::Text {
                        text: "draft prompt".to_string(),
                        text_elements: Vec::new(),
                    }]
                );
                assert_eq!(model, "gpt-restored".to_string());
                assert_eq!(effort, Some(ReasoningEffortConfig::High));
                assert_eq!(
                    collaboration_mode,
                    Some(CollaborationMode {
                        mode: ModeKind::Plan,
                        settings: Settings {
                            model: "gpt-restored".to_string(),
                            reasoning_effort: Some(ReasoningEffortConfig::High),
                            minion_instructions: None,
                        },
                    })
                );
            }
            other => panic!("expected restored draft submission, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_process_snapshot_restores_collaboration_mode_without_input() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let process_id = ProcessId::new();
        let session_configured = Event {
            id: "session-configured".to_string(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: process_id,
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/tmp/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        };
        app.chat_widget
            .handle_codex_event(session_configured.clone());
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::High));
        app.chat_widget
            .set_collaboration_mask(CollaborationModeMask {
                name: "Plan".to_string(),
                mode: Some(ModeKind::Plan),
                model: Some("gpt-restored".to_string()),
                reasoning_effort: Some(Some(ReasoningEffortConfig::High)),
                minion_instructions: None,
            });
        let input_state = app
            .chat_widget
            .capture_process_input_state()
            .expect("expected collaboration-only input state");

        let (chat_widget, _app_event_tx, _rx, _new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_codex_event(session_configured);
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::Low));
        app.chat_widget
            .set_collaboration_mask(CollaborationModeMask {
                name: "Default".to_string(),
                mode: Some(ModeKind::Default),
                model: Some("gpt-replacement".to_string()),
                reasoning_effort: Some(Some(ReasoningEffortConfig::Low)),
                minion_instructions: None,
            });

        app.replay_process_snapshot(
            ProcessEventSnapshot {
                session_configured: None,
                events: vec![],
                input_state: Some(input_state),
            },
            true,
        );

        assert_eq!(
            app.chat_widget.active_collaboration_mode_kind(),
            ModeKind::Plan
        );
        assert_eq!(app.chat_widget.current_model(), "gpt-restored");
        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::High)
        );
    }

    #[tokio::test]
    async fn replayed_interrupted_turn_restores_queued_input_to_composer() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let process_id = ProcessId::new();
        let session_configured = Event {
            id: "session-configured".to_string(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: process_id,
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/tmp/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        };
        app.chat_widget
            .handle_codex_event(session_configured.clone());
        app.chat_widget.handle_codex_event(Event {
            id: "turn-started".to_string(),
            msg: EventMsg::TurnStarted(TurnStartedEvent {
                turn_id: "turn-1".to_string(),
                model_context_window: None,
                collaboration_mode_kind: Default::default(),
            }),
        });
        app.chat_widget.handle_codex_event(Event {
            id: "agent-delta".to_string(),
            msg: EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
                process_id: String::new(),
                turn_id: String::new(),
                item_id: String::new(),
                delta: "streaming".to_string(),
            }),
        });
        app.chat_widget
            .apply_external_edit("queued follow-up".to_string());
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let input_state = app
            .chat_widget
            .capture_process_input_state()
            .expect("expected queued follow-up state");

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_codex_event(session_configured);
        while new_op_rx.try_recv().is_ok() {}

        app.replay_process_snapshot(
            ProcessEventSnapshot {
                session_configured: None,
                events: vec![Event {
                    id: "turn-aborted".to_string(),
                    msg: EventMsg::TurnAborted(TurnAbortedEvent {
                        turn_id: Some("turn-1".to_string()),
                        reason: TurnAbortReason::ReviewEnded,
                    }),
                }],
                input_state: Some(input_state),
            },
            true,
        );

        assert_eq!(
            app.chat_widget.composer_text_with_pending(),
            "queued follow-up"
        );
        assert!(app.chat_widget.queued_user_message_texts().is_empty());
        assert!(
            new_op_rx.try_recv().is_err(),
            "replayed interrupted turns should restore queued input for editing, not submit it"
        );
    }

    #[tokio::test]
    async fn live_turn_started_refreshes_status_line_with_runtime_context_window() {
        let mut app = make_test_app().await;
        app.chat_widget
            .setup_status_line(vec![crate::bottom_pane::StatusLineItem::ContextWindowSize]);

        assert_eq!(app.chat_widget.status_line_text(), None);

        app.handle_codex_event_now(Event {
            id: "turn-started".to_string(),
            msg: EventMsg::TurnStarted(TurnStartedEvent {
                turn_id: "turn-1".to_string(),
                model_context_window: Some(950_000),
                collaboration_mode_kind: Default::default(),
            }),
        });

        assert_eq!(
            app.chat_widget.status_line_text(),
            Some("950K window".into())
        );
    }

    #[tokio::test]
    async fn open_agent_picker_keeps_missing_threads_for_replay() -> Result<()> {
        let mut app = make_test_app().await;
        let process_id = ProcessId::new();
        app.process_event_channels
            .insert(process_id, ProcessEventChannel::new(1));

        app.open_agent_picker().await;

        assert_eq!(app.process_event_channels.contains_key(&process_id), true);
        assert_eq!(
            app.agent_navigation.get(&process_id),
            Some(&AgentPickerProcessEntry {
                agent_nickname: None,
                agent_role: None,
                is_closed: true,
            })
        );
        assert_eq!(app.agent_navigation.ordered_process_ids(), vec![process_id]);
        Ok(())
    }

    #[tokio::test]
    async fn open_agent_picker_keeps_cached_closed_processes() -> Result<()> {
        let mut app = make_test_app().await;
        let process_id = ProcessId::new();
        app.process_event_channels
            .insert(process_id, ProcessEventChannel::new(1));
        app.agent_navigation.upsert(
            process_id,
            Some("Robie".to_string()),
            Some("scout".to_string()),
            false,
        );

        app.open_agent_picker().await;

        assert_eq!(app.process_event_channels.contains_key(&process_id), true);
        assert_eq!(
            app.agent_navigation.get(&process_id),
            Some(&AgentPickerProcessEntry {
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("scout".to_string()),
                is_closed: true,
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn open_agent_picker_prompts_to_enable_multi_agent_when_disabled() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let _ = app.config.features.disable(Feature::Collab);

        app.open_agent_picker().await;
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::UpdateFeatureFlags { updates }) if updates == vec![(Feature::Collab, true)]
        );
        let cell = match app_event_rx.try_recv() {
            Ok(AppEvent::InsertHistoryCell(cell)) => cell,
            other => panic!("expected InsertHistoryCell event, got {other:?}"),
        };
        let rendered = cell
            .display_lines(120)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Subagents will be enabled in the next session."));
        Ok(())
    }

    #[tokio::test]
    async fn open_agent_picker_allows_existing_agent_threads_when_feature_is_disabled() -> Result<()>
    {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let process_id = ProcessId::new();
        app.process_event_channels
            .insert(process_id, ProcessEventChannel::new(1));

        app.open_agent_picker().await;
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::SelectAgentProcess(selected_process_id)) if selected_process_id == process_id
        );
        Ok(())
    }

    #[tokio::test]
    async fn refresh_pending_process_approvals_only_lists_inactive_processes() {
        let mut app = make_test_app().await;
        let main_process_id =
            ProcessId::from_string("00000000-0000-0000-0000-000000000001").expect("valid thread");
        let agent_process_id =
            ProcessId::from_string("00000000-0000-0000-0000-000000000002").expect("valid thread");

        app.primary_process_id = Some(main_process_id);
        app.active_process_id = Some(main_process_id);
        app.process_event_channels
            .insert(main_process_id, ProcessEventChannel::new(1));

        let agent_channel = ProcessEventChannel::new(1);
        {
            let mut store = agent_channel.store.lock().await;
            store.push_event(Event {
                id: "ev-1".to_string(),
                msg: EventMsg::ExecApprovalRequest(chaos_ipc::protocol::ExecApprovalRequestEvent {
                    call_id: "call-1".to_string(),
                    approval_id: None,
                    turn_id: "turn-1".to_string(),
                    command: vec!["echo".to_string(), "hi".to_string()],
                    cwd: PathBuf::from("/tmp"),
                    reason: None,
                    network_approval_context: None,
                    proposed_execpolicy_amendment: None,
                    proposed_network_policy_amendments: None,
                    additional_permissions: None,
                    skill_metadata: None,
                    available_decisions: None,
                    parsed_cmd: Vec::new(),
                }),
            });
        }
        app.process_event_channels
            .insert(agent_process_id, agent_channel);
        app.agent_navigation.upsert(
            agent_process_id,
            Some("Robie".to_string()),
            Some("scout".to_string()),
            false,
        );

        app.refresh_pending_process_approvals().await;
        assert_eq!(
            app.chat_widget.pending_process_approvals(),
            &["Robie [scout]".to_string()]
        );

        app.active_process_id = Some(agent_process_id);
        app.refresh_pending_process_approvals().await;
        assert!(app.chat_widget.pending_process_approvals().is_empty());
    }

    #[tokio::test]
    async fn inactive_process_approval_bubbles_into_active_view() -> Result<()> {
        let mut app = make_test_app().await;
        let main_process_id =
            ProcessId::from_string("00000000-0000-0000-0000-000000000011").expect("valid thread");
        let agent_process_id =
            ProcessId::from_string("00000000-0000-0000-0000-000000000022").expect("valid thread");

        app.primary_process_id = Some(main_process_id);
        app.active_process_id = Some(main_process_id);
        app.process_event_channels
            .insert(main_process_id, ProcessEventChannel::new(1));
        app.process_event_channels.insert(
            agent_process_id,
            ProcessEventChannel::new_with_session_configured(
                1,
                Event {
                    id: String::new(),
                    msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                        session_id: agent_process_id,
                        forked_from_id: None,
                        process_name: None,
                        model: "gpt-5".to_string(),
                        model_provider_id: "test-provider".to_string(),
                        service_tier: None,
                        approval_policy: ApprovalPolicy::Interactive,
                        approvals_reviewer: ApprovalsReviewer::User,
                        sandbox_policy: SandboxPolicy::new_workspace_write_policy(),
                        cwd: PathBuf::from("/tmp/agent"),
                        reasoning_effort: None,
                        history_log_id: 0,
                        history_entry_count: 0,
                        initial_messages: None,
                        network_proxy: None,
                    }),
                },
            ),
        );
        app.agent_navigation.upsert(
            agent_process_id,
            Some("Robie".to_string()),
            Some("scout".to_string()),
            false,
        );

        app.enqueue_process_event(
            agent_process_id,
            Event {
                id: "ev-approval".to_string(),
                msg: EventMsg::ExecApprovalRequest(chaos_ipc::protocol::ExecApprovalRequestEvent {
                    call_id: "call-approval".to_string(),
                    approval_id: None,
                    turn_id: "turn-approval".to_string(),
                    command: vec!["echo".to_string(), "hi".to_string()],
                    cwd: PathBuf::from("/tmp/agent"),
                    reason: Some("need approval".to_string()),
                    network_approval_context: None,
                    proposed_execpolicy_amendment: None,
                    proposed_network_policy_amendments: None,
                    additional_permissions: None,
                    skill_metadata: None,
                    available_decisions: None,
                    parsed_cmd: Vec::new(),
                }),
            },
        )
        .await?;

        assert_eq!(app.chat_widget.has_active_view(), true);
        assert_eq!(
            app.chat_widget.pending_process_approvals(),
            &["Robie [scout]".to_string()]
        );

        Ok(())
    }

    #[test]
    fn agent_picker_item_name_snapshot() {
        let process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000123")
            .expect("valid thread id");
        let snapshot = [
            format!(
                "{} | {}",
                format_agent_picker_item_name(Some("Robie"), Some("scout"), true),
                process_id
            ),
            format!(
                "{} | {}",
                format_agent_picker_item_name(Some("Robie"), Some("scout"), false),
                process_id
            ),
            format!(
                "{} | {}",
                format_agent_picker_item_name(Some("Robie"), None, false),
                process_id
            ),
            format!(
                "{} | {}",
                format_agent_picker_item_name(None, Some("scout"), false),
                process_id
            ),
            format!(
                "{} | {}",
                format_agent_picker_item_name(None, None, false),
                process_id
            ),
        ]
        .join("\n");
        assert_snapshot!("agent_picker_item_name", snapshot);
    }

    #[tokio::test]
    async fn active_non_primary_shutdown_target_returns_none_for_non_shutdown_event() -> Result<()>
    {
        let mut app = make_test_app().await;
        app.active_process_id = Some(ProcessId::new());
        app.primary_process_id = Some(ProcessId::new());

        assert_eq!(
            app.active_non_primary_shutdown_target(&EventMsg::SkillsUpdateAvailable),
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn active_non_primary_shutdown_target_returns_none_for_primary_thread_shutdown()
    -> Result<()> {
        let mut app = make_test_app().await;
        let process_id = ProcessId::new();
        app.active_process_id = Some(process_id);
        app.primary_process_id = Some(process_id);

        assert_eq!(
            app.active_non_primary_shutdown_target(&EventMsg::ShutdownComplete),
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn active_non_primary_shutdown_target_returns_ids_for_non_primary_shutdown() -> Result<()>
    {
        let mut app = make_test_app().await;
        let active_process_id = ProcessId::new();
        let primary_process_id = ProcessId::new();
        app.active_process_id = Some(active_process_id);
        app.primary_process_id = Some(primary_process_id);

        assert_eq!(
            app.active_non_primary_shutdown_target(&EventMsg::ShutdownComplete),
            Some((active_process_id, primary_process_id))
        );
        Ok(())
    }

    #[tokio::test]
    async fn active_non_primary_shutdown_target_returns_none_when_shutdown_exit_is_pending()
    -> Result<()> {
        let mut app = make_test_app().await;
        let active_process_id = ProcessId::new();
        let primary_process_id = ProcessId::new();
        app.active_process_id = Some(active_process_id);
        app.primary_process_id = Some(primary_process_id);
        app.pending_shutdown_exit_process_id = Some(active_process_id);

        assert_eq!(
            app.active_non_primary_shutdown_target(&EventMsg::ShutdownComplete),
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn active_non_primary_shutdown_target_still_switches_for_other_pending_exit_thread()
    -> Result<()> {
        let mut app = make_test_app().await;
        let active_process_id = ProcessId::new();
        let primary_process_id = ProcessId::new();
        app.active_process_id = Some(active_process_id);
        app.primary_process_id = Some(primary_process_id);
        app.pending_shutdown_exit_process_id = Some(ProcessId::new());

        assert_eq!(
            app.active_non_primary_shutdown_target(&EventMsg::ShutdownComplete),
            Some((active_process_id, primary_process_id))
        );
        Ok(())
    }

    async fn render_clear_ui_header_after_long_transcript_for_snapshot() -> String {
        let mut app = make_test_app().await;
        app.config.cwd = PathBuf::from("/tmp/project");
        app.chat_widget.set_model("gpt-test");
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::High));
        let story_part_one = "In the cliffside town of Bracken Ferry, the lighthouse had been dark for \
            nineteen years, and the children were told it was because the sea no longer wanted a \
            guide. Mara, who repaired clocks for a living, found that hard to believe. Every dawn she \
            heard the gulls circling the empty tower, and every dusk she watched ships hesitate at the \
            mouth of the bay as if listening for a signal that never came. When an old brass key fell \
            out of a cracked parcel in her workshop, tagged only with the words 'for the lamp room,' \
            she decided to climb the hill and see what the town had forgotten.";
        let story_part_two = "Inside the lighthouse she found gears wrapped in oilcloth, logbooks filled \
            with weather notes, and a lens shrouded beneath salt-stiff canvas. The mechanism was not \
            broken, only unfinished. Someone had removed the governor spring and hidden it in a false \
            drawer, along with a letter from the last keeper admitting he had darkened the light on \
            purpose after smugglers threatened his family. Mara spent the night rebuilding the clockwork \
            from spare watch parts, her fingers blackened with soot and grease, while a storm gathered \
            over the water and the harbor bells began to ring.";
        let story_part_three = "At midnight the first squall hit, and the fishing boats returned early, \
            blind in sheets of rain. Mara wound the mechanism, set the teeth by hand, and watched the \
            great lens begin to turn in slow, certain arcs. The beam swept across the bay, caught the \
            whitecaps, and reached the boats just as they were drifting toward the rocks below the \
            eastern cliffs. In the morning the town square was crowded with wet sailors, angry elders, \
            and wide-eyed children, but when the oldest captain placed the keeper's log on the fountain \
            and thanked Mara for relighting the coast, nobody argued. By sunset, Bracken Ferry had a \
            lighthouse again, and Mara had more clocks to mend than ever because everyone wanted \
            something in town to keep better time.";

        let user_cell = |text: &str| -> Arc<dyn HistoryCell> {
            Arc::new(UserHistoryCell {
                message: text.to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>
        };
        let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
            Arc::new(AgentMessageCell::new(
                vec![Line::from(text.to_string())],
                true,
            )) as Arc<dyn HistoryCell>
        };
        let make_header = |is_first| -> Arc<dyn HistoryCell> {
            let event = SessionConfiguredEvent {
                session_id: ProcessId::new(),
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/tmp/project"),
                reasoning_effort: Some(ReasoningEffortConfig::High),
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            };
            Arc::new(new_session_info(
                app.chat_widget.config_ref(),
                app.chat_widget.current_model(),
                event,
                is_first,
            )) as Arc<dyn HistoryCell>
        };

        app.transcript_cells = vec![
            make_header(true),
            Arc::new(crate::history_cell::new_info_event(
                "startup tip that used to replay".to_string(),
                None,
            )) as Arc<dyn HistoryCell>,
            user_cell("Tell me a long story about a town with a dark lighthouse."),
            agent_cell(story_part_one),
            user_cell("Continue the story and reveal why the light went out."),
            agent_cell(story_part_two),
            user_cell("Finish the story with a storm and a resolution."),
            agent_cell(story_part_three),
        ];
        app.has_emitted_history_lines = true;

        let rendered = app
            .clear_ui_header_lines(80)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !rendered.contains("startup tip that used to replay"),
            "clear header should not replay startup notices"
        );
        assert!(
            !rendered.contains("Bracken Ferry"),
            "clear header should not replay prior conversation turns"
        );
        rendered
    }

    #[tokio::test]
    async fn clear_ui_after_long_transcript_snapshots_fresh_header_only() {
        let rendered = render_clear_ui_header_after_long_transcript_for_snapshot().await;
        assert_snapshot!("clear_ui_after_long_transcript_fresh_header_only", rendered);
    }

    #[tokio::test]
    async fn ctrl_l_clear_ui_after_long_transcript_reuses_clear_header_snapshot() {
        let rendered = render_clear_ui_header_after_long_transcript_for_snapshot().await;
        assert_snapshot!("clear_ui_after_long_transcript_fresh_header_only", rendered);
    }

    async fn make_test_app() -> App {
        let (chat_widget, app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
        let config = chat_widget.config_ref().clone();
        let server = Arc::new(
            chaos_kern::test_support::process_table_with_models_provider(
                ChaosAuth::from_api_key("Test API Key"),
                config.model_provider.clone(),
            ),
        );
        let auth_manager = chaos_kern::test_support::auth_manager_from_auth(
            ChaosAuth::from_api_key("Test API Key"),
        );
        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        let model = chaos_kern::test_support::get_model_offline(config.model.as_deref());
        let session_telemetry = test_session_telemetry(&config, model.as_str());
        let tool_list_pane = Rc::new(RefCell::new(ToolListPane::new()));
        let tool_list_close = Rc::new(Cell::new(false));

        App {
            server,
            session_telemetry,
            app_event_tx,
            chat_widget,
            auth_manager,
            config,
            active_profile: None,
            cli_kv_overrides: Vec::new(),
            harness_overrides: ConfigOverrides::default(),
            runtime_approval_policy_override: None,
            runtime_sandbox_policy_override: None,
            tile_manager: TileManager::new(tool_list_pane.clone(), tool_list_close.clone()),
            tool_list_pane,
            tool_list_close,
            file_search,
            log_state_db: None,
            log_state_db_init_error: None,
            log_panel: LogPanelState::default(),
            transcript_cells: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            enhanced_keys_supported: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            suppress_shutdown_complete: false,
            pending_shutdown_exit_process_id: None,

            process_event_channels: HashMap::new(),
            process_event_listener_tasks: HashMap::new(),
            agent_navigation: AgentNavigationState::default(),
            active_process_id: None,
            active_process_rx: None,
            primary_process_id: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
        }
    }

    #[cfg(feature = "vt100-tests")]
    fn make_test_tui() -> crate::tui::Tui {
        let terminal = crate::custom_terminal::Terminal::with_options(
            crate::test_backend::VT100Backend::new(100, 30),
        )
        .expect("test terminal");
        let mut tui = crate::tui::Tui::new(terminal);
        tui.draw(10, |_frame| {}).expect("prime test tui");
        tui
    }

    async fn make_test_app_with_channels() -> (
        App,
        tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
        tokio::sync::mpsc::UnboundedReceiver<Op>,
    ) {
        let (chat_widget, app_event_tx, rx, op_rx) = make_chatwidget_manual_with_sender().await;
        let config = chat_widget.config_ref().clone();
        let server = Arc::new(
            chaos_kern::test_support::process_table_with_models_provider(
                ChaosAuth::from_api_key("Test API Key"),
                config.model_provider.clone(),
            ),
        );
        let auth_manager = chaos_kern::test_support::auth_manager_from_auth(
            ChaosAuth::from_api_key("Test API Key"),
        );
        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        let model = chaos_kern::test_support::get_model_offline(config.model.as_deref());
        let session_telemetry = test_session_telemetry(&config, model.as_str());
        let tool_list_pane = Rc::new(RefCell::new(ToolListPane::new()));
        let tool_list_close = Rc::new(Cell::new(false));

        (
            App {
                server,
                session_telemetry,
                app_event_tx,
                chat_widget,
                auth_manager,
                config,
                active_profile: None,
                cli_kv_overrides: Vec::new(),
                harness_overrides: ConfigOverrides::default(),
                runtime_approval_policy_override: None,
                runtime_sandbox_policy_override: None,
                tile_manager: TileManager::new(tool_list_pane.clone(), tool_list_close.clone()),
                tool_list_pane,
                tool_list_close,
                file_search,
                log_state_db: None,
                log_state_db_init_error: None,
                log_panel: LogPanelState::default(),
                transcript_cells: Vec::new(),
                overlay: None,
                deferred_history_lines: Vec::new(),
                has_emitted_history_lines: false,
                enhanced_keys_supported: false,
                commit_anim_running: Arc::new(AtomicBool::new(false)),
                status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
                backtrack: BacktrackState::default(),
                backtrack_render_pending: false,
                suppress_shutdown_complete: false,
                pending_shutdown_exit_process_id: None,

                process_event_channels: HashMap::new(),
                process_event_listener_tasks: HashMap::new(),
                agent_navigation: AgentNavigationState::default(),
                active_process_id: None,
                active_process_rx: None,
                primary_process_id: None,
                primary_session_configured: None,
                pending_primary_events: VecDeque::new(),
            },
            rx,
            op_rx,
        )
    }

    fn next_user_turn_op(op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) -> Op {
        let mut seen = Vec::new();
        while let Ok(op) = op_rx.try_recv() {
            if matches!(op, Op::UserTurn { .. }) {
                return op;
            }
            seen.push(format!("{op:?}"));
        }
        panic!("expected UserTurn op, saw: {seen:?}");
    }

    fn test_session_telemetry(config: &Config, model: &str) -> SessionTelemetry {
        let model_info = chaos_kern::test_support::construct_model_info_offline(model, config);
        SessionTelemetry::new(
            ProcessId::new(),
            model,
            model_info.slug.as_str(),
            None,
            None,
            None,
            "test_originator".to_string(),
            false,
            "test".to_string(),
            SessionSource::Cli,
        )
    }

    fn app_enabled_in_effective_config(config: &Config, app_id: &str) -> Option<bool> {
        config
            .config_layer_stack
            .effective_config()
            .as_table()
            .and_then(|table| table.get("apps"))
            .and_then(TomlValue::as_table)
            .and_then(|apps| apps.get(app_id))
            .and_then(TomlValue::as_table)
            .and_then(|app| app.get("enabled"))
            .and_then(TomlValue::as_bool)
    }

    #[tokio::test]
    async fn update_reasoning_effort_updates_collaboration_mode() {
        let mut app = make_test_app().await;
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

        app.on_update_reasoning_effort(Some(ReasoningEffortConfig::High));

        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(
            app.config.model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_loads_latest_apps_state() -> Result<()> {
        let mut app = make_test_app().await;
        let chaos_home = tempdir()?;
        app.config.chaos_home = chaos_home.path().to_path_buf();
        let app_id = "unit_test_refresh_in_memory_config_connector".to_string();

        assert_eq!(app_enabled_in_effective_config(&app.config, &app_id), None);

        ConfigEditsBuilder::new(&app.config.chaos_home)
            .with_edits([
                ConfigEdit::SetPath {
                    segments: vec!["apps".to_string(), app_id.clone(), "enabled".to_string()],
                    value: false.into(),
                },
                ConfigEdit::SetPath {
                    segments: vec![
                        "apps".to_string(),
                        app_id.clone(),
                        "disabled_reason".to_string(),
                    ],
                    value: "user".into(),
                },
            ])
            .apply()
            .await
            .expect("persist app toggle");

        assert_eq!(app_enabled_in_effective_config(&app.config, &app_id), None);

        app.refresh_in_memory_config_from_disk().await?;

        assert_eq!(
            app_enabled_in_effective_config(&app.config, &app_id),
            Some(false)
        );
        Ok(())
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_best_effort_keeps_current_config_on_error()
    -> Result<()> {
        let mut app = make_test_app().await;
        let chaos_home = tempdir()?;
        app.config.chaos_home = chaos_home.path().to_path_buf();
        std::fs::write(chaos_home.path().join("config.toml"), "[broken")?;
        let original_config = app.config.clone();

        app.refresh_in_memory_config_from_disk_best_effort("starting a new thread")
            .await;

        assert_eq!(app.config, original_config);
        Ok(())
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_uses_active_chat_widget_cwd() -> Result<()> {
        let mut app = make_test_app().await;
        let original_cwd = app.config.cwd.clone();
        let next_cwd_tmp = tempdir()?;
        let next_cwd = next_cwd_tmp.path().to_path_buf();

        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: ProcessId::new(),
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: next_cwd.clone(),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        });

        assert_eq!(app.chat_widget.config_ref().cwd, next_cwd);
        assert_eq!(app.config.cwd, original_cwd);

        app.refresh_in_memory_config_from_disk().await?;

        assert_eq!(app.config.cwd, app.chat_widget.config_ref().cwd);
        Ok(())
    }

    #[tokio::test]
    async fn rebuild_config_for_resume_or_fallback_uses_current_config_on_same_cwd_error()
    -> Result<()> {
        let mut app = make_test_app().await;
        let chaos_home = tempdir()?;
        app.config.chaos_home = chaos_home.path().to_path_buf();
        std::fs::write(chaos_home.path().join("config.toml"), "[broken")?;
        let current_config = app.config.clone();
        let current_cwd = current_config.cwd.clone();

        let resume_config = app
            .rebuild_config_for_resume_or_fallback(&current_cwd, current_cwd.clone())
            .await?;

        assert_eq!(resume_config, current_config);
        Ok(())
    }

    #[tokio::test]
    async fn rebuild_config_for_resume_or_fallback_errors_when_cwd_changes() -> Result<()> {
        let mut app = make_test_app().await;
        let chaos_home = tempdir()?;
        app.config.chaos_home = chaos_home.path().to_path_buf();
        std::fs::write(chaos_home.path().join("config.toml"), "[broken")?;
        let current_cwd = app.config.cwd.clone();
        let next_cwd_tmp = tempdir()?;
        let next_cwd = next_cwd_tmp.path().to_path_buf();

        let result = app
            .rebuild_config_for_resume_or_fallback(&current_cwd, next_cwd)
            .await;

        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn sync_tui_theme_selection_updates_chat_widget_config_copy() {
        let mut app = make_test_app().await;

        app.sync_tui_theme_selection("dracula".to_string());

        assert_eq!(app.config.tui_theme.as_deref(), Some("dracula"));
        assert_eq!(
            app.chat_widget.config_ref().tui_theme.as_deref(),
            Some("dracula")
        );
    }

    #[tokio::test]
    async fn backtrack_selection_with_duplicate_history_targets_unique_turn() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        let user_cell = |text: &str,
                         text_elements: Vec<TextElement>,
                         local_image_paths: Vec<PathBuf>,
                         remote_image_urls: Vec<String>|
         -> Arc<dyn HistoryCell> {
            Arc::new(UserHistoryCell {
                message: text.to_string(),
                text_elements,
                local_image_paths,
                remote_image_urls,
            }) as Arc<dyn HistoryCell>
        };
        let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
            Arc::new(AgentMessageCell::new(
                vec![Line::from(text.to_string())],
                true,
            )) as Arc<dyn HistoryCell>
        };

        let make_header = |is_first| {
            let event = SessionConfiguredEvent {
                session_id: ProcessId::new(),
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            };
            Arc::new(new_session_info(
                app.chat_widget.config_ref(),
                app.chat_widget.current_model(),
                event,
                is_first,
            )) as Arc<dyn HistoryCell>
        };

        let placeholder = "[Image #1]";
        let edited_text = format!("follow-up (edited) {placeholder}");
        let edited_range = edited_text.len().saturating_sub(placeholder.len())..edited_text.len();
        let edited_text_elements = vec![TextElement::new(edited_range.into(), None)];
        let edited_local_image_paths = vec![PathBuf::from("/tmp/fake-image.png")];

        // Simulate a transcript with duplicated history (e.g., from prior backtracks)
        // and an edited turn appended after a session header boundary.
        app.transcript_cells = vec![
            make_header(true),
            user_cell("first question", Vec::new(), Vec::new(), Vec::new()),
            agent_cell("answer first"),
            user_cell("follow-up", Vec::new(), Vec::new(), Vec::new()),
            agent_cell("answer follow-up"),
            make_header(false),
            user_cell("first question", Vec::new(), Vec::new(), Vec::new()),
            agent_cell("answer first"),
            user_cell(
                &edited_text,
                edited_text_elements.clone(),
                edited_local_image_paths.clone(),
                vec!["https://example.com/backtrack.png".to_string()],
            ),
            agent_cell("answer edited"),
        ];

        assert_eq!(user_count(&app.transcript_cells), 2);

        let base_id = ProcessId::new();
        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: base_id,
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        });

        app.backtrack.base_id = Some(base_id);
        app.backtrack.primed = true;
        app.backtrack.nth_user_message = user_count(&app.transcript_cells).saturating_sub(1);

        let selection = app
            .confirm_backtrack_from_main()
            .expect("backtrack selection");
        assert_eq!(selection.nth_user_message, 1);
        assert_eq!(selection.prefill, edited_text);
        assert_eq!(selection.text_elements, edited_text_elements);
        assert_eq!(selection.local_image_paths, edited_local_image_paths);
        assert_eq!(
            selection.remote_image_urls,
            vec!["https://example.com/backtrack.png".to_string()]
        );

        app.apply_backtrack_rollback(selection);
        assert_eq!(
            app.chat_widget.remote_image_urls(),
            vec!["https://example.com/backtrack.png".to_string()]
        );

        let mut rollback_turns = None;
        while let Ok(op) = op_rx.try_recv() {
            if let Op::ProcessRollback { num_turns } = op {
                rollback_turns = Some(num_turns);
            }
        }

        assert_eq!(rollback_turns, Some(1));
    }

    #[tokio::test]
    async fn backtrack_remote_image_only_selection_clears_existing_composer_draft() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        app.transcript_cells = vec![Arc::new(UserHistoryCell {
            message: "original".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }) as Arc<dyn HistoryCell>];
        app.chat_widget
            .set_composer_text("stale draft".to_string(), Vec::new(), Vec::new());

        let remote_image_url = "https://example.com/remote-only.png".to_string();
        app.apply_backtrack_rollback(BacktrackSelection {
            nth_user_message: 0,
            prefill: String::new(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: vec![remote_image_url.clone()],
        });

        assert_eq!(app.chat_widget.composer_text_with_pending(), "");
        assert_eq!(app.chat_widget.remote_image_urls(), vec![remote_image_url]);

        let mut rollback_turns = None;
        while let Ok(op) = op_rx.try_recv() {
            if let Op::ProcessRollback { num_turns } = op {
                rollback_turns = Some(num_turns);
            }
        }
        assert_eq!(rollback_turns, Some(1));
    }

    #[tokio::test]
    async fn backtrack_resubmit_preserves_data_image_urls_in_user_turn() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        let process_id = ProcessId::new();
        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: process_id,
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        });

        let data_image_url = "data:image/png;base64,abc123".to_string();
        app.transcript_cells = vec![Arc::new(UserHistoryCell {
            message: "please inspect this".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: vec![data_image_url.clone()],
        }) as Arc<dyn HistoryCell>];

        app.apply_backtrack_rollback(BacktrackSelection {
            nth_user_message: 0,
            prefill: "please inspect this".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: vec![data_image_url.clone()],
        });

        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let mut saw_rollback = false;
        let mut submitted_items: Option<Vec<UserInput>> = None;
        while let Ok(op) = op_rx.try_recv() {
            match op {
                Op::ProcessRollback { .. } => saw_rollback = true,
                Op::UserTurn { items, .. } => submitted_items = Some(items),
                _ => {}
            }
        }

        assert!(saw_rollback);
        let items = submitted_items.expect("expected user turn after backtrack resubmit");
        assert!(items.iter().any(|item| {
            matches!(
                item,
                UserInput::Image { image_url } if image_url == &data_image_url
            )
        }));
    }

    #[tokio::test]
    async fn replayed_initial_messages_apply_rollback_in_queue_order() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;

        let session_id = ProcessId::new();
        app.handle_codex_event_replay(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id,
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: Some(vec![
                    EventMsg::UserMessage(UserMessageEvent {
                        message: "first prompt".to_string(),
                        images: None,
                        local_images: Vec::new(),
                        text_elements: Vec::new(),
                    }),
                    EventMsg::UserMessage(UserMessageEvent {
                        message: "second prompt".to_string(),
                        images: None,
                        local_images: Vec::new(),
                        text_elements: Vec::new(),
                    }),
                    EventMsg::ProcessRolledBack(ProcessRolledBackEvent { num_turns: 1 }),
                    EventMsg::UserMessage(UserMessageEvent {
                        message: "third prompt".to_string(),
                        images: None,
                        local_images: Vec::new(),
                        text_elements: Vec::new(),
                    }),
                ]),
                network_proxy: None,
            }),
        });

        let mut saw_rollback = false;
        while let Ok(event) = app_event_rx.try_recv() {
            match event {
                AppEvent::InsertHistoryCell(cell) => {
                    let cell: Arc<dyn HistoryCell> = cell.into();
                    app.transcript_cells.push(cell);
                }
                AppEvent::ApplyProcessRollback { num_turns } => {
                    saw_rollback = true;
                    crate::app_backtrack::trim_transcript_cells_drop_last_n_user_turns(
                        &mut app.transcript_cells,
                        num_turns,
                    );
                }
                _ => {}
            }
        }

        assert!(saw_rollback);
        let user_messages: Vec<String> = app
            .transcript_cells
            .iter()
            .filter_map(|cell| {
                cell.as_any()
                    .downcast_ref::<UserHistoryCell>()
                    .map(|cell| cell.message.clone())
            })
            .collect();
        assert_eq!(
            user_messages,
            vec!["first prompt".to_string(), "third prompt".to_string()]
        );
    }

    #[tokio::test]
    async fn live_rollback_during_replay_is_applied_in_app_event_order() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;

        let session_id = ProcessId::new();
        app.handle_codex_event_replay(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id,
                forked_from_id: None,
                process_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: Some(vec![
                    EventMsg::UserMessage(UserMessageEvent {
                        message: "first prompt".to_string(),
                        images: None,
                        local_images: Vec::new(),
                        text_elements: Vec::new(),
                    }),
                    EventMsg::UserMessage(UserMessageEvent {
                        message: "second prompt".to_string(),
                        images: None,
                        local_images: Vec::new(),
                        text_elements: Vec::new(),
                    }),
                ]),
                network_proxy: None,
            }),
        });

        // Simulate a live rollback arriving before queued replay inserts are drained.
        app.handle_codex_event_now(Event {
            id: "live-rollback".to_string(),
            msg: EventMsg::ProcessRolledBack(ProcessRolledBackEvent { num_turns: 1 }),
        });

        let mut saw_rollback = false;
        while let Ok(event) = app_event_rx.try_recv() {
            match event {
                AppEvent::InsertHistoryCell(cell) => {
                    let cell: Arc<dyn HistoryCell> = cell.into();
                    app.transcript_cells.push(cell);
                }
                AppEvent::ApplyProcessRollback { num_turns } => {
                    saw_rollback = true;
                    crate::app_backtrack::trim_transcript_cells_drop_last_n_user_turns(
                        &mut app.transcript_cells,
                        num_turns,
                    );
                }
                _ => {}
            }
        }

        assert!(saw_rollback);
        let user_messages: Vec<String> = app
            .transcript_cells
            .iter()
            .filter_map(|cell| {
                cell.as_any()
                    .downcast_ref::<UserHistoryCell>()
                    .map(|cell| cell.message.clone())
            })
            .collect();
        assert_eq!(user_messages, vec!["first prompt".to_string()]);
    }

    #[tokio::test]
    async fn queued_rollback_syncs_overlay_and_clears_deferred_history() {
        let mut app = make_test_app().await;
        app.transcript_cells = vec![
            Arc::new(UserHistoryCell {
                message: "first".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(
                vec![Line::from("after first")],
                false,
            )) as Arc<dyn HistoryCell>,
            Arc::new(UserHistoryCell {
                message: "second".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(
                vec![Line::from("after second")],
                false,
            )) as Arc<dyn HistoryCell>,
        ];
        app.overlay = Some(Overlay::new_transcript(app.transcript_cells.clone()));
        app.deferred_history_lines = vec![Line::from("stale buffered line")];
        app.backtrack.overlay_preview_active = true;
        app.backtrack.nth_user_message = 1;

        let changed = app.apply_non_pending_process_rollback(1);

        assert!(changed);
        assert!(app.backtrack_render_pending);
        assert!(app.deferred_history_lines.is_empty());
        assert_eq!(app.backtrack.nth_user_message, 0);
        let user_messages: Vec<String> = app
            .transcript_cells
            .iter()
            .filter_map(|cell| {
                cell.as_any()
                    .downcast_ref::<UserHistoryCell>()
                    .map(|cell| cell.message.clone())
            })
            .collect();
        assert_eq!(user_messages, vec!["first".to_string()]);
        let overlay_cell_count = match app.overlay.as_ref() {
            Some(Overlay::Transcript(t)) => t.committed_cell_count(),
            _ => panic!("expected transcript overlay"),
        };
        assert_eq!(overlay_cell_count, app.transcript_cells.len());
    }

    #[cfg(feature = "vt100-tests")]
    #[tokio::test]
    async fn page_up_opens_transcript_overlay_from_main_view() {
        let mut app = make_test_app().await;
        let mut tui = make_test_tui();
        app.transcript_cells = vec![
            Arc::new(UserHistoryCell {
                message: "first".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(vec![Line::from("reply")], false)) as Arc<dyn HistoryCell>,
        ];

        app.handle_key_event(
            &mut tui,
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
        )
        .await;

        assert!(matches!(app.overlay, Some(Overlay::Transcript(_))));
    }

    #[cfg(feature = "vt100-tests")]
    #[tokio::test]
    async fn page_up_keeps_log_panel_priority_when_visible() {
        let mut app = make_test_app().await;
        let mut tui = make_test_tui();
        app.log_panel = LogPanelState::default();
        app.log_panel.toggle();

        app.handle_key_event(
            &mut tui,
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
        )
        .await;

        assert!(app.overlay.is_none(), "log panel PageUp should not open transcript overlay");
    }

    #[tokio::test]
    async fn new_session_requests_shutdown_for_previous_conversation() {
        let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        let process_id = ProcessId::new();
        let event = SessionConfiguredEvent {
            session_id: process_id,
            forked_from_id: None,
            process_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: ApprovalsReviewer::User,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        };

        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(event),
        });

        while app_event_rx.try_recv().is_ok() {}
        while op_rx.try_recv().is_ok() {}

        app.shutdown_current_process().await;

        match op_rx.try_recv() {
            Ok(Op::Shutdown) => {}
            Ok(other) => panic!("expected Op::Shutdown, got {other:?}"),
            Err(_) => panic!("expected shutdown op to be sent"),
        }
    }

    #[tokio::test]
    async fn shutdown_first_exit_returns_immediate_exit_when_shutdown_submit_fails() {
        let mut app = make_test_app().await;
        let process_id = ProcessId::new();
        app.active_process_id = Some(process_id);

        let control = app.handle_exit_mode(ExitMode::ShutdownFirst);

        assert_eq!(app.pending_shutdown_exit_process_id, None);
        assert!(matches!(
            control,
            AppRunControl::Exit(ExitReason::UserRequested)
        ));
    }

    #[tokio::test]
    async fn shutdown_first_exit_waits_for_shutdown_when_submit_succeeds() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        let process_id = ProcessId::new();
        app.active_process_id = Some(process_id);

        let control = app.handle_exit_mode(ExitMode::ShutdownFirst);

        assert_eq!(app.pending_shutdown_exit_process_id, Some(process_id));
        assert!(matches!(control, AppRunControl::Continue));
        assert_eq!(op_rx.try_recv(), Ok(Op::Shutdown));
    }

    #[tokio::test]
    async fn clear_only_ui_reset_preserves_chat_session_state() {
        let mut app = make_test_app().await;
        let process_id = ProcessId::new();
        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: process_id,
                forked_from_id: None,
                process_name: Some("keep me".to_string()),
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: ApprovalPolicy::Headless,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                cwd: PathBuf::from("/tmp/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        });
        app.chat_widget
            .apply_external_edit("draft prompt".to_string());
        app.transcript_cells = vec![Arc::new(UserHistoryCell {
            message: "old message".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }) as Arc<dyn HistoryCell>];
        app.overlay = Some(Overlay::new_transcript(app.transcript_cells.clone()));
        app.deferred_history_lines = vec![Line::from("stale buffered line")];
        app.has_emitted_history_lines = true;
        app.backtrack.primed = true;
        app.backtrack.overlay_preview_active = true;
        app.backtrack.nth_user_message = 0;
        app.backtrack_render_pending = true;

        app.reset_app_ui_state_after_clear();

        assert!(app.overlay.is_none());
        assert!(app.transcript_cells.is_empty());
        assert!(app.deferred_history_lines.is_empty());
        assert!(!app.has_emitted_history_lines);
        assert!(!app.backtrack.primed);
        assert!(!app.backtrack.overlay_preview_active);
        assert!(app.backtrack.pending_rollback.is_none());
        assert!(!app.backtrack_render_pending);
        assert_eq!(app.chat_widget.process_id(), Some(process_id));
        assert_eq!(app.chat_widget.composer_text_with_pending(), "draft prompt");
    }

    #[tokio::test]
    async fn session_summary_skip_zero_usage() {
        assert!(session_summary(TokenUsage::default(), None, None).is_none());
    }

    #[tokio::test]
    async fn session_summary_includes_resume_hint() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Default::default()
        };
        let conversation = ProcessId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();

        let summary = session_summary(usage, Some(conversation), None).expect("summary");
        assert_eq!(
            summary.usage_line,
            "Token usage: total=12 input=10 output=2"
        );
        assert_eq!(
            summary.resume_command,
            Some("chaos resume 123e4567-e89b-12d3-a456-426614174000".to_string())
        );
    }

    #[tokio::test]
    async fn session_summary_prefers_name_over_id() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Default::default()
        };
        let conversation = ProcessId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();

        let summary = session_summary(usage, Some(conversation), Some("my-session".to_string()))
            .expect("summary");
        assert_eq!(
            summary.resume_command,
            Some("chaos resume my-session".to_string())
        );
    }
}
