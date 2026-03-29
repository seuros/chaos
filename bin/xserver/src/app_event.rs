//! Application-level events used to coordinate UI actions.
//!
//! `AppEvent` is the internal message bus between UI components and the top-level `App` loop.
//! Widgets emit events to request actions that must be handled at the app layer (like opening
//! pickers, persisting configuration, or shutting down the agent), without needing direct access to
//! `App` internals.
//!
//! Exit is modelled explicitly via `AppEvent::Exit(ExitMode)` so callers can request shutdown-first
//! quits without reaching into the app loop or coupling to shutdown/exit sequencing.

use std::path::PathBuf;

use chaos_ipc::ProcessId;
use chaos_ipc::api::AppInfo;
use chaos_ipc::mcp::RequestId as McpRequestId;
use chaos_ipc::openai_models::ModelPreset;
use chaos_ipc::protocol::ElicitationAction;
use chaos_ipc::protocol::Event;
use chaos_locate::FileMatch;
use chaos_sudoers::ApprovalPreset;

use crate::bottom_pane::ApprovalRequest;
use crate::bottom_pane::StatusLineItem;
use crate::history_cell::HistoryCell;

use chaos_ipc::config_types::CollaborationModeMask;
use chaos_ipc::config_types::Personality;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::protocol::AskForApproval;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_kern::config::types::ApprovalsReviewer;
use chaos_kern::features::Feature;

#[derive(Debug, Clone)]
pub(crate) struct ConnectorsSnapshot {
    pub(crate) connectors: Vec<AppInfo>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum AppEvent {
    CodexEvent(Event),
    /// Open the agent picker for switching active processes.
    OpenAgentPicker,
    /// Switch the active process to the selected agent.
    SelectAgentProcess(ProcessId),

    /// Submit an op to the specified process, regardless of current focus.
    SubmitProcessOp {
        process_id: ProcessId,
        op: chaos_ipc::protocol::Op,
    },

    /// Forward an event from a non-primary process into the app-level process router.
    ProcessEvent {
        process_id: ProcessId,
        event: Event,
    },

    /// Start a new session.
    NewSession,

    /// Clear the terminal UI (screen + scrollback), start a fresh session, and keep the
    /// previous chat resumable.
    ClearUi,

    /// Open the resume picker inside the running TUI session.
    OpenResumePicker,

    /// Fork the current session into a new process.
    ForkCurrentSession,

    /// Request to exit the application.
    ///
    /// Use `ShutdownFirst` for user-initiated quits so core cleanup runs and the
    /// UI exits only after `ShutdownComplete`. `Immediate` is a last-resort
    /// escape hatch that skips shutdown and may drop in-flight work (e.g.,
    /// background tasks, rollout flush, or child process cleanup).
    Exit(ExitMode),

    /// Request to exit the application due to a fatal error.
    FatalExitRequest(String),

    /// Forward an `Op` to the Agent. Using an `AppEvent` for this avoids
    /// bubbling channels through layers of widgets.
    CodexOp(chaos_ipc::protocol::Op),

    /// Kick off an asynchronous file search for the given query (text after
    /// the `@`). Previous searches may be cancelled by the app layer so there
    /// is at most one in-flight search.
    StartFileSearch(String),

    /// Result of a completed asynchronous file search. The `query` echoes the
    /// original search term so the UI can decide whether the results are
    /// still relevant.
    FileSearchResult {
        query: String,
        matches: Vec<FileMatch>,
    },

    /// Result of prefetching connectors.
    #[expect(dead_code)]
    ConnectorsLoaded {
        result: Result<ConnectorsSnapshot, String>,
        is_final: bool,
    },

    /// Result of computing a `/diff` command.
    DiffResult(String),

    /// Open the app link view in the bottom pane.
    OpenAppLink {
        app_id: String,
        title: String,
        description: Option<String>,
        instructions: String,
        url: String,
        is_installed: bool,
        is_enabled: bool,
    },

    /// Open the provided URL in the user's browser.
    OpenUrlInBrowser {
        url: String,
    },

    /// Open a URL-mode MCP elicitation in the user's browser, then resolve the request.
    OpenUrlElicitationInBrowser {
        process_id: ProcessId,
        server_name: String,
        request_id: McpRequestId,
        url: String,
        on_open: ElicitationAction,
        on_error: ElicitationAction,
    },

    /// Refresh app connector state and mention bindings.
    RefreshConnectors {
        force_refetch: bool,
    },

    InsertHistoryCell(Box<dyn HistoryCell>),

    /// Apply rollback semantics to local transcript cells.
    ///
    /// This is emitted when rollback was not initiated by the current
    /// backtrack flow so trimming occurs in AppEvent queue order relative to
    /// inserted history cells.
    ApplyProcessRollback {
        num_turns: u32,
    },

    StartCommitAnimation,
    StopCommitAnimation,
    CommitTick,

    /// Update the current reasoning effort in the running app and widget.
    UpdateReasoningEffort(Option<ReasoningEffort>),

    /// Update the current model slug in the running app and widget.
    UpdateModel(String),

    /// Update the active collaboration mask in the running app and widget.
    UpdateCollaborationMode(CollaborationModeMask),

    /// Update the current personality in the running app and widget.
    UpdatePersonality(Personality),

    /// Persist the selected model and reasoning effort to the appropriate config.
    PersistModelSelection {
        model: String,
        effort: Option<ReasoningEffort>,
    },

    /// Persist the selected personality to the appropriate config.
    PersistPersonalitySelection {
        personality: Personality,
    },

    /// Open the reasoning selection popup after picking a model.
    OpenReasoningPopup {
        model: ModelPreset,
    },

    /// Open the Plan-mode reasoning scope prompt for the selected model/effort.
    OpenPlanReasoningScopePrompt {
        model: String,
        effort: Option<ReasoningEffort>,
    },

    /// Open the full model picker (non-auto models).
    OpenAllModelsPopup {
        models: Vec<ModelPreset>,
    },

    /// Open the confirmation prompt before enabling full access mode.
    OpenFullAccessConfirmation {
        preset: ApprovalPreset,
        return_to_permissions: bool,
    },

    /// Update the current approval policy in the running app and widget.
    UpdateAskForApprovalPolicy(AskForApproval),

    /// Update the current sandbox policy in the running app and widget.
    UpdateSandboxPolicy(SandboxPolicy),

    /// Update the current approvals reviewer in the running app and widget.
    UpdateApprovalsReviewer(ApprovalsReviewer),

    /// Update feature flags and persist them to the top-level config.
    UpdateFeatureFlags {
        updates: Vec<(Feature, bool)>,
    },

    /// Update whether the full access warning prompt has been acknowledged.
    UpdateFullAccessWarningAcknowledged(bool),

    /// Update whether the rate limit switch prompt has been acknowledged for the session.
    UpdateRateLimitSwitchPromptHidden(bool),

    /// Update the Plan-mode-specific reasoning effort in memory.
    UpdatePlanModeReasoningEffort(Option<ReasoningEffort>),

    /// Persist the acknowledgement flag for the full access warning prompt.
    PersistFullAccessWarningAcknowledged,

    /// Persist the acknowledgement flag for the rate limit switch prompt.
    PersistRateLimitSwitchPromptHidden,

    /// Persist the Plan-mode-specific reasoning effort.
    PersistPlanModeReasoningEffort(Option<ReasoningEffort>),

    /// Persist the acknowledgement flag for the model migration prompt.
    PersistModelMigrationPromptAcknowledged {
        from_model: String,
        to_model: String,
    },

    /// Re-open the approval presets popup.
    OpenApprovalsPopup,

    /// Enable or disable an app by connector ID.
    SetAppEnabled {
        id: String,
        enabled: bool,
    },

    /// Re-open the permissions presets popup.
    OpenPermissionsPopup,

    /// Open the branch picker option from the review popup.
    OpenReviewBranchPicker(PathBuf),

    /// Open the commit picker option from the review popup.
    OpenReviewCommitPicker(PathBuf),

    /// Open the custom prompt option from the review popup.
    OpenReviewCustomPrompt,

    /// Submit a user message with an explicit collaboration mask.
    SubmitUserMessageWithMode {
        text: String,
        collaboration_mode: CollaborationModeMask,
    },

    /// Open the approval popup.
    FullScreenApprovalRequest(ApprovalRequest),

    /// Launch the external editor after a normal draw has completed.
    LaunchExternalEditor,

    /// Async update of the current git branch for status line rendering.
    StatusLineBranchUpdated {
        cwd: PathBuf,
        branch: Option<String>,
    },
    /// Apply a user-confirmed status-line item ordering/selection.
    StatusLineSetup {
        items: Vec<StatusLineItem>,
    },
    /// Dismiss the status-line setup UI without changing config.
    StatusLineSetupCancelled,

    /// Apply a user-confirmed syntax theme selection.
    SyntaxThemeSelected {
        name: String,
    },
}

/// The exit strategy requested by the UI layer.
///
/// Most user-initiated exits should use `ShutdownFirst` so core cleanup runs and the UI exits only
/// after core acknowledges completion. `Immediate` is an escape hatch for cases where shutdown has
/// already completed (or is being bypassed) and the UI loop should terminate right away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitMode {
    /// Shutdown core and exit after completion.
    ShutdownFirst,
    /// Exit the UI loop immediately without waiting for shutdown.
    ///
    /// This skips `Op::Shutdown`, so any in-flight work may be dropped and
    /// cleanup that normally runs before `ShutdownComplete` can be missed.
    Immediate,
}
